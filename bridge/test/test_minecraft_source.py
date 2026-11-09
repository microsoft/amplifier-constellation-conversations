"""Dedicated installation-input protocol: disposable files/transports only."""
import copy
import datetime
import hashlib
import importlib.util
import io
import json
import pathlib
import tempfile
import unittest
import uuid

BRIDGE = pathlib.Path(__file__).resolve().parents[1]
def module(name, file):
    spec = importlib.util.spec_from_file_location(name, BRIDGE / file)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value
instruction = module("source_instruction", "minecraft-instruction.py")
observer = module("source_observer", "minecraft-observe.py")
TOKEN = "scoped_fixture_credential_never_project_123456"
OWNER_TOKEN = "owner_fixture_credential_1234567890"
NOW = "2026-11-09T16:00:00.000Z"

class Transport:
    def __init__(self, responses):
        self.responses = responses
        self.calls = []
    def open(self, request, timeout):
        self.calls.append(request)
        result = self.responses[len(self.calls) - 1]
        if isinstance(result, Exception):
            raise result
        return io.BytesIO(json.dumps(result).encode())

class ScopedTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="scoped-consumer-")
        self.addCleanup(tmp.cleanup)
        self.home = pathlib.Path(tmp.name)
        self.installation = {"username":"Coach", "control_port":12345, "token":OWNER_TOKEN}
        (self.home / "config.json").write_text(json.dumps(self.installation))
        (self.home / "caller-token").write_text(TOKEN)
        self.config = self.home / "source-client.json"
        self.config.write_text(json.dumps({"version":1,"caller_id":"companion","token_file":str(self.home / "caller-token")}))
        self.rid, self.queue = str(uuid.uuid4()), str(uuid.uuid4())
        original = "Original human words.\nPlease explain the result."
        self.source = {"time_basis":"retained_human_transcript","original_text":original,"source_revision":hashlib.sha256(original.encode()).hexdigest(),
            "actor":"authenticated-human","audience":"private","call_id":"original-call","delegation_id":"original-intent",
            "conversation_id":"original-private-room","occurred_at":1794240000000}
        self.value = {"version":1,"request_id":self.rid,"instruction":"MODEL PARAPHRASE MUST NOT BE SUBMITTED","source":self.source}
        self.wire = instruction.source_envelope(self.rid, self.source)
        self.receipt = {"queued":True,"request_id":self.queue,"source_request_id":self.rid,"received_at":NOW,"duplicate":False}
        self.transport = Transport([instruction.SOURCE_CAPABILITY,self.receipt])
        self.result = instruction.submit(self.home, self.value, self.transport, self.config)
        self.action = {"request_id":self.rid,"receipt":self.result}
        self.row = {"request_id":self.queue,"delivery":"terminal","admitted_at":NOW,"terminal_at":NOW,
            "conversation_only":True,"turn_task_id":None,"blocked_reason":None,
            "responses":[{"id":"public-packet","at":NOW,"text":"The path is clear; I have not resumed construction.","audience":"public"}],
            "task_authorizations":[],"completion":"not_established","source":{**self.wire,"caller":{"id":"companion","label":"Companion"}},
            "response_association":"shared_turn","private_game_responses_included":False,"private_responses_withheld":True}
        self.client = instruction.source_client(self.config,self.installation)
        self.requests=[]
    def query(self, row=None, capability=None, actions=None):
        def exchange(method,path,payload,token):
            self.requests.append((method,path,payload,token))
            self.assertEqual(token,TOKEN)
            if path == "/source-capabilities":
                return instruction.SOURCE_CAPABILITY if capability is None else capability
            self.assertEqual((method,path),("POST","/source-outcomes"))
            return {"version":1,"observed_at":NOW,"outcomes":[self.row if row is None else row]}
        return observer.read_scoped_outcomes(exchange,[self.action] if actions is None else actions,self.client)
    def test_submission_uses_exact_human_source_with_dedicated_credential(self):
        self.assertEqual(self.result["state"],"queued")
        self.assertEqual(self.result["protocol"],"installation_inputs.v1")
        self.assertEqual(self.result["source"],self.wire)
        self.assertEqual(self.result["receipt"],self.receipt)
        self.assertEqual([(r.get_method(),r.full_url.rsplit('/',1)[-1]) for r in self.transport.calls],[("GET","source-capabilities"),("POST","task")])
        for request in self.transport.calls:
            self.assertEqual(request.get_header("Authorization"),"Bearer "+TOKEN)
        self.assertEqual(json.loads(self.transport.calls[-1].data),{"text":self.source["original_text"],"source":self.wire})
        self.assertEqual(self.wire["actor"],{"id":"authenticated-human","label":"authenticated-human"})
        self.assertEqual(self.wire["audience"],{"kind":"private","experience":"companion","id":"original-private-room"})
        self.assertNotIn("MODEL PARAPHRASE",self.transport.calls[-1].data.decode())
        self.assertNotIn(TOKEN,json.dumps(self.result))
    def test_capability_failure_never_submits_or_falls_back(self):
        for caps in [{}, {**instruction.SOURCE_CAPABILITY,"private_game_responses":True},
                     {**instruction.SOURCE_CAPABILITY,"version":True},
                     {**instruction.SOURCE_CAPABILITY,"outcomes":{"method":"GET","path":"/input-outcomes","max_items":16}},TimeoutError(TOKEN)]:
            transport=Transport([caps])
            result=instruction.submit(self.home,self.value,transport,self.config)
            self.assertEqual(result["state"],"failed")
            self.assertEqual(len(transport.calls),1)
            self.assertNotIn(TOKEN,json.dumps(result))
    def test_uncertain_submission_and_wrong_receipt_never_retry(self):
        for response in [TimeoutError(TOKEN),{**self.receipt,"source_request_id":str(uuid.uuid4())},
                         {**self.receipt,"duplicate":0},{**self.receipt,"received_at":"yesterday"}]:
            transport=Transport([instruction.SOURCE_CAPABILITY,response])
            result=instruction.submit(self.home,self.value,transport,self.config)
            self.assertEqual(result["state"],"unknown")
            self.assertEqual(len(transport.calls),2)
            self.assertNotIn(TOKEN,json.dumps(result))
    def test_missing_or_forged_original_source_never_dispatches(self):
        for key,value in [("source_revision","changed"),("original_text","Changed words"),("actor",{"label":"model"}),
                          ("time_basis","delegation_created_at"),("time_basis",None),("audience","public"),("occurred_at",True),("call_id",None),("conversation_id",None)]:
            transport=Transport([])
            changed={**self.value,"source":{**self.source,key:value}}
            self.assertEqual(instruction.submit(self.home,changed,transport,self.config)["state"],"failed")
            self.assertFalse(transport.calls)
    def test_dedicated_credentials_cannot_alias_broad_token(self):
        (self.home/"caller-token").write_text(OWNER_TOKEN)
        transport=Transport([])
        self.assertEqual(instruction.submit(self.home,self.value,transport,self.config)["state"],"failed")
        self.assertFalse(transport.calls)
    def test_scoped_outcome_preserves_binding_and_shared_turn_not_fulfillment(self):
        availability,rows=self.query()
        self.assertEqual(availability,"available")
        self.assertEqual(self.requests[-1][2],{"version":1,"requests":[{"request_id":self.queue,"source":self.wire}]})
        result=observer.outcome_observation(self.action,"minecraft:coach",NOW,rows,availability)
        self.assertEqual(result["state"],"settled")
        self.assertEqual(result["fulfillment"],"not_established")
        self.assertFalse(result["conversation"]["reply_verified"])
        self.assertEqual(result["installation_source"],self.row["source"])
        self.assertEqual(result["domain_response_association"],"shared_turn")
        self.assertNotIn("source_ids",result)
        self.assertTrue(result["domain_private_responses_withheld"])
        self.assertFalse(result["private_game_responses_included"])
    def test_other_actor_call_audience_caller_and_source_changes_never_disclose(self):
        changes=[("actor",{"id":"other","label":"other"}),("audience",{**self.wire["audience"],"id":"other-room"}),
            ("call_id","other-call"),("intent_id","other-intent"),("revision","other-revision"),
            ("occurred_at","2026-11-09T16:00:01.000Z"),("request_id",str(uuid.uuid4())),("caller",{"id":"other-caller","label":"Other"})]
        for key,value in changes:
            row=copy.deepcopy(self.row);row["source"][key]=value
            availability,rows=self.query(row)
            self.assertEqual((availability,rows),("unavailable",{}),key)
            result=observer.outcome_observation(self.action,"minecraft:coach",NOW,rows,availability)
            self.assertEqual(result["state"],"unknown")
            self.assertNotIn("responses",result)
    def test_unauthorized_unknown_is_not_fabricated_source_or_terminal(self):
        unknown={"request_id":self.queue,"delivery":"unknown","reason":"not_retained_or_not_authorized","completion":"not_established"}
        availability,rows=self.query(unknown)
        self.assertEqual(availability,"available")
        result=observer.outcome_observation(self.action,"minecraft:coach",NOW,rows,availability)
        self.assertEqual(result["state"],"unknown")
        self.assertNotIn("installation_source",result)
        self.assertNotIn("responses",result)
    def test_private_packets_and_malformed_scoped_flags_fail_closed(self):
        for key,value in [("private_game_responses_included",True),("private_game_responses_included",0),
                          ("response_association","answered"),("private_responses_withheld",1),("responses_omitted","false"),
                          ("source_ids",[self.queue,"another-source"])]:
            row={**self.row,key:value}
            self.assertEqual(self.query(row),("unavailable",{}),key)
        row=copy.deepcopy(self.row);row["responses"][0]["audience"]="private";row["responses"][0]["text"]="PRIVATE_SENTINEL"
        self.assertEqual(self.query(row),("unavailable",{}))
    def test_missing_client_duplicate_or_changed_receipt_never_uses_broad_fallback(self):
        self.assertEqual(observer.read_scoped_outcomes(None,[self.action],None),("not_configured",{}))
        self.assertEqual(self.query(actions=[self.action,self.action]),("unavailable",{}))
        self.assertFalse(self.requests)
        for key,value in [("source_request_id",str(uuid.uuid4())),("queued",False)]:
            action=copy.deepcopy(self.action);action["receipt"]["receipt"][key]=value
            self.assertEqual(self.query(actions=[action]),("unavailable",{}))
        self.assertFalse(self.requests)
    def test_full_observer_reads_scoped_results_but_never_operator_history(self):
        config=self.home/"observer.json"
        config.write_text(json.dumps({"minecraft_home":str(self.home),"connection_id":"minecraft:coach","expected_username":"Coach","source_config":str(self.config)}))
        (self.home/"workshop.json").write_text(json.dumps({"task":None,"events":[],"task_history":[]}))
        parent=self
        class Reader:
            def open(self,request,timeout):
                parent.requests.append(request)
                path=request.full_url.rsplit('/',1)[-1]
                token=request.get_header("Authorization")
                if path=="installation-state":
                    parent.assertEqual(token,"Bearer "+OWNER_TOKEN);result={"task":None,"ready":True}
                elif path=="source-capabilities":
                    parent.assertEqual(token,"Bearer "+TOKEN);result=instruction.SOURCE_CAPABILITY
                elif path=="source-outcomes":
                    parent.assertEqual(token,"Bearer "+TOKEN)
                    parent.assertEqual(request.get_method(),"POST")
                    parent.assertEqual(json.loads(request.data),{"version":1,"requests":[{"request_id":parent.queue,"source":parent.wire}]})
                    result={"version":1,"observed_at":NOW,"outcomes":[parent.row]}
                else: raise AssertionError("Unexpected broad lookup: "+path)
                return io.BytesIO(json.dumps(result).encode())
        result=observer.observe(config,{"version":1,"actions":[self.action]},Reader(),NOW)
        self.assertEqual(result["availability"],"available")
        self.assertEqual(result["actions"][0]["state"],"settled")
        self.assertEqual(result["coverage"]["installation_input_outcomes"],"available")
        self.assertEqual(len(self.requests),3)
        self.assertNotIn(TOKEN,json.dumps(result))
        self.assertNotIn(OWNER_TOKEN,json.dumps(result))

if __name__ == "__main__":
    unittest.main()
