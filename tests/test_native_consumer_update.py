"""Controller failure boundaries; these do not claim native lifetime proof."""
import importlib.util
import http.server
import io
import json
from pathlib import Path
import queue
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import urllib.error
import urllib.request

spec = importlib.util.spec_from_file_location("native_consumer_update", Path(__file__).with_name("native_consumer_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


class ConsumerUpdateControllerTests(unittest.TestCase):
    def test_check_accepts_one_reply_after_ordinary_http_limit_within_ui_allowance(self):
        requests = []
        checked = {"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}

        class DelayedCheck(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                requests.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                # Exercise a real delayed HTTP status, beyond the former 15s
                # fixture limit but within the update page's existing 40s.
                time.sleep(16)
                body = json.dumps(checked).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        with tempfile.TemporaryDirectory() as directory, http.server.HTTPServer(("127.0.0.1", 0), DelayedCheck) as server:
            server.timeout = 20
            home = Path(directory)
            (home / "config.json").write_text(json.dumps({"public_url": f"http://127.0.0.1:{server.server_port}"}))
            install = controller.consumer.ConsumerInstallation.__new__(controller.consumer.ConsumerInstallation)
            install.home, install.csrf, install.result, install.checkpoint = home, None, {}, Mock()
            install.browser = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            thread = threading.Thread(target=server.handle_request)
            thread.start()
            try:
                self.assertEqual(controller.check_once(install, Path("candidate")), checked)
            finally:
                thread.join(timeout=20)
                self.assertFalse(thread.is_alive())
            self.assertEqual(requests, [(controller.API + "/check", {"package_path": "candidate"})])
            self.assertEqual(install.result["check_request"]["response"], "received")
            self.assertGreaterEqual(install.result["check_request"]["elapsed_seconds"], 15)
            self.assertLess(install.result["check_request"]["elapsed_seconds"], 40)

    def test_unknown_check_retains_only_bounded_ledger_evidence_without_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / "native-browser-update").mkdir()
            (home / "config.json").write_text(json.dumps({"id": "installation"}))
            check_id = "f9b9dd53-f007-4d83-b4e8-90d56046e943"
            ledger = home / "native-browser-update/control.json"
            ledger.write_text(json.dumps({"schema": 1, "installation_id": "installation",
                "current_check": check_id, "checks": {check_id: {"plan": "private-package-path"}},
                "current_operation": None, "operations": {}, "private": "secret-nonce"}))
            install = SimpleNamespace(home=home, result={}, checkpoint=Mock(),
                browser_request=Mock(side_effect=TimeoutError("private socket error")))
            with patch.object(controller, "request_status", return_value={"owner": "standalone", "mode": "normal", "actions": {"check": True}}), patch.object(controller, "submit_once") as submit:
                with self.assertRaises(TimeoutError):
                    controller.exercise_api(install, Path("a"), Path("b"), Mock())
                submit.assert_not_called()
            install.browser_request.assert_called_once()
            row = install.result["check_request"]
            self.assertEqual(row["response"], "transport_unknown")
            self.assertEqual(row["mutation_retries"], 0)
            self.assertEqual(row["ledger_after_failure"]["current_check_id"], check_id)
            self.assertTrue(row["ledger_after_failure"]["current_check_record_present"])
            self.assertNotIn("private", json.dumps(row))
            self.assertNotIn("secret-nonce", json.dumps(row))
            ledger.write_text(json.dumps({"schema": 1, "installation_id": "installation",
                "current_check": "private-malformed-id", "current_operation": None}))
            self.assertEqual(controller.check_failure_ledger(home),
                {"status": "unavailable", "code": "browser_ledger_observation_failed"})

    def test_task_engine_without_actual_worker_cannot_prove_survival(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            ledger = home / "native-browser-update/control.json"
            ledger.parent.mkdir()
            ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
                "transaction_id": "transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
            observer = controller.NativeObserver.__new__(controller.NativeObserver)
            observer.install = SimpleNamespace(home=home)
            observer.previous_operation = observer.operation = observer.attempt = observer.transaction = None
            observer.worker_generation = observer.actual_worker_generation = observer.worker_observation = None
            observer.old_identity = {"pid": 1, "birth": "old"}
            observer.old_exit = observer.survived_old_exit = False
            observer.observations = []
            engine = {"status": "observed", "profile_matches": True, "active": True, "transitioning": False,
                      "processes": [{"identity": {"pid": 2, "birth": "engine"}}], "previous_process_alive": False}
            observer.call = Mock(return_value=engine)
            observer.sample()
            self.assertFalse(observer.survived_old_exit)
            self.assertIsNone(observer.actual_worker_generation)
            observer.call.return_value = {**engine, "actual_worker_identity": {"pid": 3, "birth": "worker"},
                                          "actual_worker_alive": True, "actual_worker_manager_binding": "descendant_of_task_engine"}
            observer.sample()
            self.assertTrue(observer.survived_old_exit)
            self.assertIsNotNone(observer.actual_worker_generation)

    def test_legacy_startup_success_does_not_require_json_output(self):
        install = SimpleNamespace(worker=Path("app"), home=Path("owned-home"),
                                  begin_operation=Mock(return_value={}), end_operation=Mock())
        native = SimpleNamespace(command=Mock(return_value=SimpleNamespace(returncode=0, stdout="", stderr="")))
        with patch.object(controller, "startup_module", return_value=native):
            controller.startup_command(install, {"PATH": "fixed-os-tools"}, "install")
        native.command.assert_called_once_with([Path("app"), "--home", Path("owned-home"), "startup", "install"],
            env={"PATH": "fixed-os-tools"}, check=False, timeout=30)
        self.assertEqual(install.end_operation.call_args.args[2:], ("succeeded", 0))

    def test_protected_receipt_keeps_only_hashes_and_rejects_changed_work(self):
        baseline = {"files": {"secrets/local-password.json": "digest"},
                    "tables": {"browser_sessions": [["private-session-row"]],
                               "companion_actions": [["retained-work", "completed"]]},
                    "companion_policy": {"enabled": False}}
        install = SimpleNamespace(baseline=baseline, snapshot=Mock(return_value=baseline))
        result = controller.protected_receipt(install)
        self.assertEqual(result["tables"]["browser_sessions"]["rows"], 1)
        self.assertNotIn("private-session-row", json.dumps(result))
        install.snapshot.return_value = {**baseline, "tables": {"companion_actions": []}}
        with self.assertRaisesRegex(AssertionError, "protected"):
            controller.protected_receipt(install)

    def test_frame_limit_discards_oversized_private_line_before_capture(self):
        events, capture = queue.Queue(), controller.PrivateCapture()
        controller.read_frames(io.BytesIO(b'"private-' + b'x' * 40 + b'"\n'), events, capture, frame_limit=16)
        self.assertEqual(events.get()[0], "error")
        self.assertEqual(capture.value(), b"")

    def test_raw_capture_overflow_drops_private_content_instead_of_partial_truncation(self):
        capture = controller.PrivateCapture()
        capture.append(b"private-token")
        capture.append(b"x" * (1024 * 1024))
        capture.append(b"more-private")
        self.assertTrue(capture.overflow)
        self.assertNotIn(b"private-token", capture.value())
        self.assertNotIn(b"more-private", capture.value())

    def test_request_reconciliation_never_mutates_or_accepts_a_rebound_transaction(self):
        request = "request"
        first = {"request": {"id": request, "state": "recorded", "operation_id": "operation"},
                 "operation": {"id": "operation", "transaction_id": "original", "phase": "running"}}
        rebound = {**first, "operation": {**first["operation"], "transaction_id": "different", "phase": "completed", "outcome": "applied"}}
        install = SimpleNamespace()
        with patch.object(controller, "request_status", side_effect=[first, rebound]) as status, patch.object(controller.time, "sleep"), self.assertRaisesRegex(AssertionError, "original transaction"):
            controller.await_operation(install, request, "applied", Mock())
        self.assertTrue(all(call.args == (install, request) and call.kwargs == {"tolerate_unavailable": True} for call in status.call_args_list))

    def test_apply_transport_failure_is_retained_without_another_mutation(self):
        install = SimpleNamespace(home=Path("private-home"), csrf="private-csrf", result={}, checkpoint=Mock(),
                                  browser=SimpleNamespace(open=Mock(side_effect=urllib.error.URLError("lost response"))))
        with patch.object(controller, "read_json", return_value={"public_url": "http://127.0.0.1:12345"}):
            controller.submit_once(install, "apply", {"request_id": "original", "check_id": "checked"})
        install.browser.open.assert_called_once()
        self.assertEqual(install.result["request_dispatch"], [{"action": "apply", "request_id": "original",
                          "response": "transport_unknown", "mutation_retries": 0}])

    def test_server_error_is_uncertain_but_client_refusal_fails_clearly(self):
        for status in [503, 409]:
            with self.subTest(status=status):
                response = Mock(status=status)
                response.__enter__ = Mock(return_value=response)
                response.__exit__ = Mock(return_value=False)
                install = SimpleNamespace(home=Path("private-home"), csrf="private-csrf", result={}, checkpoint=Mock(),
                                          browser=SimpleNamespace(open=Mock(return_value=response)))
                with patch.object(controller, "read_json", return_value={"public_url": "http://127.0.0.1:12345"}):
                    if status == 503:
                        controller.submit_once(install, "recover", {"request_id": "original"})
                        self.assertEqual(install.result["request_dispatch"][0]["response"], "server_error_unknown")
                    else:
                        with self.assertRaisesRegex(AssertionError, "HTTP 409"):
                            controller.submit_once(install, "recover", {"request_id": "original"})
                        self.assertEqual(install.result["request_dispatch"][0]["response"], "refused")
                install.browser.open.assert_called_once()

    def test_apply_uncertainty_enters_original_uuid_reconciliation_before_any_rollback(self):
        install = SimpleNamespace(result={}, checkpoint=Mock(), browser_request=Mock(return_value={"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}))
        with patch.object(controller, "request_status", return_value={"owner": "standalone", "mode": "normal", "actions": {"check": True}}), patch.object(controller, "submit_once") as submit, patch.object(controller, "await_operation", side_effect=TimeoutError("outcome unknown")) as reconcile:
            with self.assertRaises(TimeoutError):
                controller.exercise_api(install, Path("a"), Path("b"), Mock())
        submit.assert_called_once()
        reconcile.assert_called_once()
        self.assertEqual(submit.call_args.args[1], "apply")
        self.assertEqual(reconcile.call_args.args[1], submit.call_args.args[2]["request_id"])

    def test_marker_only_lifetime_evidence_cannot_pass_a_checkpoint(self):
        baseline = {"files": {}, "tables": {}, "companion_policy": {}}
        install = SimpleNamespace(baseline=baseline, snapshot=lambda: baseline, selected=Mock(),
            browser_request=Mock(return_value={"authenticated": True}), home=Path("private-home"),
            await_running=Mock(return_value={"id": "installation"}))
        observer = Mock()
        observer.settled.return_value = {"marker_present": True, "worker_exit_observed": False}
        snapshot = {"operation": {"id": "operation", "transaction_id": "transaction", "phase": "completed", "outcome": "applied"}}
        with patch.object(controller, "read_json", return_value={"id": "installation"}), self.assertRaisesRegex(AssertionError, "OS lifetime"):
            controller.phase_checkpoint(install, Path("bundle"), "applied", snapshot, observer)


if __name__ == "__main__":
    unittest.main()
