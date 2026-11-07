"""Only temporary owner files and fake transports; no installed service calls."""
import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
import uuid
from unittest.mock import patch

HELPER = pathlib.Path(__file__).resolve().parents[1] / "minecraft-observe.py"
SPEC = importlib.util.spec_from_file_location("minecraft_observe", HELPER)
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)
TOKEN = "fixture-control-secret-never-output"
NOW = "2026-11-07T20:00:00+00:00"


class MinecraftObserveTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="companion-observe-")
        self.addCleanup(tmp.cleanup)
        self.home = pathlib.Path(tmp.name)
        self.config = self.home / "observer.json"
        self.config.write_text(json.dumps({"minecraft_home": str(self.home), "connection_id": "minecraft:coach", "expected_username": "Coach"}))
        (self.home / "config.json").write_text(json.dumps({"username": "Coach", "control_port": 12345, "token": TOKEN}))
        self.request = str(uuid.uuid4())
        self.queued = str(uuid.uuid4())
        self.task = {"id": str(uuid.uuid4()), "request_id": self.queued, "status": "running", "turns": 0,
                     "started_at": NOW, "goal": "Build the tower", "authority_revision": 1, "progress_revision": 0, "progress": [],
                     "authority": {"source": {"id": self.queued, "revision": "original-source-revision",
                                               "evidence_origin": "authenticated_operator_control", "audience": {"kind": "private"}}}}
        self.ledger = {"task": self.task, "task_history": [], "events": []}
        self.snapshots = {"/installation-state": {"ready": True, "task": self.task},
                          "/operator-inputs": {"messages": [{"id": self.queued, "time": NOW, "author": "Operator", "text": "Private instruction"}]},
                          "/conversation": {"messages": [{"id": "public-event", "time": NOW, "author": "Other player", "text": "Public question"}]}}
        self.input = {"version": 1, "cursor": None, "actions": [{"request_id": self.request,
                      "receipt": {"request_id": self.request, "state": "queued", "scope": "private_operator_instruction",
                                  "receipt": {"request_id": self.queued, "queued": True}}}]}
        self.calls = []
        self.error = None

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
        return helper.observe(self.config, value or self.input, opener=self, now=NOW)

    def test_fixed_reads_bind_accepted_task_to_original_private_queue(self):
        result = self.observe()
        self.assertEqual(result["availability"], "available")
        self.assertEqual(len(self.calls), 3)
        action = result["actions"][0]
        self.assertEqual(action["state"], "accepted")
        self.assertEqual(action["receipt"]["task_id"], self.task["id"])
        self.assertEqual(action["provenance"]["source_id"], self.queued)
        self.assertFalse(action["verified"])
        messages = {e["actor"]: e for e in result["events"] if e["kind"] == "message"}
        self.assertEqual(messages["Operator"]["audience"]["kind"], "private")
        self.assertEqual(messages["Other player"]["audience"]["kind"], "public")
        self.assertNotIn(TOKEN, json.dumps(result))

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
        second = helper.observe(self.config, self.input, opener=self, now="2026-11-07T20:01:00+00:00")
        self.assertNotEqual(first["actions"][0]["observed_at"], second["actions"][0]["observed_at"])
        self.assertEqual(first["actions"][0]["revision"], second["actions"][0]["revision"])

    def test_default_transport_disables_proxies_redirects_and_enforces_shared_deadline(self):
        self.observe()
        self.calls.clear()
        with patch.object(helper.urllib.request, "build_opener", return_value=self) as build:
            result = helper.observe(self.config, self.input, now=NOW)
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
        for key in ["/operator-inputs", "/conversation"]:
            self.snapshots[key]["messages"] = [{"id": f"{key[1:]}-{i}", "time": NOW, "author": "player", "text": "漢" * 3000} for i in range(16)]
        result = self.observe()
        self.assertLessEqual(len(helper.encoded(result)), 131072)
        self.assertLessEqual(len(result["events"]), 32)
        self.assertTrue(result["coverage"]["gap"])
        self.assertEqual(result["coverage"]["omitted_events"], 32)
        self.assertFalse(any(e["kind"] == "message" for e in result["events"]), "Oversize source text must be omitted whole, never clipped")
        self.assertTrue(all(len(e["text"].encode()) <= 4000 for e in result["events"]))
        self.calls.clear()
        self.assertEqual(self.observe({**self.input, "url": "https://attacker.invalid"})["availability"], "unavailable")
        self.assertEqual(self.calls, [])

    def test_unbounded_task_qualification_is_explicitly_omitted_not_clipped(self):
        self.task["progress"] = [{"at": NOW, "summary": "x" * 3999 + " DO NOT CLAIM FINISHED"}]
        result = self.observe()
        self.assertEqual(result["actions"][0]["task"]["progress"]["summary"], "")
        self.assertEqual(result["actions"][0]["task"]["omitted_fields"], ["progress.summary"])
        self.assertFalse(any(e["kind"] == "task_observation" for e in result["events"]))
        self.assertEqual(result["coverage"]["omitted_events"], 1)

    def test_cli_invalid_json_is_sanitized(self):
        result = subprocess.run([sys.executable, str(HELPER), "--config", str(self.config)], input="bad " + TOKEN,
                                text=True, capture_output=True, check=True, timeout=5)
        self.assertEqual(result.stderr, "")
        self.assertNotIn(TOKEN, result.stdout)
        self.assertEqual(json.loads(result.stdout)["availability"], "unavailable")


if __name__ == "__main__":
    unittest.main()
