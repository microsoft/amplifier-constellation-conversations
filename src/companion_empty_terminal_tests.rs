//! Real stdio ACP terminal handling and its existing Companion observation path.
//! The subprocess is deterministic: no provider, HTTP service, or world effects.
use super::*;
use crate::huddle_delivery::tests::{F, fixture, queued};

fn terminal_fixture(terminal: &str, text: &str, tool_only: bool) -> F {
    let f = fixture();
    let script = f.s.home.join("terminal.py");
    std::fs::write(&script, r#"import json, os, pathlib, sys
def send(value): print(json.dumps({'jsonrpc':'2.0', **value}), flush=True)
for line in sys.stdin:
    value = json.loads(line)
    with open(os.environ['FIXTURE_LOG'], 'a') as log: log.write(json.dumps(value) + '\n')
    method, rid = value.get('method'), value.get('id')
    if method == 'initialize':
        send({'id':rid, 'result':{'protocolVersion':1, 'agentCapabilities':{'loadSession':True, 'mcpCapabilities':{'http':True}}}})
    elif method in ('session/new', 'session/load'):
        send({'id':rid, 'result':{'sessionId':value['params'].get('sessionId', 'fixture-new-session')}})
    elif method == 'session/prompt':
        sid = value['params']['sessionId']
        terminal = os.environ['TERMINAL']
        if os.environ['TOOL_ONLY'] == '1':
            for status in ('in_progress', 'completed'):
                send({'method':'session/update', 'params':{'sessionId':sid, 'update':{'sessionUpdate':'tool_call' if status == 'in_progress' else 'tool_call_update', 'toolCallId':'fixture-read', 'title':'Read-only fixture result', 'status':status}}})
        if os.environ['REPLY']:
            send({'method':'session/update', 'params':{'sessionId':sid, 'update':{'sessionUpdate':'agent_message_chunk', 'content':{'type':'text', 'text':os.environ['REPLY']}}}})
        if terminal == 'unresolved_callback':
            send({'id':'pending-form', 'method':'elicitation/create', 'params':{'sessionId':sid, 'mode':'form', 'message':'Unresolved fixture question', 'requestedSchema':{'type':'object', 'properties':{'answer':{'type':'string'}}}}})
        if terminal == 'unknown_receipt':
            input_id = pathlib.Path(__file__).with_name('input-id').read_text()
            send({'method':'session/update', 'params':{'sessionId':sid, 'update':{'sessionUpdate':'tool_call_update', 'toolCallId':'fixture-read', 'status':'completed', '_meta':{'io.github.bkrabach.agent-runtime':{'inputReceipt':{'inputId':input_id, 'state':'unknown'}}}}}})
        if terminal == 'disconnect': sys.exit(0)
        elif terminal == 'rpc_error': send({'id':rid, 'error':{'code':-32603, 'message':'Fixture rejection'}})
        elif terminal == 'missing': send({'id':rid, 'result':{}})
        else: send({'id':rid, 'result':{'stopReason':'end_turn' if terminal in ('unresolved_callback', 'unknown_receipt') else terminal}})
    elif method and rid is not None:
        send({'id':rid, 'error':{'code':-32601, 'message':'Unsupported fixture method'}})
"#).unwrap();
    let mut agent = f.s.agent("default").unwrap().unwrap();
    agent.args = vec![script.display().to_string()];
    agent.env.insert("TERMINAL".into(), terminal.into());
    agent.env.insert("REPLY".into(), text.into());
    agent
        .env
        .insert("TOOL_ONLY".into(), if tool_only { "1" } else { "0" }.into());
    let config = serde_json::to_string(&agent).unwrap();
    let endpoint = auth::digest(&config);
    {
        let db = f.s.db.lock().unwrap();
        db.execute(
            "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
            [&config],
        )
        .unwrap();
        db.execute(
            "UPDATE room_agents SET endpoint=?2 WHERE conversation_id=?1",
            params![f.parent, endpoint],
        )
        .unwrap();
        db.execute(
            "UPDATE companion_calls SET endpoint_hash=?2 WHERE id=?1",
            params![f.call, endpoint],
        )
        .unwrap();
    }
    f
}

fn human(f: &F, participation: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let message = uuid::Uuid::new_v4().to_string();
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','A fresh independent request','complete',?3,?3,'owner')", params![message,f.parent,now_ms()]).unwrap();
    db.execute(
        "INSERT INTO input_authority(message_id,browser_hash) VALUES(?1,'browser')",
        [&message],
    )
    .unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?1,?2,?3,'default',?4,'received',?5)", params![id,message,f.parent,participation,now_ms()]).unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,20,'dispatched',?3,?4)", params![f.call,id,message,now_ms()]).unwrap();
    id
}

fn next(f: &F) -> crate::delivery::Input {
    crate::delivery::next(&f.s.db.lock().unwrap(), &f.parent, None)
        .unwrap()
        .unwrap()
}

