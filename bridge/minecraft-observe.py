#!/usr/bin/env python3
"""Read fixed installation observations; never submit, resume, or replay work."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import math
import os
import pathlib
import re
import signal
import subprocess
import sys
import time
import urllib.request
import urllib.parse
import uuid


_source_spec = importlib.util.spec_from_file_location("minecraft_instruction_contract", pathlib.Path(__file__).with_name("minecraft-instruction.py"))
source_contract = importlib.util.module_from_spec(_source_spec)
_source_spec.loader.exec_module(source_contract)


MAX_INPUT = 65536
MAX_OUTPUT = 131072
MAX_EVENTS = 32
LIMIT_STATE_MEANING = "Current assignment limits only; not the recorded reason for status, proof of completion, or permission for other activity."


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
        if (not isinstance(cursor, dict) or "seen" not in cursor or set(cursor) - {"seen", "presence", "task"}
                or not isinstance(cursor["seen"], list) or len(cursor["seen"]) > 64):
            raise ValueError("invalid_cursor")
        for row in cursor["seen"]:
            if not isinstance(row, dict) or set(row) != {"id", "revision"} or not valid_id(row["id"]) or not re.fullmatch(r"[a-f0-9]{64}", str(row["revision"])):
                raise ValueError("invalid_cursor")
        if "task" in cursor:
            task = cursor["task"]
            if (not isinstance(task, dict) or set(task) != {"id", "semantic_revision"}
                    or not valid_id(task["id"])
                    or not re.fullmatch(r"[a-f0-9]{64}", str(task["semantic_revision"]))):
                raise ValueError("invalid_cursor")
        if "presence" in cursor:
            present = cursor["presence"]
            if (not isinstance(present, dict) or set(present) != {"semantic_revision", "revision", "occurred_at"}
                    or any(not re.fullmatch(r"[a-f0-9]{64}", str(present[key])) for key in ["semantic_revision", "revision"])
                    or not valid_time(present["occurred_at"])):
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


def current_limit_state(status, current):
    """Copy only the domain's current, same-task observation; never derive a stop cause."""
    if (not isinstance(status, dict) or not isinstance(current, dict)
            or not valid_id(current.get("id")) or not valid_time(status.get("observed_at"))):
        return None
    task = status.get("task")
    if not isinstance(task, dict) or task.get("id") != current["id"]:
        return None
    value = task.get("limit_state")
    if (not isinstance(value, dict) or set(value) != {"observed_at", "deadline", "turns", "meaning"}
            or value.get("observed_at") != status["observed_at"] or value.get("meaning") != LIMIT_STATE_MEANING):
        return None
    result = {"observed_at": value["observed_at"], "meaning": LIMIT_STATE_MEANING}
    for name, flag, remaining in [("deadline", "elapsed", "remaining_ms"), ("turns", "reached", "remaining")]:
        axis = value[name]
        if not isinstance(axis, dict) or set(axis) != {"kind", flag, remaining}:
            return None
        kind, limit_reached, amount = axis["kind"], axis[flag], axis[remaining]
        if kind == "finite":
            if (type(limit_reached) is not bool or type(amount) not in (int, float)
                    or (name == "turns" and type(amount) is not int)
                    or not 0 <= amount <= 9007199254740991 or not math.isfinite(amount)
                    or limit_reached != (amount == 0)):
                return None
        elif kind == "no_configured_bound":
            if limit_reached is not False or amount is not None:
                return None
        elif kind == "unknown":
            if limit_reached is not None or amount is not None:
                return None
        else:
            return None
        result[name] = {"kind": kind, flag: limit_reached, remaining: amount}
    return result


def private_source(source, queued):
    return (isinstance(source, dict) and source.get("id") == queued
            and source.get("evidence_origin") == "authenticated_operator_control"
            and source.get("audience", {}).get("kind") == "private")


def conversation_observation(action, connection, observed_at, ledger, queued):
    """Local reservation/settlement evidence is distinct from an answered request."""
    admissions = [row for row in ledger.get("conversation_admissions", []) if isinstance(row, dict)
                  and isinstance(row.get("ids"), list) and queued in row["ids"]]
    turn = ledger.get("last_conversation_turn")
    terminal = (isinstance(turn, dict) and turn.get("conversation_only") is True
                and isinstance(turn.get("request_ids"), list) and queued in turn["request_ids"]
                and type(turn.get("interrupted")) is bool)
    if len(admissions) > 1:
        return action_observation(action, connection, observed_at, scope="conversation",
                                  reason="ambiguous_conversation_admission", fulfillment="not_established")
    if not admissions and not terminal:
        return None
    state = "interrupted" if terminal and turn["interrupted"] else "settled" if terminal else "admitted"
    admitted_at = admissions[0].get("at") if admissions else None
    if type(admitted_at) not in (int, float) or not math.isfinite(admitted_at) or admitted_at < 0:
        admitted_at = None
    return action_observation(action, connection, observed_at, state, scope="conversation",
                              reason="source_bound_local_conversation_state", fulfillment="not_established",
                              receipt={"request_id": queued},
                              conversation={"admitted_at_ms": admitted_at, "admission_scope": "conversation_only",
                                            "admission_layer": "local_body", "host_forwarding_verified": False,
                                            "local_turn_settled": terminal, "interrupted": turn["interrupted"] if terminal else None,
                                            "settlement_evidence": "last_conversation_turn" if terminal else None,
                                            "started_at": turn.get("started_at") if terminal and valid_time(turn.get("started_at")) else None,
                                            "reply_verified": False, "task_acceptance_verified": False},
                              provenance={"kind": "minecraft_local_conversation", "source_id": queued, "authority": False})


