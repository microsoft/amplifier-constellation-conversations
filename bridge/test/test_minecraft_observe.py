"""Only temporary owner files and fake transports; no installed service calls."""
import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
import urllib.parse
import uuid
from unittest.mock import patch

HELPER = pathlib.Path(__file__).resolve().parents[1] / "minecraft-observe.py"
SPEC = importlib.util.spec_from_file_location("minecraft_observe", HELPER)
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)
TOKEN = "fixture-control-secret-never-output"
PRESENCE_TOKEN = "fixture-presence-secret-never-output"
NOW = "2026-11-07T20:00:00+00:00"


class MinecraftObserveTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="companion-observe-")
        self.addCleanup(tmp.cleanup)
        self.home = pathlib.Path(tmp.name)
        self.config = self.home / "observer.json"
        self.config.write_text(json.dumps({"minecraft_home": str(self.home), "connection_id": "minecraft:coach", "expected_username": "Coach"}))
        (self.home / "config.json").write_text(json.dumps({"username": "Coach", "control_port": 12345, "token": TOKEN, "presence_token": PRESENCE_TOKEN}))
        self.request = str(uuid.uuid4())
        self.queued = str(uuid.uuid4())
        self.task = {"id": str(uuid.uuid4()), "request_id": self.queued, "status": "running", "turns": 0,
                     "started_at": NOW, "goal": "Build the tower", "authority_revision": 1, "progress_revision": 0, "progress": [],
                     "authority": {"source": {"id": self.queued, "revision": "original-source-revision",
                                               "evidence_origin": "authenticated_operator_control", "audience": {"kind": "private"}}}}
        self.ledger = {"task": self.task, "task_history": [], "events": []}
        self.snapshots = {"/installation-state": {"ready": True, "task": self.task},
                          "/status": {},
                          "/operator-inputs": {"messages": [{"id": self.queued, "time": NOW, "author": "Operator", "text": "Private instruction"}]}}
        self.public = {"username": "Coach", "messages": [{"id": "public-event", "at": NOW, "actor": "Other player", "text": "Public question"},
                       {"id": "own-event", "at": NOW, "actor": "Coach", "text": "The passage is open; the mechanism is not tested."}]}
        self.presence = {"username": "Coach", "observed_at": NOW, "ready": True, "busy": False, "turn_active": False,
                         "position": {"x": 1, "y": 64, "z": 2}, "players": [{"name": "Brian", "position": {"x": 4, "y": 64, "z": 6}}],
                         "task": self.task, "token": "must-not-project", "inventory": [{"name": "private-metadata"}]}
        self.input = {"version": 1, "cursor": None, "actions": [{"request_id": self.request,
                      "receipt": {"request_id": self.request, "state": "queued", "scope": "private_operator_instruction",
                                  "receipt": {"request_id": self.queued, "queued": True}}}]}
        self.calls = []
        self.presence_calls = []
        self.error = None

    def read_presence(self, port, token, username, timeout, node_executable):
        self.presence_calls.append((port, username))
        self.assertEqual(port, 12345)
        self.assertEqual(token, PRESENCE_TOKEN)
        self.assertEqual(username, "Coach")
        self.assertEqual(node_executable, self.home / "bin" / "node")
        self.assertGreater(timeout, 0)
        self.assertLessEqual(timeout, 8)
        return {"activity": self.public, "status": self.presence}

    def open(self, request, timeout):
        self.calls.append(request)
        self.assertEqual(request.get_method(), "GET")
        self.assertEqual(request.get_header("Authorization"), "Bearer " + TOKEN)
        self.assertEqual(timeout, 5)
        path = request.full_url.removeprefix("http://127.0.0.1:12345")
        self.assertIn(path, self.snapshots)
        if self.error:
            raise self.error
        return io.BytesIO(json.dumps(self.snapshots[path]).encode())

    def observe(self, value=None):
        (self.home / "workshop.json").write_text(json.dumps(self.ledger))
        return helper.observe(self.config, value or self.input, opener=self, now=NOW, presence_reader=self.read_presence)

    def test_fixed_reads_bind_accepted_task_to_original_private_queue(self):
        result = self.observe()
        self.assertEqual(result["availability"], "available")
        self.assertEqual(len(self.calls), 3)
        self.assertEqual(self.presence_calls, [(12345, "Coach")])
        action = result["actions"][0]
        self.assertEqual(action["state"], "accepted")
        self.assertEqual(action["receipt"]["task_id"], self.task["id"])
        self.assertEqual(action["provenance"]["source_id"], self.queued)
        self.assertFalse(action["verified"])
        messages = {e["actor"]: e for e in result["events"] if e["kind"] == "message"}
        self.assertEqual(messages["Operator"]["audience"]["kind"], "private")
        self.assertEqual(messages["Other player"]["audience"]["kind"], "public")
        self.assertEqual(messages["Coach"]["text"], "The passage is open; the mechanism is not tested.")
        self.assertEqual(messages["Coach"]["id"], "minecraft:message:own-event")
        self.assertEqual(messages["Coach"]["occurred_at"], NOW)
        self.assertNotIn(TOKEN, json.dumps(result))
        self.assertNotIn(PRESENCE_TOKEN, json.dumps(result))

    def test_current_task_progress_and_reported_completion_are_not_verified(self):
        for status, expected in [("running", "running"), ("waiting", "waiting"), ("review", "reported_complete"),
                                 ("needs_review", "needs_review"), ("stopped", "stopped"), ("budget_exhausted", "budget_exhausted")]:
            self.task.update(status=status, turns=1, progress_revision=1, progress=[{"at": NOW, "summary": "Agent report"}])
            action = self.observe()["actions"][0]
            self.assertEqual(action["state"], expected)
            self.assertFalse(action["verified"])
            self.assertNotEqual(action["state"], "completed")

    def test_different_current_task_cannot_complete_old_action_but_retained_binding_can_be_superseded(self):
        old = self.task.copy()
        self.task = {**self.task, "id": str(uuid.uuid4()), "request_id": "unrelated", "authority": {}}
        self.snapshots["/installation-state"]["task"] = self.task
        self.ledger["task"] = self.task
        self.assertEqual(self.observe()["actions"][0]["state"], "unknown")
        self.ledger["task_history"] = [{**old, "status": "review"}]
        action = self.observe()["actions"][0]
        self.assertEqual(action["state"], "superseded")
        self.assertEqual(action["reported_state"], "reported_complete")
        self.assertFalse(action["verified"])

    def test_mismatched_receipt_source_and_snapshot_fail_closed(self):
        self.task["authority"]["source"]["id"] = "different"
        self.assertEqual(self.observe()["actions"][0]["state"], "unknown")
        self.ledger["task"] = {"id": "new-task"}
        result = self.observe()
        self.assertEqual(result["availability"], "unavailable")
        self.assertEqual(result["events"], [])
        self.assertEqual(result["actions"][0]["state"], "unknown")

    def test_cursor_deduplicates_but_keeps_changed_task_revision(self):
        initial = self.observe()
        repeated = self.observe({**self.input, "cursor": initial["cursor"]})
        self.assertEqual(repeated["events"], [])
        self.task.update(progress_revision=2, progress=[{"at": NOW, "summary": "New result"}])
        changed = self.observe({**self.input, "cursor": initial["cursor"]})
        self.assertEqual([e["kind"] for e in changed["events"]], ["task_observation"])

    def test_observation_clock_does_not_change_semantic_action_revision(self):
        first = self.observe()
        second = helper.observe(self.config, self.input, opener=self, now="2026-11-07T20:01:00+00:00", presence_reader=self.read_presence)
        self.assertNotEqual(first["actions"][0]["observed_at"], second["actions"][0]["observed_at"])
        self.assertEqual(first["actions"][0]["revision"], second["actions"][0]["revision"])

    def test_default_transport_disables_proxies_redirects_and_enforces_shared_deadline(self):
        self.observe()
        self.calls.clear()
        with patch.object(helper.urllib.request, "build_opener", return_value=self) as build:
            result = helper.observe(self.config, self.input, now=NOW, presence_reader=self.read_presence)
        self.assertEqual(result["availability"], "available")
        handlers = build.call_args.args
        self.assertEqual(handlers[0].proxies, {})
        self.assertIsNone(handlers[1].redirect_request(None, None, None, None, None, None))
        self.calls.clear()
        with patch.object(helper.time, "monotonic", side_effect=[0, 10]):
            result = helper.observe(self.config, self.input, opener=self, now=NOW)
        self.assertEqual(result["availability"], "unavailable")
        self.assertEqual(self.calls, [])

    def test_errors_preserve_cursor_without_retry_or_secret_output(self):
        initial = self.observe()
        self.calls.clear()
        self.error = TimeoutError(TOKEN)
        result = self.observe({**self.input, "cursor": initial["cursor"]})
        self.assertEqual(result["cursor"], initial["cursor"])
        self.assertEqual(result["availability"], "unavailable")
        self.assertEqual(len(self.calls), 1)
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_bounded_utf8_events_output_and_no_arbitrary_input_targets(self):
        self.snapshots["/operator-inputs"]["messages"] = [{"id": f"private-{i}", "time": NOW, "author": "player", "text": "漢" * 3000} for i in range(16)]
        self.public["messages"] = [{"id": f"public-{i}", "at": NOW, "actor": "player", "text": "漢" * 3000} for i in range(16)]
        result = self.observe()
        self.assertLessEqual(len(helper.encoded(result)), 131072)
        self.assertLessEqual(len(result["events"]), 32)
        self.assertTrue(result["coverage"]["gap"])
        self.assertEqual(result["coverage"]["omitted_events"], 16)
        self.assertEqual(result["coverage"]["private_messages_withheld"], 16)
        self.assertFalse(any(e["kind"] == "message" for e in result["events"]), "Oversize source text must be omitted whole, never clipped")
        self.assertTrue(all(len(e["text"].encode()) <= 4000 for e in result["events"]))
        self.calls.clear()
        self.assertEqual(self.observe({**self.input, "url": "https://attacker.invalid"})["availability"], "unavailable")
        self.assertEqual(self.calls, [])

    def burst(self, message):
        self.snapshots["/operator-inputs"]["messages"] = [
            {"id": self.queued if i == 0 else f"private-{i}", "time": NOW, "author": "Operator", "text": message} for i in range(16)]
        self.public["messages"] = [
            {"id": f"own-public-{i}", "at": NOW, "actor": "Coach", "text": message} for i in range(20)]

    def drain_burst(self, max_polls=8):
        cursor, emitted, batches = None, {}, []
        for poll in range(max_polls):
            self.presence["observed_at"] = f"2026-11-07T20:00:{poll:02d}+00:00"
            result = self.observe({**self.input, "cursor": cursor})
            self.assertEqual(result["availability"], "available")
            self.assertLessEqual(len(helper.encoded(result)), helper.MAX_OUTPUT)
            previous = {row["id"]: row["revision"] for row in (cursor or {}).get("seen", [])}
            for event in result["events"]:
                self.assertNotIn(event["id"], emitted, "Unchanged burst records must not repeat")
                emitted[event["id"]] = event
                previous[event["id"]] = event["revision"]
            self.assertEqual({row["id"]: row["revision"] for row in result["cursor"]["seen"]}, previous,
                             "Only emitted records may advance the acknowledgement cursor")
            cursor = result["cursor"]
            batches.append(result)
            if not result["events"]:
                break
        self.assertEqual(len(emitted), 23, "This call's private input, 20 public messages, presence and task must drain")
        self.assertEqual(sum(event["actor"] == "Coach" and event["kind"] == "message" for event in emitted.values()), 20)
        self.assertEqual(batches[-1]["events"], [])
        return batches

    def test_count_bounded_burst_advances_only_emitted_records(self):
        self.burst("Distinct public and private source records")
        with patch.object(helper, "MAX_EVENTS", 16):
            batches = self.drain_burst()
        self.assertEqual(len(batches[0]["events"]), 16)
        self.assertEqual(batches[0]["coverage"]["deferred_events"], 7)
        self.assertEqual(len(batches[1]["events"]), 7)

    def test_byte_bounded_burst_is_delivered_on_following_identical_polls(self):
        self.burst("x" * 4000)
        with patch.object(helper, "MAX_OUTPUT", 80000):
            batches = self.drain_burst()
        self.assertTrue(batches[0]["coverage"]["output_truncated"])
        self.assertLess(len(batches[0]["events"]), 23)
        self.assertGreater(batches[0]["coverage"]["deferred_events"], 0)

    def test_deferred_presence_keeps_identity_without_becoming_acknowledged(self):
        self.burst("x" * 4000)
        # Force task-only output first, then allow the pending presence to drain.
        with patch.object(helper, "MAX_OUTPUT", 2000):
            first = self.observe()
        self.assertEqual(first["availability"], "available")
        self.assertFalse(any(row["id"].startswith("minecraft:presence:") for row in first["cursor"]["seen"]))
        self.assertIn("presence", first["cursor"])
        pending = first["cursor"]["presence"]
        self.presence["observed_at"] = "2026-11-07T20:00:20+00:00"
        second = self.observe({**self.input, "cursor": first["cursor"]})
        event = next(event for event in second["events"] if event["kind"] == "presence_observation")
        self.assertEqual(event["occurred_at"], pending["occurred_at"])
        self.assertEqual(event["revision"], pending["revision"])

    def test_unbounded_task_qualification_is_explicitly_omitted_not_clipped(self):
        self.task["progress"] = [{"at": NOW, "summary": "x" * 3999 + " DO NOT CLAIM FINISHED"}]
        result = self.observe()
        self.assertEqual(result["actions"][0]["task"]["progress"]["summary"], "")
        self.assertEqual(result["actions"][0]["task"]["omitted_fields"], ["progress.summary"])
        self.assertFalse(any(e["kind"] == "task_observation" for e in result["events"]))
        self.assertEqual(result["coverage"]["omitted_events"], 1)

    def test_public_presence_is_small_direct_and_excludes_private_task_and_snapshot_fields(self):
        result = self.observe()
        event = next(e for e in result["events"] if e["kind"] == "presence_observation")
        value = json.loads(event["text"])
        self.assertEqual(value["position"], {"x": 1, "y": 64, "z": 2})
        self.assertEqual(value["players"][0]["name"], "Brian")
        self.assertEqual(value["players"][0]["distance_blocks"], 5)
        self.assertFalse(value["busy"])
        public = json.dumps([e for e in result["events"] if e["audience"]["kind"] == "public"])
        for private in ["Private instruction", "Build the tower", "must-not-project", "private-metadata", TOKEN, PRESENCE_TOKEN]:
            self.assertNotIn(private, public)
        self.presence["observed_at"] = "2026-11-07T20:00:20+00:00"
        self.assertEqual(self.observe({**self.input, "cursor": result["cursor"]})["events"], [])
        self.presence["position"]["x"] = 2
        changed = self.observe({**self.input, "cursor": result["cursor"]})
        self.assertEqual([e["kind"] for e in changed["events"]], ["presence_observation"])
        self.presence["position"]["x"] = 1
        self.presence["observed_at"] = "2026-11-07T20:00:30+00:00"
        returned = self.observe({**self.input, "cursor": changed["cursor"]})
        self.assertNotEqual(returned["events"][0]["revision"], event["revision"])
        self.assertEqual(returned["events"][0]["occurred_at"], self.presence["observed_at"])

    def test_missing_presence_is_explicit_and_does_not_use_incoming_only_history(self):
        (self.home / "config.json").write_text(json.dumps({"username": "Coach", "control_port": 12345, "token": TOKEN}))
        result = self.observe()
        self.assertEqual(result["availability"], "available")
        self.assertFalse(result["coverage"]["public_presence_available"])
        self.assertTrue(result["coverage"]["gap"])
        self.assertEqual(self.presence_calls, [])
        self.assertFalse(any(e["audience"]["kind"] == "public" for e in result["events"]))

    def test_wrong_presence_identity_cannot_leak_other_agents_public_or_private_data(self):
        self.public["username"] = "Other agent"
        self.public["messages"][0]["text"] = "Wrong connected agent message"
        result = self.observe()
        self.assertFalse(result["coverage"]["public_presence_available"])
        self.assertNotIn("Wrong connected agent message", json.dumps(result))

    def make_conversation_only(self):
        self.task["request_id"] = "original-public-task-source"
        self.task["authority"]["source"]["id"] = self.task["request_id"]
        self.ledger["conversation_admissions"] = [{"at": 1794081600000, "ids": [self.queued]}]

    def test_exact_conversation_admission_is_not_task_acceptance_or_answer(self):
        self.make_conversation_only()
        action = self.observe()["actions"][0]
        self.assertEqual(action["state"], "admitted")
        self.assertEqual(action["scope"], "conversation")
        self.assertEqual(action["receipt"], {"request_id": self.queued})
        self.assertEqual(action["fulfillment"], "not_established")
        self.assertFalse(action["conversation"]["local_turn_settled"])
        self.assertFalse(action["conversation"]["reply_verified"])
        self.assertFalse(action["verified"])
        self.assertNotIn("task", action)

    def test_last_conversation_settlement_needs_exact_source_and_never_fabricates_a_reply(self):
        self.make_conversation_only()
        self.ledger["last_conversation_turn"] = {"conversation_only": True, "request_ids": [self.queued],
                                                 "started_at": NOW, "interrupted": False,
                                                 "messages": [{"text": "Private raw input"}], "actions": [{"secret": TOKEN}]}
        action = self.observe()["actions"][0]
        self.assertEqual(action["state"], "settled")
        self.assertTrue(action["conversation"]["local_turn_settled"])
        self.assertFalse(action["conversation"]["reply_verified"])
        self.assertNotIn("Private raw input", json.dumps(action))
        self.ledger["last_conversation_turn"]["interrupted"] = True
        self.assertEqual(self.observe()["actions"][0]["state"], "interrupted")
        self.ledger["last_conversation_turn"]["request_ids"] = ["different"]
        self.assertEqual(self.observe()["actions"][0]["state"], "admitted")
        self.ledger["conversation_admissions"] = []
        self.assertEqual(self.observe()["actions"][0]["state"], "unknown")

    def test_later_amendment_joins_exact_authorization_without_replacing_original_task_source(self):
        self.make_conversation_only()
        self.ledger["task_authorizations"] = [{"operation": "amend", "source": {"id": self.queued,
            "revision": "amendment-revision", "evidence_origin": "authenticated_operator_control", "audience": {"kind": "private"}},
            "result": {"task_id": self.task["id"], "accepted": True}}]
        action = self.observe()["actions"][0]
        self.assertEqual(action["scope"], "task")
        self.assertEqual(action["task"]["id"], self.task["id"])
        self.assertEqual(action["provenance"]["task_transition"], "amend")
        self.assertEqual(action["provenance"]["source_revision"], "amendment-revision")
        self.assertFalse(action["verified"])
        self.assertEqual(self.task["request_id"], "original-public-task-source")
        self.ledger["task_authorizations"][0]["source"]["audience"] = {"kind": "public"}
        self.assertEqual(self.observe()["actions"][0]["scope"], "conversation")

    def advertise_outcome(self, delivery="terminal", **overrides):
        self.snapshots["/status"]["input_outcomes"] = {"version": 1, "method": "GET", "path": "/input-outcomes", "max_ids": 16}
        outcome = {"request_id": self.queued, "delivery": delivery, "admitted_at": NOW, "terminal_at": NOW,
                   "conversation_only": True, "turn_task_id": None, "blocked_reason": "task_interrupted",
                   "responses": [{"id": "captured-public-reply", "at": NOW, "text": "I need the interrupted task reviewed.", "audience": "public"}],
                   "task_authorizations": [], "completion": "not_established", **overrides}
        path = "/input-outcomes?" + urllib.parse.urlencode({"ids": self.queued})
        self.snapshots[path] = {"version": 1, "observed_at": NOW, "outcomes": [outcome]}
        return outcome, self.snapshots[path]

    def test_durable_terminal_keeps_captured_blockage_and_is_not_fulfillment(self):
        self.advertise_outcome()
        # Today's task is running; it cannot rewrite the earlier input's blocker.
        action = self.observe()["actions"][0]
        self.assertEqual(action["state"], "settled")
        self.assertEqual(action["scope"], "conversation")
        self.assertEqual(action["delivery_state"], "terminal")
        self.assertEqual(action["conversation"]["blocked_reason"], "task_interrupted")
        self.assertEqual(action["conversation"]["terminal_at"], NOW)
        self.assertEqual(action["responses"][0]["text"], "I need the interrupted task reviewed.")
        self.assertEqual(action["response_association"], "same_turn_source_set_not_individual_fulfillment")
        self.assertEqual(action["fulfillment"], "not_established")
        self.assertFalse(action["verified"])
        self.assertFalse(action["conversation"]["reply_verified"])
        self.assertNotIn("task", action)
        self.assertEqual(action["provenance"]["source_id"], self.queued)
        self.assertEqual(action["provenance"]["version"], 1)

    def test_capability_negotiation_never_follows_an_advertised_arbitrary_route(self):
        base = {"version": 1, "method": "GET", "path": "/input-outcomes", "max_ids": 16}
        for change in [{"version": True}, {"version": 2}, {"method": "POST"}, {"path": "https://attacker.invalid"},
                       {"path": "/task"}, {"max_ids": True}, {"max_ids": 0}, {"max_ids": 17}]:
            with self.subTest(change=change):
                self.calls.clear()
                self.snapshots["/status"]["input_outcomes"] = {**base, **change}
                result = self.observe()
                self.assertEqual(result["coverage"]["input_outcomes"], "unsupported")
                self.assertEqual(result["actions"][0]["state"], "accepted", "Legacy exact task evidence stays available")
                self.assertEqual([call.full_url.split(":12345")[1] for call in self.calls],
                                 ["/installation-state", "/operator-inputs", "/status"])

    def test_no_actions_or_unconfirmed_queue_never_queries_outcomes(self):
        self.advertise_outcome()
        for value in [{"version": 1, "actions": []},
                      {**self.input, "actions": [{**self.input["actions"][0], "receipt": {"state": "unknown"}}]}]:
            self.calls.clear()
            result = self.observe(value)
            self.assertEqual(result["coverage"]["input_outcomes"], "not_requested")
            self.assertEqual(len(self.calls), 1)
        self.input["actions"][0]["receipt"]["receipt"]["request_id"] = "/task?secret=bad"
        self.calls.clear()
        self.assertEqual(self.observe()["coverage"]["input_outcomes"], "not_requested")
        self.assertEqual(len(self.calls), 1)

    def test_private_response_content_never_escapes_a_coadmitted_turn(self):
        private = "Different caller's private answer must not enter this call"
        row, _ = self.advertise_outcome()
        row["responses"].append({"id": "private-reply", "at": NOW, "text": private, "audience": "private", "secret": TOKEN})
        result = self.observe()
        action = result["actions"][0]
        self.assertEqual(action["private_responses_withheld"], 1)
        self.assertEqual(len(action["responses"]), 1)
        self.assertNotIn(private, json.dumps(result))
        self.assertNotIn(TOKEN, json.dumps(result))
        self.assertEqual(action["audience"]["kind"], "private")

    def test_accepted_authority_receipt_is_separate_from_terminal_or_completion(self):
        row, _ = self.advertise_outcome("admitted", terminal_at=None, conversation_only=False,
                                       turn_task_id=self.task["id"], blocked_reason=None)
        row["task_authorizations"] = [{"operation": "amend", "task_id": self.task["id"], "authority_revision": 2,
                                        "private_original_text": "must-not-be-copied"}]
        action = self.observe()["actions"][0]
        self.assertEqual(action["state"], "admitted")
        self.assertFalse(action["conversation"]["local_turn_settled"])
        self.assertEqual(action["task_authorizations"], [{"operation": "amend", "task_id": self.task["id"], "authority_revision": 2}])
        self.assertFalse(action["verified"])
        self.assertEqual(action["fulfillment"], "not_established")

    def test_outcome_states_never_masquerade_as_successful_task_completion(self):
        for delivery, expected in [("queued", "unknown"), ("admitted", "admitted"), ("terminal", "settled"),
                                   ("interrupted", "interrupted"), ("unknown", "unknown")]:
            self.advertise_outcome(delivery)
            action = self.observe()["actions"][0]
            self.assertEqual(action["state"], expected)
            self.assertEqual(action["delivery_state"], delivery)
            self.assertEqual(action["fulfillment"], "not_established")
            self.assertFalse(action["verified"])

    def test_missing_retained_outcome_does_not_fall_back_to_current_task(self):
        _, snapshot = self.advertise_outcome()
        snapshot["outcomes"] = []
        result = self.observe()
        self.assertEqual(result["availability"], "available")
        self.assertEqual(result["actions"][0]["state"], "unknown")
        self.assertEqual(result["actions"][0]["reason"], "input_outcome_not_retained")
        self.assertNotIn("task", result["actions"][0])

    def test_malformed_or_cross_source_outcomes_are_unknown_without_content_leak(self):
        for change in [{"request_id": str(uuid.uuid4())}, {"delivery": "completed"}, {"completion": "verified"},
                       {"conversation_only": "true"}, {"terminal_at": "yesterday"},
                       {"task_authorizations": [{"operation": "accept", "task_id": "task", "authority_revision": True}]},
                       {"responses": [{"id": "unknown-audience", "at": NOW, "text": TOKEN, "audience": "everybody"}]}]:
            with self.subTest(change=change):
                self.advertise_outcome(**change)
                result = self.observe()
                self.assertEqual(result["availability"], "available")
                self.assertEqual(result["actions"][0]["state"], "unknown")
                self.assertEqual(result["coverage"]["input_outcomes"], "unavailable")
                self.assertNotIn(TOKEN, json.dumps(result))

    def test_duplicate_outcome_identity_and_unknown_version_are_rejected(self):
        row, snapshot = self.advertise_outcome()
        snapshot["outcomes"].append(row)
        self.assertEqual(self.observe()["actions"][0]["state"], "unknown")
        snapshot["outcomes"] = [row]
        snapshot["version"] = 2
        self.assertEqual(self.observe()["actions"][0]["state"], "unknown")

    def test_public_replies_are_bounded_whole_and_omissions_remain_explicit(self):
        row, _ = self.advertise_outcome()
        row["responses"] = [{"id": f"reply-{i}", "at": NOW, "text": "x" * 3990 + " not done", "audience": "public"}
                            for i in range(6)]
        action = self.observe()["actions"][0]
        self.assertEqual(len(action["responses"]), 1)
        self.assertEqual(action["responses_omitted"], 5)
        self.assertTrue(action["responses"][0]["text"].endswith(" not done"))
        self.assertLessEqual(len(helper.encoded(action)), 16000)

    def test_outcome_observation_poll_time_does_not_change_revision(self):
        self.advertise_outcome()
        first = self.observe()["actions"][0]
        second = helper.observe(self.config, self.input, opener=self, now="2026-11-07T20:01:00+00:00",
                                presence_reader=self.read_presence)["actions"][0]
        self.assertEqual(first["revision"], second["revision"])
        self.assertNotEqual(first["observed_at"], second["observed_at"])

    def test_outcome_query_failure_does_not_retry_or_erase_other_observations(self):
        self.advertise_outcome()
        original = self.open
        def fail_outcomes(request, timeout):
            if "/input-outcomes?" in request.full_url:
                self.calls.append(request)
                raise TimeoutError(TOKEN)
            return original(request, timeout)
        with patch.object(self, "open", side_effect=fail_outcomes):
            result = self.observe()
        self.assertEqual(result["availability"], "available")
        self.assertEqual(result["actions"][0]["state"], "unknown")
        self.assertTrue(result["events"])
        self.assertEqual(sum("/input-outcomes?" in call.full_url for call in self.calls), 1)
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_negotiated_batch_limit_and_original_request_mapping_are_preserved(self):
        row, _ = self.advertise_outcome()
        self.snapshots["/status"]["input_outcomes"]["max_ids"] = 1
        second_request, second_queue = str(uuid.uuid4()), str(uuid.uuid4())
        self.input["actions"].append({"request_id": second_request,
            "receipt": {"request_id": second_request, "state": "queued", "scope": "private_operator_instruction",
                        "receipt": {"request_id": second_queue, "queued": True}}})
        self.snapshots["/input-outcomes?ids=" + second_queue] = {"version": 1, "observed_at": NOW,
            "outcomes": [{**row, "request_id": second_queue, "delivery": "interrupted"}]}
        result = self.observe()
        self.assertEqual([(a["request_id"], a["receipt"]["request_id"], a["state"]) for a in result["actions"]],
                         [(self.request, self.queued, "settled"), (second_request, second_queue, "interrupted")])
        queried = [urllib.parse.parse_qs(urllib.parse.urlparse(call.full_url).query)["ids"][0]
                   for call in self.calls if "/input-outcomes?" in call.full_url]
        self.assertEqual(queried, [self.queued, second_queue])

    def test_two_companion_requests_cannot_claim_one_original_queue_outcome(self):
        self.advertise_outcome()
        second_request = str(uuid.uuid4())
        self.input["actions"].append({"request_id": second_request,
            "receipt": {**self.input["actions"][0]["receipt"], "request_id": second_request}})
        result = self.observe()
        self.assertEqual([a["state"] for a in result["actions"]], ["unknown", "unknown"])
        self.assertFalse(any("/input-outcomes?" in call.full_url for call in self.calls))

    def test_two_calls_only_receive_their_original_private_instructions(self):
        other_request, other_queue = str(uuid.uuid4()), str(uuid.uuid4())
        self.snapshots["/operator-inputs"]["messages"].append(
            {"id": other_queue, "time": NOW, "author": "Operator", "text": "Other caller's private instruction"})
        first = self.observe()
        self.assertIn("Private instruction", json.dumps(first))
        self.assertNotIn("Other caller's private instruction", json.dumps(first))
        second_input = {"version": 1, "actions": [{"request_id": other_request,
            "receipt": {"request_id": other_request, "state": "queued", "scope": "private_operator_instruction",
                        "receipt": {"request_id": other_queue, "queued": True}}}]}
        second = self.observe(second_input)
        self.assertIn("Other caller's private instruction", json.dumps(second))
        self.assertNotIn("Private instruction", json.dumps(second))
        self.assertNotIn("Build the tower", json.dumps(second), "Another caller's private task must also be withheld")
        for result in [first, second]:
            self.assertTrue(any(e["id"] == "minecraft:message:own-event" for e in result["events"]),
                            "Public speech remains available to either call")
            self.assertEqual(result["coverage"]["private_messages_withheld"], 1)

    def test_direct_status_has_no_private_instruction_or_task_access(self):
        result = self.observe({"version": 1, "actions": []})
        self.assertEqual(result["availability"], "available")
        self.assertFalse(any(e["audience"]["kind"] == "private" for e in result["events"]))
        self.assertNotIn("Private instruction", json.dumps(result))
        self.assertNotIn("Build the tower", json.dumps(result))
        self.assertFalse(any("/operator-inputs" in call.full_url for call in self.calls))
        self.assertTrue(result["coverage"]["private_task_withheld"])
        self.task["authority"]["source"]["audience"] = {"kind": "public"}
        result = self.observe({"version": 1, "actions": []})
        self.assertTrue(any(e["kind"] == "task_observation" for e in result["events"]),
                        "Public task observations are not private call data")

    def test_unknown_and_duplicate_receipts_do_not_grant_private_context(self):
        unknown = {**self.input["actions"][0], "receipt": {**self.input["actions"][0]["receipt"], "state": "unknown"}}
        duplicate_id = str(uuid.uuid4())
        duplicate = {"request_id": duplicate_id, "receipt": {**self.input["actions"][0]["receipt"], "request_id": duplicate_id}}
        for actions in [[unknown], [*self.input["actions"], duplicate]]:
            self.calls.clear()
            result = self.observe({"version": 1, "actions": actions})
            self.assertEqual([a["state"] for a in result["actions"]], ["unknown"] * len(actions))
            self.assertNotIn("Private instruction", json.dumps(result))
            self.assertNotIn("Build the tower", json.dumps(result))
            self.assertFalse(any("/operator-inputs" in call.full_url for call in self.calls))

    def test_cli_invalid_json_is_sanitized(self):
        result = subprocess.run([sys.executable, str(HELPER), "--config", str(self.config)], input="bad " + TOKEN,
                                text=True, capture_output=True, check=True, timeout=5)
        self.assertEqual(result.stderr, "")
        self.assertNotIn(TOKEN, result.stdout)
        self.assertEqual(json.loads(result.stdout)["availability"], "unavailable")


if __name__ == "__main__":
    unittest.main()