fn outcome(f: &F, input: &str) -> (String, String, String, String) {
    f.s.db.lock().unwrap().query_row("SELECT d.state,m.status,m.text,m.id FROM input_deliveries d JOIN messages m ON m.id=d.target_turn WHERE d.id=?1", [input], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
}

fn frames(f: &F) -> Vec<Value> {
    std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn no_card_or_voice(f: &F) {
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='card'",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1 AND semantic_key NOT LIKE 'request-pending-%'",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn successful_empty_peer_terminal_settles_and_does_not_block_next_human() {
    let f = terminal_fixture("end_turn", "", true);
    let (post, peer) = queued(&f);
    let input = next(&f);
    assert_eq!(input.id, peer);
    assert_eq!(input.actor, "Peer");
    let later = human(&f, "direct");
    let result = crate::driver::deliver(f.s.clone(), input).await;
    assert!(result.is_ok(), "successful empty peer terminal: {result:?}");
    let (state, status, text, target) = outcome(&f, &peer);
    assert_eq!(
        (state.as_str(), status.as_str(), text.as_str()),
        ("completed", "complete", "")
    );
    assert!(crate::huddle_delivery::quiet_turn(&f.s.db.lock().unwrap(), &target).unwrap());
    observe_results(&f.s, &f.call, &f.parent, true).unwrap();
    no_card_or_voice(&f);
    let fresh = next(&f);
    assert_eq!(fresh.id, later);
    crate::driver::deliver(f.s.clone(), fresh).await.unwrap();
    observe_results(&f.s, &f.call, &f.parent, true).unwrap();
    assert_eq!(outcome(&f, &later).0, "completed");
    no_card_or_voice(&f);
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT state FROM input_deliveries WHERE id='original-input'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "completed"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM input_deliveries WHERE message_id=?1",
            [&post],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM audit WHERE kind='turn.interrupted'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    let log = frames(&f);
    assert_eq!(
        log.iter().filter(|v| v["method"] == "session/new").count(),
        0
    );
    assert_eq!(
        log.iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        2
    );
    assert!(
        log.iter()
            .filter(|v| v["method"] == "session/load")
            .all(|v| v["params"]["sessionId"] == "original-session")
    );
}

#[tokio::test]
async fn successful_empty_direct_and_ambient_terminals_preserve_identity_without_fabricated_reply()
{
    for (participation, text, tool_only) in [
        ("direct", "", false),
        ("direct", " \n\t", true),
        ("ambient", "", false),
    ] {
        let f = terminal_fixture("end_turn", text, tool_only);
        let id = human(&f, participation);
        let result = crate::driver::deliver(f.s.clone(), next(&f)).await;
        assert!(result.is_ok(), "{participation} {text:?}: {result:?}");
        let (state, status, retained, target) = outcome(&f, &id);
        assert_eq!(
            (state.as_str(), status.as_str(), retained.as_str()),
            ("completed", "complete", text)
        );
        observe_results(&f.s, &f.call, &f.parent, true).unwrap();
        no_card_or_voice(&f);
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT state FROM companion_delegations WHERE call_id=?1 AND id=?2",
                params![f.call, id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "completed"
        );
        let data: String = db
            .query_row(
                "SELECT data FROM companion_events WHERE call_id=?1 AND event_key=?2",
                params![f.call, format!("result:{target}")],
                |r| r.get(0),
            )
            .unwrap();
        let data: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["text"], text);
        assert_eq!(data["silent"], true);
    }
}

#[tokio::test]
async fn textless_failure_or_unresolved_callback_stays_unknown_and_never_replays() {
    for terminal in [
        "cancelled",
        "refusal",
        "max_tokens",
        "max_turn_requests",
        "missing",
        "rpc_error",
        "disconnect",
        "unresolved_callback",
    ] {
        let f = terminal_fixture(terminal, "", true);
        let (_, peer) = queued(&f);
        let input = next(&f);
        let later = human(&f, "direct");
        let result = crate::driver::deliver(f.s.clone(), input).await;
        assert!(result.is_err(), "{terminal} must not become success");
        let (state, status, text, _) = outcome(&f, &peer);
        assert_eq!(
            (state.as_str(), status.as_str(), text.as_str()),
            ("unknown", "interrupted", ""),
            "{terminal}"
        );
        observe_results(&f.s, &f.call, &f.parent, false).unwrap();
        let db = f.s.db.lock().unwrap();
        assert!(
            crate::delivery::next(&db, &f.parent, None)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM input_deliveries WHERE id=?1",
                [later],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "received"
        );
        drop(db);
        assert_eq!(
            frames(&f)
                .iter()
                .filter(|v| v["method"] == "session/prompt")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn successful_empty_terminal_does_not_override_explicit_unknown_input_receipt() {
    let f = terminal_fixture("unknown_receipt", "", true);
    let id = human(&f, "direct");
    std::fs::write(f.s.home.join("input-id"), &id).unwrap();
    let input = next(&f);
    let later = human(&f, "direct");
    crate::driver::deliver(f.s.clone(), input).await.unwrap();
    let (state, status, text, _) = outcome(&f, &id);
    assert_eq!(
        (state.as_str(), status.as_str(), text.as_str()),
        ("unknown", "complete", "")
    );
    observe_results(&f.s, &f.call, &f.parent, false).unwrap();
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind IN ('card','result')",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    // The existing uncertainty notice is allowed; there must be no successful
    // result or invented answer simply because the ACP prompt ended normally.
    let updates: String = db
        .query_row(
            "SELECT updates FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get(0),
        )
        .unwrap();
    assert!(updates.contains("result is not confirmed"));
    assert!(
        crate::delivery::next(&db, &f.parent, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM input_deliveries WHERE id=?1",
            [&later],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "received"
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM companion_delegations WHERE call_id=?1 AND id=?2",
            params![f.call, id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unknown"
    );
    drop(db);
    assert_eq!(
        frames(&f)
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
}