def bind_action(action, connection, observed_at, ledger, current):
    original = action["receipt"]
    receipt = original.get("receipt", {})
    queued = receipt.get("request_id") if isinstance(receipt, dict) else None
    if (original.get("request_id") != action["request_id"] or original.get("state") != "queued"
            or original.get("scope") != "private_operator_instruction" or receipt.get("queued") is not True or not valid_id(queued)):
        return action_observation(action, connection, observed_at, reason="no_confirmed_original_queue_receipt")
    tasks = ([current] if isinstance(current, dict) else []) + ledger.get("task_history", [])
    authorizations = [row for row in ledger.get("task_authorizations", []) if isinstance(row, dict)
                      and private_source(row.get("source"), queued)]
    if len(authorizations) > 1:
        return action_observation(action, connection, observed_at, reason="ambiguous_task_authorization")
    authorization = authorizations[0] if authorizations else None
    associated = authorization.get("result", {}).get("task_id") if authorization else None
    matches = [task for task in tasks if isinstance(task, dict) and
               (task.get("request_id") == queued or task.get("authority", {}).get("source", {}).get("id") == queued
                or task.get("id") == associated)]
    if len(matches) > 1:
        return action_observation(action, connection, observed_at, reason="ambiguous_task_binding")
    if not matches:
        conversation = conversation_observation(action, connection, observed_at, ledger, queued)
        if conversation:
            return conversation
        # The original receipt proves historical queueing only, not current queue membership.
        return action_observation(action, connection, observed_at, reason="task_binding_not_retained",
                                  delivery_state="queued", receipt={"request_id": queued})
    task = matches[0]
    source = authorization["source"] if associated == task.get("id") else task.get("authority", {}).get("source", {})
    operator_event = any(isinstance(e, dict) and e.get("id") == queued and e.get("kind") == "chat"
                         and e.get("source") == "human" and e.get("accepted") is True
                         and e.get("provenance") == "operator_control"
                         and e.get("transport", {}).get("kind") == "authenticated_operator_control"
                         for e in ledger.get("events", []))
    authorized_transition = (authorization is not None and associated == task.get("id")
                             and authorization.get("operation") in ["accept", "amend"]
                             and authorization.get("result", {}).get("accepted") is True)
    if ((task.get("request_id") != queued and not authorized_transition) or (source and (source.get("id") != queued
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
                                          "task_transition": authorization.get("operation") if authorized_transition else "original_acceptance",
                                          "completion": "agent_report_only", "authority": False})


def outcome_queue(action):
    """Only a confirmed original queue receipt can choose a lookup ID."""
    original = action["receipt"]
    receipt = original.get("receipt")
    if (original.get("request_id") != action["request_id"] or original.get("state") != "queued"
            or original.get("scope") not in ["private_operator_instruction", "private_installation_input"] or not isinstance(receipt, dict)
            or receipt.get("queued") is not True):
        return None
    queued = receipt.get("request_id")
    try:
        return queued if isinstance(queued, str) and str(uuid.UUID(queued)) == queued else None
    except ValueError:
        return None


def is_scoped(action):
    original = action["receipt"]
    return original.get("scope") == "private_installation_input" or "protocol" in original


def scoped_original(action):
    original, queued = action["receipt"], outcome_queue(action)
    if (queued is None or original.get("scope") != "private_installation_input"
            or original.get("protocol") != "installation_inputs.v1"
            or not source_contract.valid_source(original.get("source"))
            or original["source"]["request_id"] != action["request_id"]
            or original["receipt"].get("source_request_id") != action["request_id"]
            or not isinstance(original.get("caller_id"), str)
            or not re.fullmatch(r"[A-Za-z0-9_.-]{1,80}", original["caller_id"])):
        raise ValueError("unbound_scoped_receipt")
    return queued, original["source"], original["caller_id"]


def read_scoped_outcomes(exchange, actions, client):
    if not actions:
        return "not_requested", {}
    if client is None:
        return "not_configured", {}
    try:
        originals = [scoped_original(action) for action in actions]
        if len({row[0] for row in originals}) != len(originals) or any(row[2] != client["caller_id"] for row in originals):
            raise ValueError("scoped_caller_mismatch")
        capability = exchange("GET", "/source-capabilities", None, client["token"])
        if not source_contract.source_capability(capability):
            raise ValueError("scoped_capability_unavailable")
        expected = {queue: source for queue, source, _ in originals}
        snapshot = exchange("POST", "/source-outcomes", {"version": 1, "requests": [
            {"request_id": queue, "source": source} for queue, source, _ in originals]}, client["token"])
        if (type(snapshot.get("version")) is not int or snapshot["version"] != 1 or not valid_time(snapshot.get("observed_at"))
                or not isinstance(snapshot.get("outcomes"), list) or len(snapshot["outcomes"]) > len(originals)):
            raise ValueError("invalid_scoped_outcomes")
        outcomes = {}
        for row in snapshot["outcomes"]:
            if not isinstance(row, dict) or row.get("request_id") not in expected or row["request_id"] in outcomes:
                raise ValueError("scoped_queue_mismatch")
            # Lack of retention/authorization is deliberately non-enumerating.
            # It carries no caller, response, admission or terminal facts.
            if row == {"request_id": row["request_id"], "delivery": "unknown", "reason": "not_retained_or_not_authorized", "completion": "not_established"}:
                outcomes[row["request_id"]] = row
                continue
            bound = row.get("source")
            if not isinstance(bound, dict) or set(bound) != set(expected[row["request_id"]]) | {"caller"}:
                raise ValueError("scoped_source_missing")
            caller = bound["caller"]
            if (not isinstance(caller, dict) or set(caller) != {"id", "label"} or caller.get("id") != client["caller_id"]
                    or not valid_id(caller.get("label")) or {k: v for k, v in bound.items() if k != "caller"} != expected[row["request_id"]]
                    or row.get("private_game_responses_included") is not False
                    or row.get("response_association") not in ["shared_turn", "single_source", "not_established"]
                    or "source_ids" in row):
                raise ValueError("scoped_original_source_mismatch")
            validate_outcome(row)
            if any(reply["audience"] != "public" for reply in row["responses"]):
                raise ValueError("scoped_private_response")
            # Public game packets keep their original contract. This separate
            # extension is only for the exact credential-bound private source.
            if source_contract.source_replies_capability(capability.get("source_replies")):
                validate_source_replies(row, expected[row["request_id"]])
                row = {**row, "source_replies_capability": capability["source_replies"]}
            elif any(key in row for key in ["source_replies", "source_replies_omitted", "source_replies_capability"]):
                raise ValueError("unnegotiated_source_replies")
            outcomes[row["request_id"]] = row
        return "available", outcomes
    except Exception:
        # A scoped lookup must never broaden to owner /input-outcomes or today's
        # task when the token, capability or complete source does not match.
        return "unavailable", {}


def validate_source_replies(row, source):
    replies = row.get("source_replies")
    if (not isinstance(replies, list) or len(replies) > 32
            or "source_replies_omitted" in row and type(row["source_replies_omitted"]) is not bool):
        raise ValueError("invalid_source_replies")
    ids = set()
    for reply in replies:
        if (not isinstance(reply, dict) or set(reply) != {"id", "at", "text", "audience", "delivery"}
                or not valid_id(reply["id"])
                or reply["id"] in ids or not valid_time(reply["at"])
                or reply["audience"] != source["audience"] or reply["delivery"] != "recorded"
                or not isinstance(reply["text"], str) or not reply["text"].strip()
                or len(reply["text"].encode()) > 12000):
            raise ValueError("invalid_source_reply")
        ids.add(reply["id"])


def validate_outcome(row):
    if (not isinstance(row, dict) or not valid_id(row.get("request_id"))
            or row.get("delivery") not in ["queued", "admitted", "terminal", "interrupted", "unknown"]
            or row.get("completion") != "not_established"
            or "conversation_only" not in row
            or (type(row.get("conversation_only")) is not bool
                and not (row.get("delivery") in ["queued", "unknown"] and row.get("conversation_only") is None))
            or row.get("turn_task_id") is not None and not valid_id(row["turn_task_id"])
            or any(row.get(key) is not None and not valid_time(row[key]) for key in ["admitted_at", "terminal_at"])
            or row.get("blocked_reason") is not None and not text(row["blocked_reason"], 1000)
            or not isinstance(row.get("responses"), list) or len(row["responses"]) > 64
            or not isinstance(row.get("task_authorizations"), list) or len(row["task_authorizations"]) > 16):
        raise ValueError("invalid_input_outcome")
    for flag in ["private_responses_withheld", "responses_omitted"]:
        if flag in row and type(row[flag]) is not bool:
            raise ValueError("invalid_outcome_coverage")
    if "source_ids" in row:
        sources = row["source_ids"]
        if (not isinstance(sources, list) or not 1 <= len(sources) <= 64
                or any(not valid_id(source) for source in sources)
                or len(set(sources)) != len(sources) or row["request_id"] not in sources):
            raise ValueError("invalid_outcome_source_set")
    response_ids = set()
    for response in row["responses"]:
        if (not isinstance(response, dict) or not valid_id(response.get("id"))
                or response["id"] in response_ids or not valid_time(response.get("at"))
                or response.get("audience") not in ["public", "private"]
                or not isinstance(response.get("text"), str) or not response["text"].strip()
                or len(response["text"].encode()) > 4000):
            raise ValueError("invalid_outcome_response")
        response_ids.add(response["id"])
    if "work_resumptions" in row:
        resumptions = row["work_resumptions"]
        if (not isinstance(resumptions, list) or len(resumptions) > 16
                or any(not isinstance(item, dict)
                       or set(item) != {"task_id", "recorded_status", "scheduled", "execution_started"}
                       or not valid_id(item["task_id"]) or not valid_id(item["recorded_status"])
                       or type(item["scheduled"]) is not bool or type(item["execution_started"]) is not bool
                       for item in resumptions)):
            raise ValueError("invalid_work_resumptions")
    if "work_resumptions_omitted" in row and type(row["work_resumptions_omitted"]) is not bool:
        raise ValueError("invalid_work_resumption_coverage")
    for authorization in row["task_authorizations"]:
        if (not isinstance(authorization, dict) or authorization.get("operation") not in ["accept", "amend"]
                or not valid_id(authorization.get("task_id")) or type(authorization.get("authority_revision")) is not int
                or authorization["authority_revision"] < 1):
            raise ValueError("invalid_outcome_authorization")


def read_outcomes(get, actions):
    """Negotiate one fixed read-only route; unsupported hosts keep legacy evidence."""
    queued = [value for action in actions if (value := outcome_queue(action)) is not None]
    if not queued:
        return "not_requested", None
    try:
        capability = get("/status").get("input_outcomes")
    except Exception:
        return "discovery_unavailable", None
    if capability is None:
        return "not_advertised", None
    if (not isinstance(capability, dict) or type(capability.get("version")) is not int or capability["version"] != 1
            or capability.get("method") != "GET" or capability.get("path") != "/input-outcomes"
            or type(capability.get("max_ids")) is not int or not 1 <= capability["max_ids"] <= 16):
        return "unsupported", None
    try:
        if len(set(queued)) != len(queued):
            raise ValueError("ambiguous_original_queue_binding")
        outcomes = {}
        for offset in range(0, len(queued), capability["max_ids"]):
            batch = queued[offset:offset + capability["max_ids"]]
            snapshot = get("/input-outcomes?" + urllib.parse.urlencode({"ids": ",".join(batch)}))
            if (type(snapshot.get("version")) is not int or snapshot["version"] != 1
                    or not valid_time(snapshot.get("observed_at")) or not isinstance(snapshot.get("outcomes"), list)
                    or len(snapshot["outcomes"]) > len(batch)):
                raise ValueError("invalid_input_outcomes")
            for row in snapshot["outcomes"]:
                validate_outcome(row)
                if row["request_id"] not in batch or row["request_id"] in outcomes:
                    raise ValueError("input_outcome_binding_mismatch")
                outcomes[row["request_id"]] = row
        return "available", outcomes
    except Exception:
        # An advertised lookup that failed does not fall back to today's task
        # or a nearby reply and manufacture a source-specific terminal result.
        return "unavailable", {}


def outcome_observation(action, connection, observed_at, outcomes, availability):
    queued = outcome_queue(action)
    row = outcomes.get(queued)
    if row is None:
        return action_observation(action, connection, observed_at, scope="conversation", fulfillment="not_established",
                                  reason="input_outcome_not_retained" if availability == "available" else "input_outcomes_unavailable",
                                  **({"receipt": {"request_id": queued}} if queued else {}))
    if row.get("reason") == "not_retained_or_not_authorized":
        return action_observation(action, connection, observed_at, scope="conversation", fulfillment="not_established",
                                  reason="source_outcome_not_retained_or_not_authorized", receipt={"request_id": queued})
    # Keep scheduling facts ahead of potentially long replies, sharing the
    # existing projection budget. Empty/no-omission evidence normalizes to
    # absence so adding optional support cannot repeat an unchanged reply.
    # Neither case asserts that no resumption happened.
    resumption_evidence, resumption_bytes = {}, 0
    if row.get("work_resumptions"):
        resumptions, resumption_omitted = [], 0
        for item in row["work_resumptions"]:
            size = len(encoded(item))
            if resumption_bytes + size > 4000:
                resumption_omitted += 1
                continue
            resumptions.append(dict(item))
            resumption_bytes += size
        resumption_evidence.update(work_resumptions=resumptions, work_resumptions_omitted=resumption_omitted)
    if row.get("work_resumptions_omitted") is True:
        resumption_evidence["domain_work_resumptions_omitted"] = True
    responses, response_bytes, omitted = [], resumption_bytes, 0
    private_count = 0
    for response in row["responses"]:
        if response["audience"] == "private":
            # The proposed v1 projection has no per-response recipient or
            # co-admitted source set. A broad control token is not call consent.
            private_count += 1
            continue
        projected = {key: response[key] for key in ["id", "at", "text", "audience"]}
        size = len(encoded(projected))
        if len(responses) >= 8 or response_bytes + size > 8000:
            omitted += 1
            continue
        responses.append(projected)
        response_bytes += size
    state = {"queued": "unknown", "admitted": "admitted", "terminal": "settled",
             "interrupted": "interrupted", "unknown": "unknown"}[row["delivery"]]
    # Domain omissions are booleans, not counts. Keep them separate from the
    # records this observer itself withheld and never invent a retained total.
    domain_evidence = {"domain_" + flag: row[flag] for flag in ["private_responses_withheld", "responses_omitted"] if flag in row}
    domain_evidence.update(resumption_evidence)
    if "source_ids" in row:
        domain_evidence["source_ids"] = row["source_ids"]
    if is_scoped(action):
        domain_evidence["domain_response_association"] = row["response_association"]
        domain_evidence["installation_source"] = row["source"]
        domain_evidence["private_game_responses_included"] = False
        if source_contract.source_replies_capability(row.get("source_replies_capability")):
            # Share the existing reply projection budget and retain whole
            # records only; never drop the end of a reply's qualification.
            private_replies, private_omitted = [], 0
            for reply in row["source_replies"]:
                size = len(encoded(reply))
                if len(private_replies) >= 8 or response_bytes + size > 8000:
                    private_omitted += 1
                    continue
                private_replies.append(reply)
                response_bytes += size
            domain_evidence.update(source_replies=private_replies, source_replies_omitted=private_omitted,
                                   source_replies_capability=row["source_replies_capability"])
            if "source_replies_omitted" in row:
                domain_evidence["domain_source_replies_omitted"] = row["source_replies_omitted"]
    if row.get("reason") in ["not_retained", "terminal_not_retained"]:
        domain_evidence["outcome_reason"] = row["reason"]
    return action_observation(action, connection, observed_at, state, scope="conversation", fulfillment="not_established",
                              reason="source_bound_input_outcome", receipt={"request_id": queued}, delivery_state=row["delivery"],
                              conversation={"admitted_at": row.get("admitted_at"), "terminal_at": row.get("terminal_at"),
                                            "admission_layer": "minecraft_input_outcome", "conversation_only": row["conversation_only"],
                                            "turn_task_id": row.get("turn_task_id"), "blocked_reason": row.get("blocked_reason"),
                                            "host_forwarding_verified": False, "local_turn_settled": row["delivery"] == "terminal",
                                            "interrupted": row["delivery"] == "interrupted", "reply_verified": False,
                                            "task_acceptance_verified": False},
                              responses=responses, responses_omitted=omitted, private_responses_withheld=private_count,
                              response_association="same_turn_source_set_not_individual_fulfillment",
                              task_authorizations=[{key: item[key] for key in ["operation", "task_id", "authority_revision"]}
                                                   for item in row["task_authorizations"]],
                              provenance={"kind": "minecraft_input_outcome", "source_id": queued, "version": 1,
                                          "completion": "not_established", "authority": False}, **domain_evidence)


def read_presence(port, token, username, timeout, node_executable):
    helper = pathlib.Path(__file__).resolve().with_name("minecraft-presence.mjs")
    result = subprocess.run([str(node_executable), str(helper)], input=encoded({"port": port, "token": token, "username": username,
                             "timeout_ms": min(8000, max(1, int(timeout * 1000)))}),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=timeout, check=True,
                            env={key: os.environ[key] for key in ["HOME", "PATH", "SSL_CERT_FILE", "SSL_CERT_DIR"] if key in os.environ})
    if len(result.stdout) > 524288:
        raise ValueError("presence_limit")
    return json.loads(result.stdout)


def position(value):
    if (not isinstance(value, dict) or any(type(value.get(k)) not in (int, float)
            or not math.isfinite(value[k]) or abs(value[k]) > 30000000 for k in ["x", "y", "z"])):
        return None
    return {k: round(value[k], 2) for k in ["x", "y", "z"]}


def presence_event(status, username, connection, previous=None):
    if not isinstance(status, dict) or status.get("username") != username or not valid_time(status.get("observed_at")):
        raise ValueError("invalid_presence")
    here = position(status.get("position"))
    players = []
    for row in status.get("players", []) if isinstance(status.get("players"), list) else []:
        if not isinstance(row, dict) or not valid_id(row.get("name")) or row["name"] == username:
            continue
        there = position(row.get("position"))
        player = {"name": row["name"], "position": there}
        if here and there:
            player["distance_blocks"] = round(math.dist(list(here.values()), list(there.values())), 1)
        players.append(player)
    players.sort(key=lambda row: (row.get("distance_blocks", float("inf")), row["name"]))
    value = {"username": username, "ready": status.get("ready") is True, "busy": status.get("busy") is True,
             "turn_active": status.get("turn_active") is True, "position": here,
             "players": players[:8], "players_omitted": max(0, len(players) - 8),
             "meaning": "Observed position and activity flags; not proof of current work or request fulfillment."}
    event = {"id": "minecraft:presence:" + username, "connection_id": connection, "kind": "presence_observation",
             "actor": username, "audience": audience(connection),
             "provenance": {"kind": "minecraft_public_presence", "authority": False}, "text": encoded(value).decode()}
    semantic_revision = revision(event)
    # A new observation time alone does not create a new announcement. Preserve
    # the exact retained event identity; A -> B -> A still receives a new revision.
    occurred_at = previous["occurred_at"] if previous and previous["semantic_revision"] == semantic_revision else status["observed_at"]
    event["occurred_at"] = occurred_at
    event["revision"] = revision(event)
    return event, {"semantic_revision": semantic_revision, "revision": event["revision"], "occurred_at": occurred_at}


def observe(config_path, value, opener=None, now=None, presence_reader=None):
    deadline = time.monotonic() + 9.5
    observed_at = now or datetime.datetime.now(datetime.timezone.utc).isoformat()
    connection = None
    cursor = None
    actions = []
    try:
        # A malformed bounded cursor is an unavailable read, not permission to
        # reset its history and emit everything again on the next poll.
        if isinstance(value, dict) and len(encoded(value)) <= MAX_INPUT:
            cursor = value.get("cursor")
        cursor, actions = validate_input(value)
        if not config_path.is_absolute():
            raise ValueError("absolute_config_required")
        cfg = read_json(config_path, 32768)
        if (not {"minecraft_home", "connection_id", "expected_username"}.issubset(cfg)
                or set(cfg) - {"minecraft_home", "connection_id", "expected_username", "node_executable", "source_config"}
                or not valid_id(cfg["connection_id"]) or not valid_id(cfg["expected_username"])):
            raise ValueError("invalid_config")
        connection = cfg["connection_id"]
        home = pathlib.Path(cfg["minecraft_home"])
        if not home.is_absolute():
            raise ValueError("absolute_home_required")
        node_executable = pathlib.Path(cfg.get("node_executable", home / "bin" / "node"))
        if not node_executable.is_absolute():
            raise ValueError("absolute_node_executable_required")
        installation = read_json(home / "config.json", 65536)
        port, token = installation["control_port"], installation["token"]
        if (installation.get("username") != cfg["expected_username"] or type(port) is not int or not 0 < port < 65536
                or not isinstance(token, str) or not token or len(token) > 8192 or any(ord(c) < 33 or ord(c) > 126 for c in token)):
            raise ValueError("installation_identity_mismatch")
        transport = opener if opener is not None else urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

        def exchange(method, path, payload, selected_token):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("observation_deadline")
            request = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method=method,
                data=encoded(payload) if payload is not None else None,
                headers={"Authorization": "Bearer " + selected_token, "Content-Type": "application/json"})
            with transport.open(request, timeout=min(5, remaining)) as response:
                raw = response.read(524289)
            if len(raw) > 524288:
                raise ValueError("response_limit")
            result = json.loads(raw)
            if not isinstance(result, dict):
                raise ValueError("invalid_snapshot")
            return result

        def get(path):
            return exchange("GET", path, None, token)

        live = get("/installation-state")
        legacy_actions = [action for action in actions if not is_scoped(action)]
        scoped_actions = [action for action in actions if is_scoped(action)]
        confirmed_queues = [queued for action in actions if (queued := outcome_queue(action)) is not None]
        # A duplicated domain ID cannot establish which original call request
        # owns it. Broad control credentials do not fill that provenance gap.
        unique_queues = {queued for queued in confirmed_queues if confirmed_queues.count(queued) == 1}
        private_queues = {queued for action in legacy_actions if (queued := outcome_queue(action)) in unique_queues}
        private = get("/operator-inputs") if private_queues else {"messages": []}
        outcome_availability, outcomes = read_outcomes(get, legacy_actions)
        try:
            client = source_contract.source_client(cfg.get("source_config"), installation)
        except Exception:
            client = None
        scoped_availability, scoped_outcomes = read_scoped_outcomes(exchange, scoped_actions, client)
        # The public presence projection includes this agent's own speech. The
        # incoming-conversation route intentionally excludes it and is not changed.
        public = {"messages": []}
        presence = None
        presence_available = False
        presence_token = installation.get("presence_token")
        if isinstance(presence_token, str) and re.fullmatch(r"[\x21-\x7e]{1,8192}", presence_token):
            remaining = deadline - time.monotonic()
            if remaining > 0:
                try:
                    present = (presence_reader or read_presence)(port, presence_token, cfg["expected_username"], min(8, remaining), node_executable)
                    public, presence = present["activity"], present["status"]
                    if public.get("username") != cfg["expected_username"] or presence.get("username") != cfg["expected_username"]:
                        raise ValueError("presence_identity_mismatch")
                    presence_available = True
                except Exception:
                    public, presence = {"messages": []}, None
        ledger = read_json(home / "workshop.json", 8 * 1024 * 1024)
        current = live.get("task")
        if any((ledger.get("task") or {}).get(key) != (current or {}).get(key)
               for key in ["id", "authority_revision", "progress_revision", "status", "turns"]):
            raise ValueError("concurrent_task_change")
        if not isinstance(ledger.get("task_history", []), list) or not isinstance(ledger.get("events", []), list):
            raise ValueError("invalid_ledger")
        events = []
        omitted_events = 0
        private_messages_withheld = 0
        presence_cursor = (cursor or {}).get("presence")
        previous_task_cursor = (cursor or {}).get("task")
        task_cursor = None
        for snapshot, is_private in [(public, False), (private, True)]:
            messages = snapshot.get("messages")
            if not isinstance(messages, list) or len(messages) > (16 if is_private else 20):
                raise ValueError("invalid_messages")
            for message in messages:
                if not isinstance(message, dict) or not valid_id(message.get("id")):
                    raise ValueError("invalid_message")
                if is_private and message["id"] not in private_queues:
                    private_messages_withheld += 1
                    continue
                author, occurred = (message.get("author"), message.get("time")) if is_private else (message.get("actor"), message.get("at"))
                if (not valid_id("minecraft:message:" + message["id"]) or not isinstance(message.get("text"), str)
                        or not message["text"].strip() or len(message["text"].encode()) > 4000
                        or not valid_id(author) or not valid_time(occurred)):
                    omitted_events += 1
                    continue
                event = {"id": "minecraft:message:" + message["id"], "connection_id": connection,
                         "occurred_at": text(occurred, 64), "kind": "message",
                         "actor": text(author, 200), "audience": audience(connection, is_private),
                         "provenance": {"kind": "authenticated_operator_control" if is_private else "minecraft_public_chat",
                                        "source_id": message["id"], "authority": False}, "text": text(message.get("text"))}
                if len(encoded(event)) <= 7800:
                    events.append({**event, "revision": revision(event)})
                else:
                    omitted_events += 1
        if presence is not None:
            event, presence_cursor = presence_event(presence, cfg["expected_username"], connection, presence_cursor)
            if len(event["text"].encode()) <= 4000:
                events.append(event)
            else:
                omitted_events += 1
        task_source = (current or {}).get("authority", {}).get("source", {})
        private_task_withheld = bool(current and task_source.get("audience", {}).get("kind") != "public"
                                    and not any(private_source(task_source, queued) for queued in private_queues))
        if (isinstance(current, dict) and valid_id(current.get("id")) and valid_id("minecraft:task:" + current["id"])
                and not private_task_withheld):
            projection = task_projection(current)
            # The public MCP summary is a separate observation, not saved task
            # history. Only attach it after this call's task audience is checked.
            limits = current_limit_state(presence, current)
            if limits is not None:
                projection["limit_state"] = limits
            source = current.get("authority", {}).get("source", {})
            event = {"id": "minecraft:task:" + current["id"], "connection_id": connection,
                     "occurred_at": projection["progress"]["at"] or projection["started_at"], "observed_at": observed_at, "kind": "task_observation",
                     "actor": cfg["expected_username"], "audience": audience(connection, source.get("audience", {}).get("kind") != "public"),
                     "provenance": {"kind": "minecraft_current_task", "task_id": current["id"], "current_state_snapshot": True, "authority": False, "verified": False},
                     "text": text(json.dumps(projection, ensure_ascii=False))}
            if (len(json.dumps(projection, ensure_ascii=False).encode()) <= 4000 and not projection["omitted_fields"]
                    and len(encoded(event)) <= 7800 and valid_time(event["occurred_at"])):
                # Re-observation and an ordinary countdown are not new ambient
                # facts. Retain a separate semantic cursor: cursorless direct
                # reads still get exact values under a full immutable revision.
                semantic_projection = dict(projection)
                if limits is not None:
                    semantic_projection["limit_state"] = {
                        key: val for key, val in limits.items() if key != "observed_at"}
                    semantic_projection["limit_state"]["deadline"] = {
                        key: val for key, val in limits["deadline"].items() if key != "remaining_ms"}
                semantic = {key: val for key, val in event.items() if key not in ("observed_at", "text")}
                semantic["task"] = semantic_projection
                # A new/restricted assignment is meaningful even if both old
                # and new deadlines are still future. These raw bounds are
                # fingerprint inputs only, never additional public metadata.
                semantic["assignment_bounds"] = {key: current.get(key) for key in ["bounds", "deadline", "max_turns"]}
                task_cursor = {"id": current["id"], "semantic_revision": revision(semantic)}
                if task_cursor != previous_task_cursor:
                    immutable = {key: val for key, val in event.items() if key != "observed_at"}
                    immutable["assignment_bounds"] = semantic["assignment_bounds"]
                    events.append({**event, "revision": revision(immutable)})
            else:
                omitted_events += 1
        seen = {row["id"]: row["revision"] for row in (cursor or {}).get("seen", [])}
        fresh = [event for event in events if seen.get(event["id"]) != event["revision"]]
        # Current selected-world facts must survive a bounded burst of older
        # chat. Unemitted records retain their cursor identity and drain later;
        # this changes presentation order, never source timestamps or authority.
        fresh.sort(key=lambda event: (
            0 if event["kind"] == "task_observation" else 1 if event["kind"] == "presence_observation" else 2,
            -datetime.datetime.fromisoformat(event["occurred_at"].replace("Z", "+00:00")).timestamp(),
            event["id"]))
        selected_events = fresh[:MAX_EVENTS]

        def emitted_cursor(emitted):
            # The cursor acknowledges output, not everything read from the body.
            # Keep deferred presence identity stable without marking it consumed.
            delivered = dict(seen)
            for event in emitted:
                delivered.pop(event["id"], None)
                delivered[event["id"]] = event["revision"]
            delivered_task = task_cursor if any(event["kind"] == "task_observation" for event in emitted) else previous_task_cursor
            return {"seen": [{"id": key, "revision": val} for key, val in list(delivered.items())[-64:]],
                    **({"presence": presence_cursor} if presence_cursor else {}),
                    **({"task": delivered_task} if delivered_task else {})}

        gap = bool(cursor and any(row["id"].startswith("minecraft:message:") for row in cursor["seen"]) and
                   not any(event["id"] in {r["id"] for r in cursor["seen"]} for event in events if event["kind"] == "message"))
        action_updates = []
        for action in actions:
            queued = outcome_queue(action)
            if queued is not None and queued not in unique_queues:
                update = action_observation(action, connection, observed_at, scope="conversation", fulfillment="not_established",
                                            reason="ambiguous_original_queue_binding")
            elif is_scoped(action):
                try:
                    scoped_original(action)
                    update = outcome_observation(action, connection, observed_at, scoped_outcomes, scoped_availability)
                except Exception:
                    update = action_observation(action, connection, observed_at, scope="conversation", fulfillment="not_established",
                                                reason="scoped_original_binding_unavailable")
            elif outcomes is not None and queued is not None:
                update = outcome_observation(action, connection, observed_at, outcomes, outcome_availability)
            else:
                update = bind_action(action, connection, observed_at, ledger, current)
            action_updates.append(update)
        result = {"version": 1, "connection_id": connection, "observed_at": observed_at, "availability": "available",
                  "presence": {"ready": live.get("ready") is True, "busy": live.get("busy") is True, "turn_active": live.get("turn_active") is True},
                  "cursor": emitted_cursor(selected_events),
                  "coverage": {"baseline": cursor is None, "bounded": True, "gap": gap or len(fresh) > MAX_EVENTS or omitted_events > 0 or not presence_available,
                               "deferred_events": max(0, len(fresh) - MAX_EVENTS),
                               "omitted_events": omitted_events, "omission_reason": "event_text_or_identity_exceeded_bound" if omitted_events else None,
                               "public_presence_available": presence_available,
                               "public_messages_omitted": len(public.get("messages", [])) >= 20 or not presence_available,
                               "private_messages_omitted": bool(private.get("older_messages_omitted")),
                               "private_input_scope": "this_call_confirmed_queue_receipts",
                               "private_messages_withheld": private_messages_withheld,
                               "private_task_withheld": private_task_withheld,
                               "input_outcomes": outcome_availability, "installation_input_outcomes": scoped_availability,
                               "task_history": "retained_only", "verified_completion_supported": False},
                  "events": selected_events, "actions": action_updates}
        result["actions"] = [update if len(encoded(update)) <= 16000 else action_observation(
            action, connection, observed_at, reason="source_report_exceeded_bound") for action, update in zip(actions, result["actions"])]
        while len(encoded(result)) > MAX_OUTPUT and result["events"]:
            result["events"].pop()
            result["coverage"]["gap"] = True
            result["coverage"]["output_truncated"] = True
            result["coverage"]["deferred_events"] = len(fresh) - len(result["events"])
            result["cursor"] = emitted_cursor(result["events"])
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
