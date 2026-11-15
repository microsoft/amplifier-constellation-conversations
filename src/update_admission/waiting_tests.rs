//! Real retained DB rows drive the advisory classification. No app, manager,
//! provider, clock adjustment or normalization is used by these tests.
use super::*;

fn fixture() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE conversations(id TEXT PRIMARY KEY,status TEXT,shared INTEGER);
         CREATE TABLE agent_endpoints(id TEXT PRIMARY KEY);
         CREATE TABLE room_agents(conversation_id TEXT,agent_id TEXT);
         CREATE TABLE messages(id TEXT PRIMARY KEY,conversation_id TEXT,agent_id TEXT,role TEXT,text TEXT,status TEXT);
         CREATE TABLE input_deliveries(id TEXT PRIMARY KEY,message_id TEXT,conversation_id TEXT,agent_id TEXT,state TEXT,target_turn TEXT,mode TEXT,session_id TEXT,endpoint TEXT,receipt TEXT);
         CREATE TABLE companion_calls(id TEXT PRIMARY KEY,conversation_id TEXT,agent_id TEXT,state TEXT);
         CREATE TABLE companion_tool_grants(call_id TEXT,port INTEGER);
         CREATE TABLE companion_delegations(call_id TEXT,id TEXT,message_id TEXT,state TEXT);
         CREATE TABLE companion_actions(call_id TEXT,request_id TEXT,delegation_id TEXT,connection_id TEXT,state TEXT,result TEXT,source TEXT);
         CREATE TABLE companion_events(call_id TEXT,kind TEXT,data TEXT);
         CREATE TABLE companion_action_observers(call_id TEXT,request_id TEXT,connection_id TEXT);
         CREATE TABLE companion_observations(call_id TEXT,request_id TEXT,data TEXT);
         INSERT INTO conversations VALUES('room','idle',1);
         INSERT INTO agent_endpoints VALUES('agent');
         INSERT INTO room_agents VALUES('room','agent');
         INSERT INTO messages VALUES('input','room',NULL,'user','retained request','complete');
         INSERT INTO companion_calls VALUES('call','room','agent','connected');",
    ).unwrap();
    migrate(&db).unwrap();
    db
}

fn input(db: &Connection, state: &str) {
    db.execute("INSERT INTO input_deliveries VALUES('delivery','input','room','agent',?1,NULL,NULL,NULL,NULL,NULL)", [state]).unwrap();
}

