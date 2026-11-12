//! Actual application intake under the durable native-update gate. No provider,
//! ACP process, listening socket, or model is started by these tests.
use agent_conversations::{
    config::{Agent, Config},
    server,
    state::AppState,
    update_admission,
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

fn fixture() -> (TempDir, Arc<AppState>, String) {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Update fixture".into(),
        owner: "owner".into(),
        allowed_users: vec![],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://example.test".into(),
        pam_service: "agent-conversations".into(),
        tls_cert: home.path().join("cert.pem"),
        tls_key: home.path().join("key.pem"),
        tls_ca: None,
        import_source: "fixture".into(),
        import_surface: "fixture".into(),
        auth_mode: Default::default(),
        transport: Default::default(),
        agent: Some(Agent {
            label: "Existing agent".into(),
            command: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec![],
            cwd: home.path().to_string_lossy().into_owned(),
            session_cwd: None,
            env: BTreeMap::new(),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        }),
    }
    .save(home.path())
    .unwrap();
    let s = AppState::open(home.path().to_owned()).unwrap();
    let admin = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    (home, s, admin)
}

#[test]
fn later_source_bound_outcome_settles_queued_receipt_without_rewriting_or_claiming_success() {
    let (home, s, _) = fixture();
    let room = s
        .create_room("Retained private call", "owner", &["default".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = uuid::Uuid::new_v4().to_string();
    let queue = uuid::Uuid::new_v4().to_string();
    let text = "Inspect the existing request.";
    let source = json!({"call_id":"closed-call","conversation_id":room,"actor":"owner","audience":"private","delegation_id":"delegation","message_id":"input","occurred_at":100,"original_text":text,"source_revision":agent_conversations::auth::digest(text)});
    let receipt = json!({"request_id":request,"state":"queued","scope":"private_operator_instruction","receipt":{"request_id":queue,"queued":true}});
    let observation = json!({"request_id":request,"revision":"source-revision","observed_at":"2026-11-12T00:00:00Z","scope":"conversation","state":"settled","verified":false,"delivery_state":"terminal","fulfillment":"not_established","conversation":{"conversation_only":true},"responses":[],"provenance":{"kind":"minecraft_input_outcome","version":1,"source_id":queue}});
    {
        let db = s.db.lock().unwrap();
        let endpoint =
            agent_conversations::auth::digest(&serde_json::to_string(&s.config.agent).unwrap());
        db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES('closed-call','owner','call-request','hash','default',?1,'closed',100,100,?2)",params![room,endpoint]).unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('input',?1,'user',?2,'complete',100,100,'owner')",params![room,text]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES('closed-call','delegation',0,'completed','input',100)",[]).unwrap();
        db.execute("INSERT INTO companion_actions VALUES('closed-call',?1,'delegation','minecraft',?2,?3,'queued',?4,100)",params![request,text,source.to_string(),receipt.to_string()]).unwrap();
        db.execute("INSERT INTO companion_action_observers VALUES('closed-call',?1,'minecraft','original-source-hash')",[&request]).unwrap();
    }
    assert_eq!(
        update_admission::prepare(home.path(), &uuid::Uuid::new_v4().to_string()).unwrap()["ready"],
        false
    );
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO companion_observations VALUES('closed-call',?1,'source-revision',?2,100)",
            params![request, observation.to_string()],
        )
        .unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        update_admission::prepare(home.path(), &transaction).unwrap()["ready"],
        true
    );
    update_admission::restore(home.path(), &transaction).unwrap();
    for malformed in [
        {
            let mut v = observation.clone();
            v["provenance"]["source_id"] = json!("another-queue");
            v
        },
        {
            let mut v = observation.clone();
            v["request_id"] = json!("another-request");
            v
        },
        {
            let mut v = observation.clone();
            v["state"] = json!("unknown");
            v["delivery_state"] = json!("unknown");
            v
        },
    ] {
        s.db.lock()
            .unwrap()
            .execute(
                "UPDATE companion_observations SET data=?1",
                [malformed.to_string()],
            )
            .unwrap();
        assert_eq!(
            update_admission::prepare(home.path(), &uuid::Uuid::new_v4().to_string()).unwrap()["ready"],
            false
        );
    }
    {
        let db = s.db.lock().unwrap();
        db.execute(
            "UPDATE companion_observations SET data=?1",
            [observation.to_string()],
        )
        .unwrap();
        db.execute(
            "UPDATE companion_actions SET source=json_set(source,'$.actor','different-person')",
            [],
        )
        .unwrap();
    }
    assert_eq!(
        update_admission::prepare(home.path(), &uuid::Uuid::new_v4().to_string()).unwrap()["ready"],
        false
    );
    {
        let db = s.db.lock().unwrap();
        db.execute(
            "UPDATE companion_actions SET source=?1,state='unknown'",
            [source.to_string()],
        )
        .unwrap();
    }
    assert_eq!(
        update_admission::prepare(home.path(), &uuid::Uuid::new_v4().to_string()).unwrap()["ready"],
        false
    );
    let retained: (String, String) =
        s.db.lock()
            .unwrap()
            .query_row("SELECT state,result FROM companion_actions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
    assert_eq!(retained, ("unknown".into(), receipt.to_string()));
}
async fn request(s: Arc<AppState>, admin: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = server::router(s)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {}", admin.trim()))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let data = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&data).unwrap())
}

#[tokio::test]
async fn blocked_new_message_has_no_saved_input_or_worker_but_old_receipt_remains_readable() {
    let (home, s, admin) = fixture();
    let room = s
        .create_room("Existing room", "owner", &["default".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let old = uuid::Uuid::new_v4().to_string();
    s.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,targets) VALUES(?1,?2,'user','Earlier request','complete',1,1,'owner','[\"default\"]')",rusqlite::params![old,room]).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        update_admission::prepare(home.path(), &transaction).unwrap()["ready"],
        true
    );
    let path = format!("/api/v1/conversations/{room}/messages");
    let (status,body)=request(s.clone(),&admin,&path,json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"New work","target_agent_ids":["default"],"attachment_ids":[]})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(s.running.lock().unwrap().is_empty());
    {
        let db = s.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM input_deliveries", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let (status,body)=request(s.clone(),&admin,&path,json!({"request_id":old,"text":"Earlier request","target_agent_ids":["default"],"attachment_ids":[]})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["duplicate"], true);
    assert!(s.running.lock().unwrap().is_empty());
    assert_eq!(
        update_admission::status(home.path()).unwrap()["transaction_id"],
        transaction
    );
}

#[tokio::test]
async fn blocked_call_creates_neither_room_nor_provider_setup_and_gate_survives_reopen() {
    let (home, s, admin) = fixture();
    let executable = std::env::current_exe().unwrap();
    std::fs::write(home.path().join("companion.json"),json!({"enabled":true,"node":executable,"bridge":executable,"profiles":{"default":{"voice":"marin"}}}).to_string()).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        update_admission::prepare(home.path(), &transaction).unwrap()["ready"],
        true
    );
    let (status, body) = request(
        s.clone(),
        &admin,
        "/api/v1/companion/calls",
        json!({"request_id":uuid::Uuid::new_v4().to_string(),"agent_id":"default","sdp":"v=0\r\n"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    for table in [
        "conversations",
        "companion_calls",
        "companion_tool_grants",
        "companion_delegations",
    ] {
        assert_eq!(
            s.db.lock()
                .unwrap()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "unexpected row in {table}"
        );
    }
    drop(s);
    let reopened = AppState::open(home.path().to_owned()).unwrap();
    assert_eq!(
        update_admission::status_db(&reopened.db.lock().unwrap()).unwrap()["transaction_id"],
        transaction
    );
    assert!(reopened.running.lock().unwrap().is_empty());
    update_admission::restore(home.path(), &transaction).unwrap();
    assert_eq!(
        update_admission::status(home.path()).unwrap()["closed"],
        false
    );
}

#[tokio::test]
async fn local_probe_lifetime_is_visible_and_crash_residue_is_not_erased() {
    let (home, s, _admin) = fixture();
    let guard = update_admission::begin_probe(&s).unwrap();
    let transaction = uuid::Uuid::new_v4().to_string();
    let busy = update_admission::prepare(home.path(), &transaction).unwrap();
    assert_eq!(busy["ready"], false);
    assert_eq!(
        busy["busy"],
        json!([{"kind":"active_or_uncertain_local_operations","count":1}])
    );
    drop(guard);
    assert_eq!(
        update_admission::prepare(home.path(), &transaction).unwrap()["ready"],
        true
    );
    assert!(update_admission::begin_probe(&s).is_err());
    assert!(update_admission::begin_configuration(&s).is_err());
    update_admission::restore(home.path(), &transaction).unwrap();
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO native_update_activity VALUES('retained-crash','connection_probe',1)",
            [],
        )
        .unwrap();
    drop(s);
    let reopened = AppState::open(home.path().to_owned()).unwrap();
    let busy = update_admission::prepare(home.path(), &uuid::Uuid::new_v4().to_string()).unwrap();
    assert_eq!(busy["ready"], false);
    assert_eq!(
        reopened
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM native_update_activity", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
