#!/usr/bin/env python3
"""Read fixed installation observations; never submit, resume, or replay work."""
import argparse
import datetime
import hashlib
import json
import pathlib
import re
import signal
import sys
import time
import urllib.request
import uuid


MAX_INPUT = 65536
MAX_OUTPUT = 131072


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def revision(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def text(value, limit=4000):
    return value if isinstance(value, str) and len(value.encode()) <= limit else ""


def read_json(path, limit):
    with path.open("rb") as stream:
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise ValueError("input_limit")
    return json.loads(raw)


def valid_id(value):
    return isinstance(value, str) and 0 < len(value.encode()) <= 160 and not any(ord(c) < 32 for c in value)


def valid_time(value):
    try:
        return isinstance(value, str) and len(value) <= 64 and datetime.datetime.fromisoformat(value.replace("Z", "+00:00")).tzinfo is not None
    except ValueError:
        return False


def validate_input(value):
    if not isinstance(value, dict) or set(value) - {"version", "cursor", "actions"} or value.get("version") != 1:
        raise ValueError("invalid_input")
    if len(encoded(value)) > MAX_INPUT:
        raise ValueError("input_limit")
    cursor = value.get("cursor")
    if cursor is not None:
        if not isinstance(cursor, dict) or set(cursor) != {"seen"} or not isinstance(cursor["seen"], list) or len(cursor["seen"]) > 64:
            raise ValueError("invalid_cursor")
        for row in cursor["seen"]:
            if not isinstance(row, dict) or set(row) != {"id", "revision"} or not valid_id(row["id"]) or not re.fullmatch(r"[a-f0-9]{64}", str(row["revision"])):
                raise ValueError("invalid_cursor")
    actions = value.get("actions", [])
    if not isinstance(actions, list) or len(actions) > 4:
        raise ValueError("invalid_actions")
    ids = set()
    for action in actions:
        if not isinstance(action, dict) or set(action) != {"request_id", "receipt"} or not isinstance(action["receipt"], dict):
            raise ValueError("invalid_action")
        uuid.UUID(action["request_id"])
        if action["request_id"] in ids:
            raise ValueError("duplicate_action")
        ids.add(action["request_id"])
    return cursor, actions


def audience(connection, private=False):
    return {"kind": "private" if private else "public", "experience": "installation_control" if private else "minecraft", "connection_id": connection}


def action_observation(action, connection, observed_at, state="unknown", **extra):
    value = {"request_id": action["request_id"], "connection_id": connection, "state": state,
             "scope": "task", "verified": False, "audience": audience(connection, True),
             "provenance": {"kind": "minecraft_observation", "authority": False}, **extra}
    return {**value, "revision": revision(value), "observed_at": observed_at}


def task_projection(task):
    progress = task.get("progress") if isinstance(task.get("progress"), list) else []
    latest = progress[-1] if progress and isinstance(progress[-1], dict) else {}
    omitted = [name for name, value, limit in [("goal", task.get("goal"), 2000), ("progress.summary", latest.get("summary"), 4000)]
               if isinstance(value, str) and len(value.encode()) > limit]
    return {"id": task.get("id"), "status": task.get("status"), "goal": text(task.get("goal"), 2000),
            "authority_revision": task.get("authority_revision"), "progress_revision": task.get("progress_revision"),
            "turns": task.get("turns"), "started_at": task.get("started_at"),
            "progress": {"at": latest.get("at"), "summary": text(latest.get("summary"))}, "omitted_fields": omitted}


def bind_action(action, connection, observed_at, ledger, current):
    original = action["receipt"]
    receipt = original.get("receipt", {})
    queued = receipt.get("request_id") if isinstance(receipt, dict) else None
    if (original.get("request_id") != action["request_id"] or original.get("state") != "queued"
            or original.get("scope") != "private_operator_instruction" or receipt.get("queued") is not True or not valid_id(queued)):
        return action_observation(action, connection, observed_at, reason="no_confirmed_original_queue_receipt")
    tasks = ([current] if isinstance(current, dict) else []) + ledger.get("task_history", [])
    matches = [task for task in tasks if isinstance(task, dict) and
               (task.get("request_id") == queued or task.get("authority", {}).get("source", {}).get("id") == queued)]
    if len(matches) > 1:
        return action_observation(action, connection, observed_at, reason="ambiguous_task_binding")
    if not matches:
        # The original receipt proves historical queueing only, not current queue membership.
        return action_observation(action, connection, observed_at, reason="task_binding_not_retained",
                                  delivery_state="queued", receipt={"request_id": queued})
    task = matches[0]
    source = task.get("authority", {}).get("source", {})
    operator_event = any(isinstance(e, dict) and e.get("id") == queued and e.get("kind") == "chat"
                         and e.get("source") == "human" and e.get("accepted") is True
                         and e.get("provenance") == "operator_control"
                         and e.get("transport", {}).get("kind") == "authenticated_operator_control"
                         for e in ledger.get("events", []))
    if (task.get("request_id") != queued or (source and (source.get("id") != queued
            or source.get("evidence_origin") != "authenticated_operator_control" or source.get("audience", {}).get("kind") != "private"))
            or (not source and not operator_event) or not valid_id(task.get("id"))):
        return action_observation(action, connection, observed_at, reason="unverified_original_source_binding")
    reported = task.get("status")
    states = {"running": "running", "waiting": "waiting", "blocked": "blocked", "needs_review": "needs_review",
              "stopped": "stopped", "budget_exhausted": "budget_exhausted", "review": "reported_complete"}
    state = states.get(reported, "unknown")
    if state == "running" and task.get("turns") == 0:
        state = "accepted"
    retained = current is None or task.get("id") != current.get("id")
    return action_observation(action, connection, observed_at, "superseded" if retained else state,
                              reported_state=state, task=task_projection(task),
                              receipt={"request_id": queued, "task_id": task["id"]},
                              provenance={"kind": "minecraft_retained_task" if retained else "minecraft_current_task",
                                          "source_id": queued, "source_revision": source.get("revision"),
                                          "completion": "agent_report_only", "authority": False})


def observe(config_path, value, opener=None, now=None):
    deadline = time.monotonic() + 9.5
    observed_at = now or datetime.datetime.now(datetime.timezone.utc).isoformat()
    connection = None
    cursor = None
    actions = []
    try:
        cursor, actions = validate_input(value)
        if not config_path.is_absolute():
            raise ValueError("absolute_config_required")
        cfg = read_json(config_path, 32768)
        if set(cfg) != {"minecraft_home", "connection_id", "expected_username"} or not valid_id(cfg["connection_id"]) or not valid_id(cfg["expected_username"]):
            raise ValueError("invalid_config")
        connection = cfg["connection_id"]
        home = pathlib.Path(cfg["minecraft_home"])
        if not home.is_absolute():
            raise ValueError("absolute_home_required")
        installation = read_json(home / "config.json", 65536)
        port, token = installation["control_port"], installation["token"]
        if (installation.get("username") != cfg["expected_username"] or type(port) is not int or not 0 < port < 65536
                or not isinstance(token, str) or not token or len(token) > 8192 or any(ord(c) < 33 or ord(c) > 126 for c in token)):
            raise ValueError("installation_identity_mismatch")
        transport = opener if opener is not None else urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

        def get(path):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("observation_deadline")
            request = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method="GET", headers={"Authorization": "Bearer " + token})
            with transport.open(request, timeout=min(5, remaining)) as response:
                raw = response.read(524289)
            if len(raw) > 524288:
                raise ValueError("response_limit")
            result = json.loads(raw)
            if not isinstance(result, dict):
                raise ValueError("invalid_snapshot")
            return result

        live = get("/installation-state")
        private = get("/operator-inputs")
        public = get("/conversation")
        ledger = read_json(home / "workshop.json", 8 * 1024 * 1024)
        current = live.get("task")
        if (ledger.get("task") or {}).get("id") != (current or {}).get("id"):
            raise ValueError("concurrent_task_change")
        if not isinstance(ledger.get("task_history", []), list) or not isinstance(ledger.get("events", []), list):
            raise ValueError("invalid_ledger")
        events = []
        omitted_events = 0
        for snapshot, is_private in [(public, False), (private, True)]:
            messages = snapshot.get("messages")
            if not isinstance(messages, list) or len(messages) > 16:
                raise ValueError("invalid_messages")
            for message in messages:
                if not isinstance(message, dict) or not valid_id(message.get("id")):
                    raise ValueError("invalid_message")
                if (not valid_id("minecraft:message:" + message["id"]) or not isinstance(message.get("text"), str)
                        or not message["text"].strip() or len(message["text"].encode()) > 4000
                        or not valid_id(message.get("author")) or not valid_time(message.get("time"))):
                    omitted_events += 1
                    continue
                event = {"id": "minecraft:message:" + message["id"], "connection_id": connection,
                         "occurred_at": text(message.get("time"), 64), "kind": "message",
                         "actor": text(message.get("author"), 200), "audience": audience(connection, is_private),
                         "provenance": {"kind": "authenticated_operator_control" if is_private else "minecraft_public_chat",
                                        "source_id": message["id"], "authority": False}, "text": text(message.get("text"))}
                if len(encoded(event)) <= 7800:
                    events.append({**event, "revision": revision(event)})
                else:
                    omitted_events += 1
        if isinstance(current, dict) and valid_id(current.get("id")) and valid_id("minecraft:task:" + current["id"]):
            projection = task_projection(current)
            source = current.get("authority", {}).get("source", {})
            event = {"id": "minecraft:task:" + current["id"], "connection_id": connection,
                     "occurred_at": projection["progress"]["at"] or projection["started_at"], "kind": "task_observation",
                     "actor": cfg["expected_username"], "audience": audience(connection, source.get("audience", {}).get("kind") != "public"),
                     "provenance": {"kind": "minecraft_current_task", "task_id": current["id"], "authority": False, "verified": False},
                     "text": text(json.dumps(projection, ensure_ascii=False))}
            if (len(json.dumps(projection, ensure_ascii=False).encode()) <= 4000 and not projection["omitted_fields"]
                    and len(encoded(event)) <= 7800 and valid_time(event["occurred_at"])):
                events.append({**event, "revision": revision(event)})
            else:
                omitted_events += 1
        seen = {row["id"]: row["revision"] for row in (cursor or {}).get("seen", [])}
        fresh = [event for event in events if seen.get(event["id"]) != event["revision"]]
        for event in events:
            seen.pop(event["id"], None)
            seen[event["id"]] = event["revision"]
        gap = bool(cursor and any(row["id"].startswith("minecraft:message:") for row in cursor["seen"]) and
                   not any(event["id"] in {r["id"] for r in cursor["seen"]} for event in events if event["kind"] == "message"))
        result = {"version": 1, "connection_id": connection, "observed_at": observed_at, "availability": "available",
                  "presence": {"ready": live.get("ready") is True, "busy": live.get("busy") is True, "turn_active": live.get("turn_active") is True},
                  "cursor": {"seen": [{"id": key, "revision": val} for key, val in list(seen.items())[-64:]]},
                  "coverage": {"baseline": cursor is None, "bounded": True, "gap": gap or len(fresh) > 32 or omitted_events > 0,
                               "omitted_events": omitted_events, "omission_reason": "event_text_or_identity_exceeded_bound" if omitted_events else None,
                               "public_messages_omitted": bool(public.get("older_messages_omitted")),
                               "private_messages_omitted": bool(private.get("older_messages_omitted")),
                               "task_history": "retained_only", "verified_completion_supported": False},
                  "events": fresh[-32:], "actions": [bind_action(a, connection, observed_at, ledger, current) for a in actions]}
        result["actions"] = [update if len(encoded(update)) <= 16000 else action_observation(
            action, connection, observed_at, reason="source_report_exceeded_bound") for action, update in zip(actions, result["actions"])]
        while len(encoded(result)) > MAX_OUTPUT and result["events"]:
            result["events"].pop(0)
            result["coverage"]["gap"] = True
            result["coverage"]["output_truncated"] = True
        if len(encoded(result)) > MAX_OUTPUT:
            raise ValueError("output_limit")
        return result
    except Exception:
        return {"version": 1, "connection_id": connection, "observed_at": observed_at, "availability": "unavailable",
                "cursor": cursor, "coverage": {"gap": True, "bounded": True, "reason": "observation_unavailable"}, "events": [],
                "actions": [action_observation(a, connection, observed_at, reason="observation_unavailable") for a in actions]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=pathlib.Path, required=True)
    args = parser.parse_args()
    def expired(_signum, _frame):
        raise TimeoutError("observation_deadline")
    # Connector hosts are Unix; bound even a stalled local file/read/socket.
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, 10)
    try:
        raw = sys.stdin.buffer.read(MAX_INPUT + 1)
        if len(raw) > MAX_INPUT:
            raise ValueError("input_limit")
        value = json.loads(raw)
    except Exception:
        value = None
    result = observe(args.config, value)
    signal.setitimer(signal.ITIMER_REAL, 0)
    sys.stdout.buffer.write(encoded(result) + b"\n")


if __name__ == "__main__":
    main()
