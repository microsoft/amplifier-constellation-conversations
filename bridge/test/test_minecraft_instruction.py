"""Disposable fixtures only: never connect to an installed Minecraft controller."""
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import unittest
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch


HELPER = pathlib.Path(__file__).resolve().parents[1] / "minecraft-instruction.py"
SPEC = importlib.util.spec_from_file_location("minecraft_instruction", HELPER)
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)
TOKEN = "fixture-control-token-never-output"


class Opener:
    def __init__(self, receipt=None, error=None):
        self.calls = []
        self.receipt = receipt if receipt is not None else {"queued": True, "request_id": "event-42"}
        self.error = error

    def open(self, request, timeout):
        self.calls.append((request, timeout))
        if self.error:
            raise self.error
        raw = self.receipt if isinstance(self.receipt, bytes) else json.dumps(self.receipt).encode()
        return io.BytesIO(raw)


@contextlib.contextmanager
def local_server(redirect=False):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def handle_request(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            requests.append((self.command, self.path, body))
            if redirect:
                self.send_response(307)
                self.send_header("Location", "/credential-sink")
                self.end_headers()
            else:
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"queued":true,"request_id":"event-local"}')

        do_POST = handle_request
        do_GET = handle_request

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_port, requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


class MinecraftInstructionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="companion-minecraft-")
        self.addCleanup(self.temp.cleanup)
        self.home = pathlib.Path(self.temp.name)
        self.config = {"control_port": 12345, "token": TOKEN}
        self.save_config()
        self.input = {"version": 1, "request_id": str(uuid.uuid4()), "instruction": "Build the human-requested tower.",
                      "source": {"actor": "owner", "audience": "private", "original_text": "Retained original voice source."}}

    def save_config(self):
        (self.home / "config.json").write_text(json.dumps(self.config))

    def test_fixed_loopback_task_request_and_distinct_queue_receipt(self):
        opener = Opener()
        value = {**self.input, "url": "https://attacker.invalid", "token": "caller-token",
                 "instruction": "漢" * 2666 + "aa"}
        result = helper.submit(self.home, value, opener)
        self.assertEqual(len(opener.calls), 1)
        request, timeout = opener.calls[0]
        self.assertEqual(request.full_url, "http://127.0.0.1:12345/task")
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(request.get_header("Authorization"), "Bearer " + TOKEN)
        self.assertEqual(timeout, 15)
        self.assertEqual(json.loads(request.data), {"text": value["instruction"]})
        self.assertLess(len(request.data), 8100)
        self.assertNotIn("Retained original", request.data.decode())
        self.assertEqual(result["request_id"], self.input["request_id"])
        self.assertEqual(result["state"], "queued")
        self.assertEqual(result["receipt"], {"queued": True, "request_id": "event-42"})
        self.assertEqual(result["scope"], "private_operator_instruction")
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_uncertain_errors_and_malformed_receipts_never_retry(self):
        cases = [Opener(error=TimeoutError(TOKEN)), Opener(error=OSError(TOKEN)),
                 Opener(receipt=b"not-json " + TOKEN.encode()), Opener(receipt=b"x" * 65537),
                 Opener(receipt={"queued": False, "request_id": "event-42"}),
                 Opener(receipt={"queued": True}), Opener(receipt={"queued": True, "request_id": ""}),
                 Opener(receipt={"queued": True, "request_id": "x" * 257})]
        for opener in cases:
            with self.subTest(receipt=type(opener.receipt).__name__, error=type(opener.error).__name__):
                result = helper.submit(self.home, self.input, opener)
                self.assertEqual(result["state"], "unknown")
                self.assertEqual(result["request_id"], self.input["request_id"])
                self.assertEqual(len(opener.calls), 1)
                self.assertNotIn(TOKEN, json.dumps(result))

    def test_http_refusal_evidence_is_preserved_without_claiming_admission_or_retry(self):
        for status in [400, 401, 409, 429, 500, 503]:
            error = helper.urllib.error.HTTPError("http://secret.invalid/" + TOKEN, status,
                TOKEN, {"Authorization": TOKEN}, io.BytesIO(TOKEN.encode()))
            opener = Opener(error=error)
            result = helper.submit(self.home, self.input, opener)
            self.assertEqual(result["state"], "unknown")
            self.assertEqual(result["http_status"], status)
            self.assertEqual(result["diagnostic"], "connector_http_error")
            self.assertEqual(len(opener.calls), 1)
            self.assertNotIn(TOKEN, json.dumps(result))

    def test_invalid_authority_and_oversize_utf8_fail_before_dispatch(self):
        opener = Opener()
        for changes in [{"request_id": "bad"}, {"version": 2}, {"instruction": " "},
                        {"instruction": "a\0b"}, {"instruction": "漢" * 2667},
                        {"source": {"actor": "", "audience": "private"}},
                        {"source": {"actor": {"id": "owner"}, "audience": "private"}},
                        {"source": {"actor": "owner", "audience": "public"}}]:
            with self.subTest(changes=changes.keys()):
                self.assertEqual(helper.submit(self.home, {**self.input, **changes}, opener)["state"], "failed")
        self.assertEqual(opener.calls, [])

    def test_invalid_installation_never_dispatches_or_discloses_configuration(self):
        opener = Opener()
        for change in [{"control_port": True}, {"control_port": 65536}, {"control_port": "12345"},
                       {"token": TOKEN + "\nHeader: value"}, {"token": ""}]:
            self.config = {"control_port": 12345, "token": TOKEN, **change}
            self.save_config()
            result = helper.submit(self.home, self.input, opener)
            self.assertEqual(result["state"], "failed")
            self.assertNotIn(TOKEN, json.dumps(result))
        self.assertEqual(helper.submit(pathlib.Path("relative"), self.input, opener)["state"], "failed")
        self.assertEqual(opener.calls, [])

    def test_redirect_is_rejected_without_following_or_retrying(self):
        with local_server(redirect=True) as (port, requests):
            self.config["control_port"] = port
            self.save_config()
            result = helper.submit(self.home, self.input)
        self.assertEqual(result["state"], "unknown")
        self.assertEqual([(method, path) for method, path, _ in requests], [("POST", "/task")])
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_environment_proxy_cannot_receive_loopback_control_request(self):
        with local_server() as (port, requests), local_server() as (proxy_port, proxy_requests):
            self.config["control_port"] = port
            self.save_config()
            proxy = f"http://127.0.0.1:{proxy_port}"
            with patch.dict(os.environ, {"http_proxy": proxy, "HTTP_PROXY": proxy,
                                         "all_proxy": proxy, "ALL_PROXY": proxy}, clear=True):
                result = helper.submit(self.home, self.input)
        self.assertEqual(result["state"], "queued")
        self.assertEqual(len(requests), 1)
        self.assertEqual(proxy_requests, [])

    def test_cli_source_envelope_boundary_and_errors_are_sanitized(self):
        # Missing configuration ensures every invocation is preflight-only.
        (self.home / "config.json").unlink()
        value = {**self.input, "source": {**self.input["source"], "padding": ""}}
        value["source"]["padding"] = "x" * (65536 - len(json.dumps(value).encode()))
        boundary = json.dumps(value)
        self.assertEqual(len(boundary.encode()), 65536)
        for raw, expected_id in [(boundary, self.input["request_id"]), (boundary + " ", None),
                                 ("not-json " + TOKEN, None), ("[]", None)]:
            result = subprocess.run([sys.executable, str(HELPER), "--home", str(self.home)],
                                    input=raw, text=True, capture_output=True, timeout=5, check=True)
            self.assertEqual(result.stderr, "")
            self.assertNotIn(TOKEN, result.stdout)
            parsed = json.loads(result.stdout)
            self.assertEqual(parsed["state"], "failed")
            self.assertEqual(parsed["request_id"], expected_id)


if __name__ == "__main__":
    unittest.main()
