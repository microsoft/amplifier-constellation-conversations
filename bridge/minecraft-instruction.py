#!/usr/bin/env python3
"""Fixed installation helper for the existing private Minecraft /task API.

Run on the connector's host with --home. The model receives no control token,
endpoint or shell. Its original voice source stays in the Companion ledger;
Minecraft records its existing authenticated Operator control provenance.
This queues a new instruction, never replays game operations or renews a task.
"""
import argparse
import json
import pathlib
import sys
import urllib.error
import urllib.request
import uuid


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def submit(home, value, opener=None):
    rid = value.get("request_id") if isinstance(value, dict) else None
    try:
        uuid.UUID(rid)
    except (ValueError, TypeError, AttributeError):
        return {"request_id": None, "state": "failed", "message": "Invalid authenticated private instruction."}
    text = value.get("instruction", "")
    source = value.get("source", {})
    if (value.get("version") != 1 or not isinstance(text, str)
            or not text.strip() or "\0" in text or len(text.encode()) > 8000
            or not isinstance(source, dict) or not isinstance(source.get("actor"), str)
            or not source["actor"].strip() or len(source["actor"]) > 200
            or source.get("audience") != "private"):
        return {"request_id": rid, "state": "failed", "message": "Invalid authenticated private instruction."}
    try:
        if not home.is_absolute():
            raise ValueError("absolute_home_required")
        cfg = json.loads((home / "config.json").read_text())
        port = cfg["control_port"]
        token = cfg["token"]
        if (type(port) is not int or not 0 < port < 65536 or not isinstance(token, str)
                or not token or len(token) > 8192 or any(ord(c) < 33 or ord(c) > 126 for c in token)):
            raise ValueError("invalid_installation")
        # No caller-selected route, redirects, environment proxies, retries or
        # gameside impersonation. The API's normal admission/limits still apply.
        request = urllib.request.Request(
            f"http://127.0.0.1:{port}/task",
            data=json.dumps({"text": text}, ensure_ascii=False).encode(), method="POST",
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
        )
        transport = opener if opener is not None else urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect())
    except Exception:
        return {"request_id": rid, "state": "failed", "message": "The fixed installation configuration could not be used. No instruction was submitted."}
    try:
        with transport.open(request, timeout=15) as response:
            raw = response.read(65537)
            if len(raw) > 65536:
                raise ValueError("receipt_limit")
            receipt = json.loads(raw)
        queue_id = receipt.get("request_id")
        if (receipt.get("queued") is not True or not isinstance(queue_id, str)
                or not queue_id.strip() or len(queue_id) > 256
                or any(ord(c) < 32 for c in queue_id)):
            raise ValueError("missing_queue_receipt")
        return {"request_id": rid, "state": "queued", "scope": "private_operator_instruction",
                "receipt": {"request_id": receipt["request_id"], "queued": True},
                "message": "Queued for the existing game controller. This does not prove task acceptance, execution or completion. Minecraft records Operator; original caller attribution is retained in Companion."}
    except Exception:
        # A timeout can occur after the endpoint enqueues the instruction.
        return {"request_id": rid, "state": "unknown", "message": "No confirmed queue receipt. Do not submit this instruction again automatically."}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", type=pathlib.Path, required=True)
    args = parser.parse_args()
    value = None
    try:
        raw = sys.stdin.buffer.read(65537)
        if len(raw) > 65536:
            raise ValueError("input_limit")
        value = json.loads(raw)
        result = submit(args.home, value)
    except Exception:
        result = {"request_id": None, "state": "failed", "message": "Invalid authenticated private instruction. No instruction was submitted."}
    print(json.dumps(result))


if __name__ == "__main__":
    main()
