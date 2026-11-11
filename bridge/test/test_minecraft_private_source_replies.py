"""Exact private source replies; actual disposable Minecraft producer fixture."""
import copy
import hashlib
import importlib.util
import json
import pathlib
import unittest

BRIDGE = pathlib.Path(__file__).resolve().parents[1]
def load(name, file):
    spec = importlib.util.spec_from_file_location(name, BRIDGE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

instruction = load("private_source_instruction", "minecraft-instruction.py")
observer = load("private_source_observer", "minecraft-observe.py")
FIXTURE = BRIDGE / "test/fixtures/minecraft-private-source-reply.json"

class PrivateSourceReplies(unittest.TestCase):
    def setUp(self):
        self.fixture = json.loads(FIXTURE.read_text())
        self.capability = self.fixture["capability"]
        self.source = self.fixture["input"]["source"]
        self.queue = self.fixture["receipt"]["request_id"]
        self.row = self.fixture["response"]["outcomes"][0]
        self.action = {"request_id":self.source["request_id"], "receipt":{
            "request_id":self.source["request_id"], "scope":"private_installation_input",
            "state":"queued", "protocol":"installation_inputs.v1", "caller_id":"companion",
            "source":self.source, "receipt":self.fixture["receipt"]}}
        self.calls = []
    def query(self, row=None, capability=None, action=None):
        def exchange(method, path, payload, token):
            self.calls.append((method, path, payload))
            if path == "/source-capabilities":
                return self.capability if capability is None else capability
            self.assertEqual((method, path), ("POST", "/source-outcomes"))
            return {**self.fixture["response"], "outcomes":[self.row if row is None else row]}
        return observer.read_scoped_outcomes(exchange, [self.action if action is None else action],
                                            {"caller_id":"companion", "token":"disposable"})
    def project(self, row=None, capability=None, at=None):
        available, rows = self.query(row, capability)
        return observer.outcome_observation(self.action, "minecraft:fixture", at or self.fixture["response"]["observed_at"], rows, available)
    def test_actual_fixture_preserves_original_reply_without_delivery_or_effect_claim(self):
        self.assertEqual(hashlib.sha256(FIXTURE.read_bytes()).hexdigest(), "856ddda448526f80124618721a60ba0e565e0423e6342fcd8cfa68fbbab382df")
        self.assertTrue(instruction.source_capability(self.capability))
        result = self.project()
        self.assertEqual(result["source_replies"], self.row["source_replies"])
        self.assertEqual(result["source_replies_capability"], self.capability["source_replies"])
        self.assertEqual(result["installation_source"], self.row["source"])
        self.assertEqual(result["fulfillment"], "not_established")
        self.assertEqual(result["state"], "settled")
        self.assertFalse(result["verified"])
        self.assertFalse(result["conversation"]["reply_verified"])
        self.assertFalse(result["private_game_responses_included"])
        self.assertEqual(result["responses"], [])
    def test_capability_is_optional_exact_and_does_not_reinterpret_public_replies(self):
        old = copy.deepcopy(self.capability); del old["source_replies"]
        self.assertTrue(instruction.source_capability(old))
        public = copy.deepcopy(self.row); del public["source_replies"]
        public["responses"] = [{"id":"public", "at":self.row["terminal_at"], "audience":"public", "text":"Public observation"}]
        result = self.project(public, old)
        self.assertEqual(result["responses"], public["responses"])
        self.assertNotIn("source_replies", result)
        self.assertEqual(self.query(capability=old), ("unavailable", {}))
        for extension in [None, {}, {"version":True,"max_items":32,"max_text_bytes":12000},
                          {"version":1.0,"max_items":32,"max_text_bytes":12000},
                          {"version":1,"max_items":32.0,"max_text_bytes":12000},
                          {"version":1,"max_items":32,"max_text_bytes":12000.0},
                          {"version":2,"max_items":32,"max_text_bytes":12000},
                          {"version":1,"max_items":32,"max_text_bytes":12000,"route":"/other"}]:
            cap = {**self.capability, "source_replies":extension}
            self.assertTrue(instruction.source_capability(cap))
            self.assertFalse(instruction.source_replies_capability(extension))
            self.assertEqual(self.query(capability=cap), ("unavailable", {}))
            self.assertEqual(self.query(public, cap)[0], "available")
        public["responses"][0]["audience"] = "private"
        self.assertEqual(self.query(public, old), ("unavailable", {}))
    def test_exact_private_audience_caller_source_and_recorded_delivery_required(self):
        for key, value in [("audience", {**self.source["audience"],"id":"another-room"}),
                           ("audience", {**self.source["audience"],"experience":"voice"}),
                           ("audience", "private"), ("delivery", "heard"), ("delivery", "sent"),
                           ("at", "today"), ("id", ""), ("id", "invalid\nidentity"), ("text", "")]:
            row = copy.deepcopy(self.row); row["source_replies"][0][key] = value
            self.assertEqual(self.query(row), ("unavailable", {}), key)
        for key, value in [("caller", {"id":"other-caller","label":"Other"}),
                           ("request_id","other-source"), ("call_id","another-call"), ("revision","another-revision")]:
            row = copy.deepcopy(self.row); row["source"][key] = value
            self.assertEqual(self.query(row), ("unavailable", {}), key)
        row = copy.deepcopy(self.row); row["source_replies"].append(row["source_replies"][0])
        self.assertEqual(self.query(row), ("unavailable", {}))
        # IDs are opaque; the current producer's hash algorithm is not a wire requirement.
        row = copy.deepcopy(self.row); row["source_replies"][0]["id"] = "opaque-stable-reply"
        self.assertEqual(self.project(row)["source_replies"][0]["id"], "opaque-stable-reply")
    def test_unknown_is_content_free_and_cannot_carry_private_fields(self):
        unknown = self.fixture["cross_caller_response"]["outcomes"][0]
        result = self.project(unknown)
        self.assertEqual(result["state"], "unknown")
        self.assertNotIn("source_replies", result)
        self.assertNotIn("installation_source", result)
        for key, value in [("source_replies", self.row["source_replies"]), ("source_replies_omitted", True)]:
            self.assertEqual(self.query({**unknown,key:value}), ("unavailable", {}))
    def test_retained_lost_ack_reply_keeps_identity_time_and_semantic_revision(self):
        self.assertTrue(self.fixture["checks"]["reload_preserved"])
        self.assertTrue(self.fixture["checks"]["stable_id_and_time"])
        first = self.project()
        # Recreate the received JSON and duplicate queue receipt, not a new request.
        row = json.loads(json.dumps(self.row))
        self.action["receipt"]["receipt"]["duplicate"] = True
        second = self.project(row, at="2026-11-11T05:40:04.434Z")
        self.assertEqual(first["revision"], second["revision"])
        self.assertEqual(first["source_replies"], second["source_replies"])
    def test_long_multibyte_reply_is_omitted_whole_with_separate_coverage(self):
        row = copy.deepcopy(self.row)
        row["source_replies"][0]["text"] = "界" * 3900 + " not completion"
        row["source_replies_omitted"] = True
        result = self.project(row)
        self.assertEqual(result["source_replies"], [])
        self.assertEqual(result["source_replies_omitted"], 1)
        self.assertIs(result["domain_source_replies_omitted"], True)
        self.assertNotIn("界", json.dumps(result, ensure_ascii=False))
        self.assertLessEqual(len(observer.encoded(result)), 16000)
        row["source_replies"][0]["text"] = "界" * 4001
        self.assertEqual(self.query(row), ("unavailable", {}))
        row = copy.deepcopy(self.row); row["source_replies_omitted"] = 1
        self.assertEqual(self.query(row), ("unavailable", {}))

if __name__ == "__main__":
    unittest.main()
