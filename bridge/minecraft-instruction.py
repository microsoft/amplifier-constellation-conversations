#!/usr/bin/env python3
"""Fixed installation helper for the existing private Minecraft /task API.

Run on the connector's host with --home. The model receives no control token,
endpoint or shell. Without an explicit --source-config, original voice source stays in the Companion
ledger and Minecraft records existing Operator provenance. Opt-in scoped ingress
preserves the exact original human source with a dedicated installation credential.
This queues a new instruction, never replays game operations or renews a task.
"""
import argparse
import datetime
import hashlib
import re
import json
import pathlib
import sys
import urllib.error
import urllib.request
import uuid


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


SOURCE_CAPABILITY = {"version": 1, "submission": {"method": "POST", "path": "/task"},
                     "outcomes": {"method": "POST", "path": "/source-outcomes", "max_items": 16},
                     "source_identity": "credential_bound_caller_and_exact_source",
                     "idempotency": "same_caller_source_request_id_and_payload", "private_game_responses": False}


def source_client(path, installation):
    """Optional fixed caller credential. No endpoint or commands come from input."""
    if path is None:
        return None
    path = pathlib.Path(path)
    if not path.is_absolute() or path.stat().st_size > 8192:
        raise ValueError("invalid_source_config")
    cfg = json.loads(path.read_text())
    if (not isinstance(cfg, dict) or set(cfg) != {"version", "caller_id", "token_file"}
            or type(cfg["version"]) is not int or cfg["version"] != 1
            or not isinstance(cfg["caller_id"], str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,80}", cfg["caller_id"])
            or not isinstance(cfg["token_file"], str)):
        raise ValueError("invalid_source_config")
    token_path = pathlib.Path(cfg["token_file"])
    if not token_path.is_absolute() or token_path.stat().st_size > 257:
        raise ValueError("invalid_source_credential")
    token = token_path.read_text().strip()
    if (not re.fullmatch(r"[A-Za-z0-9_-]{32,256}", token)
            or token in [installation.get(key) for key in ["token", "context_token", "presence_token"]]):
        raise ValueError("dedicated_source_credential_required")
    return {"caller_id": cfg["caller_id"], "token": token}


def source_capability(value):
    # The candidate's fixed shape is deliberately opt-in. A changed method,
    # route, private-response claim, or identity contract is not negotiated v1.
    return (isinstance(value, dict) and value == SOURCE_CAPABILITY
            and type(value.get("version")) is int
            and type(value.get("private_game_responses")) is bool
            and type(value.get("outcomes", {}).get("max_items")) is int)


def valid_source(value):
    def field(text, size):
        return isinstance(text, str) and bool(text.strip()) and len(text) <= size and not any(ord(c) < 32 for c in text)
    if (not isinstance(value, dict) or set(value) != {"version", "request_id", "call_id", "intent_id", "revision", "occurred_at", "actor", "audience"}
            or type(value["version"]) is not int or value["version"] != 1):
        return False
    try:
        if str(uuid.UUID(value["request_id"])) != value["request_id"]:
            return False
        if not all(field(value[key], 160) for key in ["call_id", "intent_id", "revision"]):
            return False
        at = value["occurred_at"]
        if not isinstance(at, str) or not re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,3})?Z", at):
            return False
        datetime.datetime.fromisoformat(at.replace("Z", "+00:00"))
        actor, audience = value["actor"], value["audience"]
        return (isinstance(actor, dict) and set(actor) == {"id", "label"} and all(field(v, 160) for v in actor.values())
                and isinstance(audience, dict) and set(audience) == {"kind", "experience", "id"}
                and audience["kind"] == "private" and field(audience["experience"], 80) and field(audience["id"], 160))
    except (ValueError, TypeError, AttributeError, OverflowError):
        return False


