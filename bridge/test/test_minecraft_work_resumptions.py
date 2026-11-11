"""PR89 recorded scheduling facts, using its actual pure producer query fixture."""
import copy
import hashlib
import json
import unittest
import test_minecraft_private_source_replies as private
BRIDGE, observer = private.BRIDGE, private.observer

FIXTURE = BRIDGE / "test/fixtures/minecraft-work-resumption.json"

class WorkResumptions(unittest.TestCase):
    query = private.PrivateSourceReplies.query
    project = private.PrivateSourceReplies.project

    def setUp(self):
        private.PrivateSourceReplies.setUp(self)
        self.producer = json.loads(FIXTURE.read_text())
        self.resumptions = self.producer["response"]["outcomes"][0]["work_resumptions"]
        self.row["work_resumptions"] = copy.deepcopy(self.resumptions)

    def test_actual_producer_facts_are_preserved_without_execution_or_completion_inference(self):
        self.assertEqual(hashlib.sha256(FIXTURE.read_bytes()).hexdigest(), "421ead394e043a9b54f13f06d925fa3b9ce64e66e92b90548c6f8d0442b6dafb")
        self.assertEqual(len(self.resumptions), 1)  # Another source was filtered by the producer.
        result = self.project()
        self.assertEqual(result["work_resumptions"], self.resumptions)
        self.assertTrue(result["work_resumptions"][0]["scheduled"])
        self.assertFalse(result["work_resumptions"][0]["execution_started"])
        self.assertEqual(result["installation_source"], self.row["source"])
        self.assertEqual(result["provenance"]["source_id"], self.queue)
        self.assertEqual(result["fulfillment"], "not_established")
        self.assertFalse(result["verified"])

    def test_optional_absence_unknown_and_boolean_omission_do_not_invent_facts(self):
        del self.row["work_resumptions"]
        self.assertNotIn("work_resumptions", self.project())
        self.assertNotIn("domain_work_resumptions_omitted", self.project())
        self.row["work_resumptions_omitted"] = True
        projected = self.project()
        self.assertNotIn("work_resumptions", projected)
        self.assertIs(projected["domain_work_resumptions_omitted"], True)
        unknown = self.fixture["cross_caller_response"]["outcomes"][0]
        self.assertNotIn("work_resumptions", self.project(unknown))
        self.assertEqual(self.query({**unknown, "work_resumptions":self.resumptions}), ("unavailable", {}))

    def test_malformed_records_and_coverage_fail_closed(self):
        for value in [None, {}, True, self.resumptions * 17]:
            self.assertEqual(self.query({**self.row,"work_resumptions":value}), ("unavailable", {}))
        for key,value in [("task_id",None),("task_id",""),("task_id","bad\nname"),
                          ("recorded_status",None),("recorded_status",""),("recorded_status","x"*161),
                          ("scheduled",1),("scheduled",None),("execution_started",0),("execution_started",None),
                          ("new_field","private text")]:
            row=copy.deepcopy(self.row);row["work_resumptions"][0][key]=value
            self.assertEqual(self.query(row), ("unavailable", {}), (key,value))
        for value in [1, 0, None, "true"]:
            self.assertEqual(self.query({**self.row,"work_resumptions_omitted":value}), ("unavailable", {}))

    def test_empty_additive_evidence_does_not_reannounce_an_unchanged_reply(self):
        old=copy.deepcopy(self.row);del old["work_resumptions"]
        old_projection=self.project(old)
        for coverage in [None,False]:
            current={**old,"work_resumptions":[]}
            if coverage is not None:
                current["work_resumptions_omitted"]=coverage
            projection=self.project(current)
            self.assertEqual(projection,old_projection)
        current["work_resumptions_omitted"]=True
        self.assertNotEqual(self.project(current)["revision"],old_projection["revision"])

    def test_existing_source_binding_remains_authoritative(self):
        for key,value in [("caller",{"id":"other","label":"Other"}),("call_id","other-call"),
                          ("revision","other-revision"),("actor",{"id":"other","label":"Other"}),
                          ("audience",{"kind":"private","experience":"companion","id":"other"})]:
            row=copy.deepcopy(self.row);row["source"][key]=value
            self.assertEqual(self.query(row), ("unavailable", {}), key)

    def test_semantic_revision_changes_only_when_source_facts_change(self):
        first=self.project()
        later=self.project(at="2026-11-11T06:40:00.000Z")
        self.assertEqual(first["revision"],later["revision"])
        self.row["work_resumptions"][0]["execution_started"]=True
        changed=self.project()
        self.assertNotEqual(first["revision"],changed["revision"])
        self.assertTrue(changed["work_resumptions"][0]["execution_started"])
        self.assertFalse(changed["verified"])
        self.assertEqual(changed["state"],first["state"])

    def combined_row(self):
        row=copy.deepcopy(self.row)
        row["work_resumptions"]=[{**self.resumptions[0],"task_id":str(i)+"界"*50,"recorded_status":"界"*50} for i in range(16)]
        row["work_resumptions_omitted"]=True
        row["responses"]=[{"id":"public-full","at":row["terminal_at"],"audience":"public","text":"Public record. " + "p"*1900}]
        row["source_replies"]=[{**row["source_replies"][0],"id":"private-short","text":"The request was recorded, not confirmed heard."},
                                {**row["source_replies"][0],"id":"private-long","text":"界"*3000+" STILL NOT COMPLETION"}]
        return row

    def test_combined_projection_preserves_small_private_reply_and_omits_long_reply_honestly(self):
        row=self.combined_row();original=copy.deepcopy(row)
        result=self.project(row)
        self.assertEqual(row,original)
        self.assertEqual(result["responses"],row["responses"])
        self.assertEqual(result["source_replies"],[row["source_replies"][0]])
        self.assertEqual(result["source_replies_omitted"],1)
        self.assertGreater(result["work_resumptions_omitted"],0)
        self.assertIs(result["domain_work_resumptions_omitted"],True)
        self.assertNotIn("STILL NOT COMPLETION",json.dumps(result,ensure_ascii=False))
        self.assertLessEqual(len(observer.encoded(result)),16000)
        expected=json.loads((BRIDGE/"test/fixtures/minecraft-work-resumption-projection.json").read_text())
        self.assertEqual(result,expected["observation"])
        self.assertEqual(self.action,expected["action"])

    def test_resumption_projection_is_bounded_whole_and_coverage_separate(self):
        self.row["work_resumptions"]=[{**self.resumptions[0],"task_id":str(i)+"界"*50,"recorded_status":"界"*50} for i in range(16)]
        self.row["work_resumptions_omitted"]=True
        result=self.project()
        self.assertGreater(len(result["work_resumptions"]),0)
        self.assertLess(len(result["work_resumptions"]),16)
        self.assertEqual(result["work_resumptions_omitted"],16-len(result["work_resumptions"]))
        self.assertIs(result["domain_work_resumptions_omitted"],True)
        for row in result["work_resumptions"]:
            self.assertIn(row,self.row["work_resumptions"])
        self.assertLessEqual(len(observer.encoded(result)),16000)

if __name__ == "__main__":
    unittest.main()
