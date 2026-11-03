#!/usr/bin/env python3
"""A real stdio ACP peer for contract tests, with no model calls."""
import json
import os
import sys
import time
import uuid

def write(value):
    print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)

def ask_form(scope, withdraw=False):
    write({"id": "form-1", "method": "elicitation/create", "params": {
        **scope, "mode": "form", "message": "Choose the rollout approach.",
        "requestedSchema": {"type": "object", "properties": {
            "strategy": {"type": "string", "title": "Strategy", "enum": ["careful", "quick"], "default": "careful"},
            "note": {"type": "string", "title": "Note", "maxLength": 300},
            "count": {"type": "integer", "title": "Batch size", "minimum": 1, "maximum": 10, "default": 2},
        }, "required": ["strategy", "count"]}}})
    if withdraw:
        time.sleep(.15)
        write({"method": "$/cancel_request", "params": {"requestId": "form-1"}})
    response = json.loads(sys.stdin.readline())
    if os.environ.get("FIXTURE_LOG"):
        with open(os.environ["FIXTURE_LOG"], "a") as stream:
            stream.write(json.dumps(response) + "\n")
    return response.get("result", {})

authenticated=False
for line in sys.stdin:
    value = json.loads(line)
    if os.environ.get("FIXTURE_LOG"):
        with open(os.environ["FIXTURE_LOG"], "a") as stream:
            stream.write(json.dumps(value) + "\n")
    method, params = value.get("method"), value.get("params", {})
    if os.environ.get("FIXTURE_FORM_PHASE") == method:
        assert ask_form({"requestId": value["id"]})["action"] == "accept"
    if method == "initialize":
        write({"id": value["id"], "result": {"protocolVersion": 1, "authMethods": ([{"id":"browser","name":"Browser sign-in"},{"id":"terminal","type":"terminal","name":"Terminal"}] if os.environ.get("FIXTURE_AUTH") else []), "agentInfo": {"name": "Fixture individual", "version": "1"}, "agentCapabilities": {"loadSession": True, "mcpCapabilities": {"http": True}, "promptCapabilities": {"image": bool(os.environ.get("FIXTURE_IMAGES")), "embeddedContext": bool(os.environ.get("FIXTURE_EMBEDDED"))}}}})
    elif method == "authenticate":
        assert params["methodId"] == "browser"
        write({"id":"auth-url","method":"elicitation/create","params":{"requestId":value["id"],"mode":"url","elicitationId":"login-flow","url":"https://example.test/authorize","message":"Authorize the fixture agent."}})
        reply=json.loads(sys.stdin.readline())
        if os.environ.get("FIXTURE_LOG"):
            with open(os.environ["FIXTURE_LOG"],"a") as stream:stream.write(json.dumps(reply)+"\n")
        authenticated=reply.get("result",{}).get("action")=="accept"
        write({"id":value["id"],"result":{}} if authenticated else {"id":value["id"],"error":{"code":-32000,"message":"not authenticated"}})
    elif method in ("session/new", "session/load"):
        if os.environ.get("FIXTURE_AUTH") and not authenticated:
            write({"id":value["id"],"error":{"code":-32000,"message":"Sign-in required"}});continue
        write({"id": value["id"], "result": {"sessionId": params.get("sessionId", str(uuid.uuid4()))}})
    elif method == "session/prompt":
        sid = params["sessionId"]
        text = params["prompt"][0]["text"]
        if text == "wait forever":
            time.sleep(120)
        write({"method": "session/update", "params": {"sessionId": "wrong-session", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "WRONG CONVERSATION"}}}})
        write({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "tool_call", "toolCallId": "read", "title": "Reading authorized context", "status": "in_progress"}}})
        control_text = text
        try:
            history = json.loads(text.rsplit("\n", 1)[-1])
            if isinstance(history, list) and history: control_text = history[-1]["text"]
        except (ValueError, KeyError, TypeError): pass
        if "request form" in control_text:
            answer = ask_form({"sessionId": sid}, withdraw="withdraw form" in control_text)
            text = "Structured answer: " + answer.get("action", "cancel")
        if "request approval" in control_text:
            write({"id": "permission-1", "method": "session/request_permission", "params": {"sessionId": sid, "toolCall": {"toolCallId": "write", "title": "Write a fixture note", "kind": "edit", "rawInput": {"path": "fixture-only.txt"}}, "options": [{"optionId": "yes", "kind": "allow_once", "name": "Allow once"}, {"optionId": "no", "kind": "reject_once", "name": "Reject once"}]}})
            response = json.loads(sys.stdin.readline())
            if os.environ.get("FIXTURE_LOG"):
                with open(os.environ["FIXTURE_LOG"], "a") as stream:
                    stream.write(json.dumps(response) + "\n")
            assert response["result"]["outcome"]["outcome"] in ("cancelled", "selected")
        if "media proof" in control_text:
            write({"method":"session/update","params":{"sessionId":sid,"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="}}}})
        for chunk in (["[[PASS]]"] if os.environ.get("FIXTURE_AMBIENT_PASS") and "This is a group conversation" in text.split("Room participants:")[0] else ["Fixture response: ", text]):
            write({"method": "session/update", "params": {"sessionId": sid, "update": {"sessionUpdate": "agent_message_chunk", "messageId": "final", "content": {"type": "text", "text": chunk}}}})
        write({"id": value["id"], "result": {"stopReason": "end_turn"}})
    elif method:
        write({"id": value["id"], "error": {"code": -32601, "message": "Unsupported fixture request"}})