fn assert_busy(db: &mut Connection, expected_waiting: bool) -> Value {
    let before = serde_json::to_value(busy(db).unwrap()).unwrap();
    let changes = db.total_changes();
    let observed = status_db(db).unwrap();
    assert_eq!(
        db.total_changes(),
        changes,
        "classification must be read-only"
    );
    assert_eq!(observed["busy"], before);
    assert_eq!(observed["waiting_for_work"], expected_waiting);
    let result = prepare_db(db, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(result["busy"], before, "no existing blocker may be removed");
    assert_eq!(result["ready"], false);
    assert_eq!(result["closed"], false);
    assert_eq!(result["reason"], "busy");
    assert_eq!(result["waiting_for_work"], expected_waiting);
    assert_eq!(
        db.query_row("SELECT count(*) FROM native_update_admission", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    result
}

#[test]
fn active_call_tool_grant_and_known_input_wait_without_private_room_restriction() {
    let mut db = fixture();
    db.execute("INSERT INTO companion_tool_grants VALUES('call',41235)", [])
        .unwrap();
    input(&db, "received");
    let result = assert_busy(&mut db, true);
    assert_eq!(
        result["busy"],
        json!([
            {"kind":"active_calls","count":1},
            {"kind":"retained_tool_servers","count":1},
            {"kind":"pending_or_uncertain_inputs","count":1}
        ])
    );
    assert_eq!(
        db.query_row("SELECT state FROM input_deliveries", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "received"
    );
    db.execute_batch("UPDATE companion_calls SET state='closed'; UPDATE companion_tool_grants SET port=NULL; UPDATE input_deliveries SET state='completed';").unwrap();
    let ready = prepare_db(&mut db, &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(
        ready["ready"], true,
        "only the unchanged busy predicate admits idle"
    );
    assert_eq!(ready["waiting_for_work"], false);
}

#[test]
fn closed_call_ports_unknown_inputs_and_missing_associations_need_attention() {
    for mutation in [
        "UPDATE companion_calls SET state='closed'",
        "UPDATE companion_calls SET state='interrupted'",
        "UPDATE companion_tool_grants SET call_id='missing'",
        "UPDATE companion_tool_grants SET port=0",
        "DELETE FROM room_agents",
        "DELETE FROM agent_endpoints",
        "UPDATE input_deliveries SET state='unknown'",
        "UPDATE input_deliveries SET state='ack_pending'",
        "UPDATE input_deliveries SET state='notAccepted'",
        "UPDATE input_deliveries SET message_id='missing'",
        "INSERT INTO native_update_activity VALUES('uncertain','configuration',1)",
    ] {
        let mut db = fixture();
        db.execute("INSERT INTO companion_tool_grants VALUES('call',41235)", [])
            .unwrap();
        input(&db, "received");
        db.execute_batch(mutation).unwrap();
        assert_busy(&mut db, false);
    }
}

#[test]
fn current_input_turns_wait_but_interrupted_or_cross_room_targets_do_not() {
    for state in [
        "preparing",
        "offering",
        "submitted",
        "received_remote",
        "remoteQueued",
        "incorporated",
    ] {
        let mut db = fixture();
        input(&db, state);
        db.execute_batch("UPDATE conversations SET status='working'; INSERT INTO messages VALUES('answer','room','agent','assistant','','streaming'); UPDATE input_deliveries SET target_turn='answer',mode='prompt';").unwrap();
        assert_busy(&mut db, true);
        db.execute(
            "UPDATE messages SET status='interrupted' WHERE id='answer'",
            [],
        )
        .unwrap();
        assert_busy(&mut db, false);
        db.execute(
            "UPDATE messages SET status='streaming',conversation_id='other-room' WHERE id='answer'",
            [],
        )
        .unwrap();
        assert_busy(&mut db, false);
    }
    // driver::prompt commits the local preparing intent before its target row.
    let mut db = fixture();
    input(&db, "preparing");
    db.execute(
        "UPDATE input_deliveries SET target_turn='not-inserted-yet',mode='prompt'",
        [],
    )
    .unwrap();
    assert_busy(&mut db, true);
    db.execute(
        "UPDATE input_deliveries SET session_id='possibly-submitted'",
        [],
    )
    .unwrap();
    assert_busy(&mut db, false);
}

#[test]
fn delegation_intent_and_post_hangup_current_input_wait_but_unknown_or_stale_do_not() {
    for state in ["waiting_transcript", "dispatched"] {
        let mut db = fixture();
        db.execute(
            "INSERT INTO companion_delegations VALUES('call','delegation','not-yet-inserted',?1)",
            [state],
        )
        .unwrap();
        assert_busy(&mut db, true);
        db.execute("UPDATE companion_delegations SET state='unknown'", [])
            .unwrap();
        assert_busy(&mut db, false);
    }
    let mut db = fixture();
    db.execute_batch("UPDATE companion_calls SET state='closed'; INSERT INTO companion_delegations VALUES('call','delegation','input','dispatched');").unwrap();
    input(&db, "received");
    assert_busy(&mut db, true);
    db.execute("UPDATE input_deliveries SET state='unknown'", [])
        .unwrap();
    assert_busy(&mut db, false);
    db.execute("DELETE FROM input_deliveries", []).unwrap();
    assert_busy(&mut db, false);
    db.execute("UPDATE companion_delegations SET call_id='missing'", [])
        .unwrap();
    assert_busy(&mut db, false);
}

fn action_fixture(state: &str) -> Connection {
    let db = fixture();
    db.execute_batch("UPDATE companion_calls SET state='closed'; INSERT INTO companion_delegations VALUES('call','delegation','input','completed');").unwrap();
    let receipt = json!({"request_id":"request","state":state});
    let source = json!({"call_id":"call","delegation_id":"delegation","message_id":"input","conversation_id":"room"});
    db.execute(
        "INSERT INTO companion_actions VALUES('call','request','delegation','connection',?1,?2,?3)",
        params![state, receipt.to_string(), source.to_string()],
    )
    .unwrap();
    db
}

#[test]
fn accepted_actions_wait_after_hangup_without_new_observer_eligibility() {
    for state in ["queued", "accepted"] {
        let mut db = action_fixture(state);
        assert_busy(&mut db, true);
        db.execute(
            "INSERT INTO companion_observations VALUES('call','request',?1)",
            [json!({"request_id":"request","state":"running"}).to_string()],
        )
        .unwrap();
        assert_busy(&mut db, true);
        db.execute(
            "UPDATE companion_observations SET data=?1",
            [json!({"request_id":"request","state":"unknown"}).to_string()],
        )
        .unwrap();
        assert_busy(&mut db, false);
    }
}

#[test]
fn unacknowledged_actions_and_changed_source_receipt_or_observation_need_attention() {
    for state in ["submitting", "unknown"] {
        assert_busy(&mut action_fixture(state), false);
    }
    for mutation in [
        "UPDATE companion_actions SET result='{}'",
        "UPDATE companion_actions SET result='not-json'",
        "UPDATE companion_actions SET result=json_set(result,'$.request_id','another')",
        "UPDATE companion_actions SET source=json_set(source,'$.message_id','another')",
        "UPDATE companion_actions SET source=json_set(source,'$.call_id','another')",
        "UPDATE companion_actions SET connection_id=''",
        "UPDATE companion_delegations SET state='unknown'",
        "UPDATE companion_delegations SET call_id='another'",
        "DELETE FROM messages",
        "INSERT INTO companion_observations VALUES('call','request','{\"request_id\":\"another\",\"state\":\"running\"}')",
        "INSERT INTO companion_observations VALUES('call','request','{\"request_id\":\"request\",\"state\":\"needs_review\"}')",
    ] {
        let mut db = action_fixture("accepted");
        db.execute_batch(mutation).unwrap();
        assert_busy(&mut db, false);
    }
}

#[test]
fn advisory_read_failure_does_not_change_busy_result_or_release_prior_gate() {
    let mut db = fixture();
    db.execute_batch("DELETE FROM companion_calls; DROP TABLE room_agents;")
        .unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
    db.execute(
        "INSERT INTO companion_calls VALUES('call','room','agent','connected')",
        [],
    )
    .unwrap();
    let result = prepare_db(&mut db, &transaction).unwrap();
    assert_eq!(result["waiting_for_work"], false);
    assert_eq!(result["ready"], false);
    assert_eq!(result["closed"], true);
    assert_eq!(result["transaction_id"], transaction);
    assert_eq!(result["busy"], json!([{"kind":"active_calls","count":1}]));
}