def source_envelope(rid, source):
    original = source.get("original_text")
    at = source.get("occurred_at")
    if (not isinstance(original, str) or not original.strip() or "\0" in original or len(original.encode()) > 12000
            or type(at) is not int or at < 0 or source.get("audience") != "private"
            or source.get("time_basis") != "retained_human_transcript"
            or source.get("source_revision") != hashlib.sha256(original.encode()).hexdigest()):
        raise ValueError("original_source_required")
    value = {"version": 1, "request_id": rid, "call_id": source.get("call_id"),
             "intent_id": source.get("intent_id", source.get("delegation_id")),
             "revision": source["source_revision"],
             "occurred_at": datetime.datetime.fromtimestamp(at / 1000, datetime.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z"),
             "actor": {"id": source.get("actor"), "label": source.get("actor")},
             "audience": {"kind": "private", "experience": "companion", "id": source.get("conversation_id")}}
    if not valid_source(value):
        raise ValueError("invalid_original_source")
    return value


def read_response(transport, request, timeout=15):
    with transport.open(request, timeout=timeout) as response:
        raw = response.read(65537)
    if len(raw) > 65536:
        raise ValueError("receipt_limit")
    return json.loads(raw)


def submit(home, value, opener=None, source_config=None):
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
        transport = opener if opener is not None else urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect())
        client = source_client(source_config, cfg)
        wire_source = source_envelope(rid, source) if client else None
        selected_token = client["token"] if client else token
        payload = {"text": source["original_text"], "source": wire_source} if client else {"text": text}
        # A failed discovery precedes submission and never chooses legacy mode.
        if client:
            capability = read_response(transport, urllib.request.Request(
                f"http://127.0.0.1:{port}/source-capabilities", method="GET",
                headers={"Authorization": "Bearer " + selected_token}))
            if not source_capability(capability):
                raise ValueError("source_protocol_not_advertised")
        request = urllib.request.Request(f"http://127.0.0.1:{port}/task", method="POST",
            data=json.dumps(payload, ensure_ascii=False).encode(),
            headers={"Authorization": "Bearer " + selected_token, "Content-Type": "application/json"})
    except Exception:
        return {"request_id": rid, "state": "failed", "message": "The fixed installation configuration could not be used. No instruction was submitted."}
    try:
        receipt = read_response(transport, request)
        queue_id = receipt.get("request_id")
        if (receipt.get("queued") is not True or not isinstance(queue_id, str)
                or not queue_id.strip() or len(queue_id) > 256
                or any(ord(c) < 32 for c in queue_id)):
            raise ValueError("missing_queue_receipt")
        if client:
            if (str(uuid.UUID(queue_id)) != queue_id or receipt.get("source_request_id") != rid
                    or type(receipt.get("duplicate")) is not bool or not isinstance(receipt.get("received_at"), str)):
                raise ValueError("source_receipt_mismatch")
            received = datetime.datetime.fromisoformat(receipt["received_at"].replace("Z", "+00:00"))
            if received.tzinfo is None:
                raise ValueError("source_receipt_time")
            return {"request_id": rid, "state": "queued", "scope": "private_installation_input",
                    "protocol": "installation_inputs.v1", "caller_id": client["caller_id"], "source": wire_source,
                    "receipt": {key: receipt[key] for key in ["request_id", "queued", "source_request_id", "received_at", "duplicate"]},
                    "message": "The unchanged human source was queued under the dedicated installation caller. This does not confirm task acceptance, execution or fulfillment."}
        return {"request_id": rid, "state": "queued", "scope": "private_operator_instruction",
                "receipt": {"request_id": receipt["request_id"], "queued": True},
                "message": "Queued for the existing game controller. This does not prove task acceptance, execution or completion. Minecraft records Operator; original caller attribution is retained in Companion."}
    except urllib.error.HTTPError as error:
        # Retain bounded transport evidence without echoing a response body,
        # headers, URL, or token. A status alone does not establish whether an
        # earlier attempt was admitted. The domain connector owns that receipt.
        return {"request_id": rid, "state": "unknown",
                "diagnostic": "connector_http_error", "http_status": error.code,
                "message": "The game connector returned an HTTP error instead of a queue receipt. The instruction is not confirmed; do not retry automatically."}
    except (TimeoutError, urllib.error.URLError, OSError):
        return {"request_id": rid, "state": "unknown", "diagnostic": "connector_transport_unconfirmed",
                "message": "No confirmed queue receipt. Do not submit this instruction again automatically."}
    except Exception:
        return {"request_id": rid, "state": "unknown", "diagnostic": "connector_receipt_invalid",
                "message": "The game connector response did not contain a valid queue receipt. Do not submit this instruction again automatically."}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", type=pathlib.Path, required=True)
    parser.add_argument("--source-config", type=pathlib.Path)
    args = parser.parse_args()
    value = None
    try:
        raw = sys.stdin.buffer.read(65537)
        if len(raw) > 65536:
            raise ValueError("input_limit")
        value = json.loads(raw)
        result = submit(args.home, value, source_config=args.source_config)
    except Exception:
        result = {"request_id": None, "state": "failed", "message": "Invalid authenticated private instruction. No instruction was submitted."}
    print(json.dumps(result))


if __name__ == "__main__":
    main()
