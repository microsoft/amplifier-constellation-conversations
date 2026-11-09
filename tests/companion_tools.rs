//! Disposable boundary checks: no voice provider, ACP engine or live connector.
use agent_conversations::{
    auth, companion_tools,
    config::{Agent, Config},
    server,
    state::{AppState, now_ms},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    home: TempDir,
    s: Arc<AppState>,
    app: Router,
}
struct Call {
    id: String,
    room: String,
    token: String,
}
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let c = Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Tools fixture".into(),
        owner: "owner".into(),
        allowed_users: vec!["other".into()],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://example.test:18445".into(),
        pam_service: "agent-conversations".into(),
        tls_cert: home.path().join("cert"),
        tls_key: home.path().join("key"),
        tls_ca: None,
        import_source: "fixture".into(),
        import_surface: "fixture".into(),
        agent: Agent {
            label: "Coach".into(),
            command: "/bin/true".into(),
            args: vec![],
            cwd: home.path().to_string_lossy().into(),
            session_cwd: None,
            env: BTreeMap::new(),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        },
    };
    c.save(home.path()).unwrap();
    std::fs::write(home.path().join("helper.py"),r#"import json,pathlib,sys
root=pathlib.Path(__file__).parent
body=json.load(sys.stdin)
with (root/'effects.jsonl').open('a') as f:f.write(json.dumps(body)+'\n')
if (root/'lose-ack').exists():sys.exit(1)
if (root/'mismatch-ack').exists():
 print(json.dumps({'state':'completed','request_id':'other-request','message':'An untrusted completion claim'}));sys.exit(0)
print(json.dumps({'state':'queued','request_id':body['request_id'],'domain_request_id':'fixture-source'}))
"#).unwrap();
    std::fs::write(home.path().join("companion-actions.json"),json!({"enabled":true,"agents":{"default":[{"id":"minecraft","label":"Workshop","description":"Fixture human-task control","command":["/usr/bin/python3",home.path().join("helper.py")]}]}}).to_string()).unwrap();
    let s = AppState::open(home.path().into()).unwrap();
    let app = server::router(s.clone());
    Fixture { home, s, app }
}
fn call(f: &Fixture) -> Call {
    let room = f.s.create("Private voice fixture").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = uuid::Uuid::new_v4().to_string();
    let hash =
        auth::digest(&serde_json::to_string(&f.s.agent("default").unwrap().unwrap()).unwrap());
    f.s.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)",params![id,uuid::Uuid::new_v4().to_string(),room,now_ms(),hash]).unwrap();
    let config = companion_tools::create_mcp_config(&f.s, &id).unwrap();
    Call {
        id,
        room,
        token: config["token"].as_str().unwrap().to_owned(),
    }
}
fn delegation(f: &Fixture, c: &Call, role: &str) -> (String, String, String) {
    delegation_named(f, c, role, "fixture-delegation")
}
fn delegation_named(f: &Fixture, c: &Call, role: &str, did: &str) -> (String, String, String) {
    let did = did.to_owned();
    let mid = uuid::Uuid::new_v4().to_string();
    let turn = uuid::Uuid::new_v4().to_string();
    let at = now_ms();
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,?3,'Please build the small tower in the Workshop.','complete',?4,?4,'owner')",params![mid,c.room,role,at]).unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant','','streaming',?3,?3,'Coach','default')",params![turn,c.room,at]).unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,100,'dispatched',?3,?4)",params![c.id,did,mid,at]).unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,target_turn,updated_at) VALUES(?1,?2,?3,'default','direct','submitted','prompt',?4,?5)",params![uuid::Uuid::new_v4().to_string(),mid,c.room,turn,at]).unwrap();
    let control = std::fs::read_to_string(f.home.path().join("secrets/control-token")).unwrap();
    db.execute(
        "INSERT INTO input_authority(message_id,token_hash) VALUES(?1,?2)",
        params![mid, auth::digest(control.trim())],
    )
    .unwrap();
    (did, mid, turn)
}
fn action(id: &str) -> Value {
    json!({"connection_id":"minecraft","instruction":"Build the small tower the human requested.","request_id":id,"delegation_id":"fixture-delegation"})
}
fn effects(f: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(f.home.path().join("effects.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
async fn request(
    f: &Fixture,
    c: &Call,
    method: &str,
    suffix: &str,
    body: Option<Value>,
    token: &str,
    origin: bool,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(format!("/api/v1/companion/calls/{}/tools/{suffix}", c.id))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if origin {
        r = r.header("origin", "https://example.test:18445");
    }
    let response = f
        .app
        .clone()
        .oneshot(
            r.body(
                body.map(|v| Body::from(v.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn call_tokens_and_private_room_identity_cannot_cross_boundaries() {
    let f = fixture();
    let a = call(&f);
    let b = call(&f);
    assert_eq!(
        request(&f, &a, "GET", "connections", None, &b.token, false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&f, &a, "GET", "connections", None, &a.token, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, value) = request(&f, &a, "GET", "connections", None, &a.token, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["connections"][0]["id"], "minecraft");
    assert!(!value.to_string().contains("helper.py"));
    assert!(!value.to_string().contains(&a.token));
    f.s.db.lock().unwrap().execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'other','member',?2)",params![a.room,now_ms()]).unwrap();
    assert_eq!(
        request(&f, &a, "GET", "connections", None, &a.token, false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn an_active_human_delegation_is_required_before_any_helper_execution() {
    let f = fixture();
    let c = call(&f);
    let result = request(
        &f,
        &c,
        "POST",
        "actions",
        Some(action(&uuid::Uuid::new_v4().to_string())),
        &c.token,
        false,
    )
    .await;
    assert_eq!(result.0, StatusCode::BAD_REQUEST, "{}", result.1);
    assert!(effects(&f).is_empty());
    let (_, mid, turn) = delegation(&f, &c, "assistant");
    assert!(
        !request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&uuid::Uuid::new_v4().to_string())),
            &c.token,
            false
        )
        .await
        .0
        .is_success()
    );
    f.s.db
        .lock()
        .unwrap()
        .execute("UPDATE messages SET role='user' WHERE id=?1", [mid])
        .unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute("UPDATE messages SET status='complete' WHERE id=?1", [&turn])
        .unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE input_deliveries SET state='completed' WHERE target_turn=?1",
            [&turn],
        )
        .unwrap();
    assert!(
        !request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&uuid::Uuid::new_v4().to_string())),
            &c.token,
            false
        )
        .await
        .0
        .is_success()
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn duplicate_and_changed_ids_never_repeat_one_delegated_action() {
    let f = fixture();
    let c = call(&f);
    let (did, mid, _) = delegation(&f, &c, "user");
    let rid = uuid::Uuid::new_v4().to_string();
    let (status, first) = request(
        &f,
        &c,
        "POST",
        "actions",
        Some(action(&rid)),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["state"], "queued");
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&rid)),
            &c.token,
            false
        )
        .await
        .1,
        first
    );
    let second_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&second_id)),
            &c.token,
            false
        )
        .await
        .1,
        first
    );
    for id in [&rid, &second_id] {
        let mut altered = action(id);
        altered["instruction"] = json!("A different action without another human request");
        assert_eq!(
            request(&f, &c, "POST", "actions", Some(altered), &c.token, false)
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    let actual = effects(&f);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0]["source"]["actor"], "owner");
    assert_eq!(actual[0]["source"]["call_id"], c.id);
    assert_eq!(actual[0]["source"]["delegation_id"], did);
    assert_eq!(actual[0]["source"]["message_id"], mid);
    assert_eq!(
        actual[0]["source"]["original_text"],
        "Please build the small tower in the Workshop."
    );
    assert_eq!(actual[0]["source"]["audience"], "private");
    assert!(!actual[0].to_string().contains(&c.token));
}

#[tokio::test]
async fn lost_acknowledgement_and_restart_remain_unknown_without_reexecution() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let rid = uuid::Uuid::new_v4().to_string();
    std::fs::write(f.home.path().join("lose-ack"), "fixture").unwrap();
    let (status, first) = request(
        &f,
        &c,
        "POST",
        "actions",
        Some(action(&rid)),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["state"], "unknown");
    assert_eq!(effects(&f).len(), 1);
    std::fs::remove_file(f.home.path().join("lose-ack")).unwrap();
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&uuid::Uuid::new_v4().to_string())),
            &c.token,
            false
        )
        .await
        .1,
        first
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_actions SET state='submitting' WHERE request_id=?1",
            [&rid],
        )
        .unwrap();
    let reopened = AppState::open(f.home.path().into()).unwrap();
    let state: String = reopened
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT state FROM companion_actions WHERE request_id=?1",
            [&rid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "unknown");
    let replay = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/companion/calls/{}/tools/actions", c.id))
                .header("authorization", format!("Bearer {}", c.token))
                .header("content-type", "application/json")
                .body(Body::from(action(&rid).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!replay.status().is_success());
    assert_eq!(effects(&f).len(), 1);
}

#[tokio::test]
async fn disabling_actions_revokes_existing_grants() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let file = f.home.path().join("companion-actions.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    config["enabled"] = json!(false);
    std::fs::write(file, config.to_string()).unwrap();
    assert!(
        !request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&uuid::Uuid::new_v4().to_string())),
            &c.token,
            false
        )
        .await
        .0
        .is_success()
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn shared_urls_are_inert_validated_and_readable_after_display() {
    let f = fixture();
    let c = call(&f);
    for url in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://user:secret@example.com/x",
        "data:text/html,hello",
    ] {
        assert_eq!(
            request(
                &f,
                &c,
                "POST",
                "shared",
                Some(json!({"url":url,"title":"Example","kind":"link"})),
                &c.token,
                false
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let url = "https://example.invalid/does-not-need-to-resolve";
    let (status, shown) = request(
        &f,
        &c,
        "POST",
        "shared",
        Some(json!({"url":url,"title":"Example","kind":"video"})),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["state"], "saved");
    assert_eq!(shown["presentation"], "requested_current");
    let (status, shared) = request(&f, &c, "GET", "shared", None, &c.token, false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        shared.to_string().contains(url),
        "Shown cards must be discoverable: {shared}"
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn exact_delegation_stays_bound_when_later_input_is_incorporated() {
    let f = fixture();
    let c = call(&f);
    let (older, old_message, _) = delegation(&f, &c, "user");
    let (newer, new_message, _) = delegation_named(&f, &c, "user", "later-provider-delegation");
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE input_deliveries SET state='incorporated' WHERE message_id=?1",
            [&new_message],
        )
        .unwrap();
    let rid = uuid::Uuid::new_v4().to_string();
    let (status, _) = request(
        &f,
        &c,
        "POST",
        "actions",
        Some(action(&rid)),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let effect = effects(&f).pop().unwrap();
    assert_eq!(effect["source"]["delegation_id"], older);
    assert_eq!(effect["source"]["message_id"], old_message);
    let mut reuse = action(&rid);
    reuse["delegation_id"] = json!(newer);
    assert_eq!(
        request(&f, &c, "POST", "actions", Some(reuse), &c.token, false)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let other = call(&f);
    delegation_named(&f, &other, "user", "other-call-delegation");
    let mut cross = action(&uuid::Uuid::new_v4().to_string());
    cross["delegation_id"] = json!("other-call-delegation");
    assert_eq!(
        request(&f, &c, "POST", "actions", Some(cross), &c.token, false)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(effects(&f).len(), 1);
}

#[tokio::test]
async fn revoked_source_authority_prevents_new_connector_effects() {
    let f = fixture();
    let c = call(&f);
    let (_, mid, _) = delegation(&f, &c, "user");
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM input_authority WHERE message_id=?1", [mid])
        .unwrap();
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&uuid::Uuid::new_v4().to_string())),
            &c.token,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn mismatched_helper_receipts_cannot_preserve_an_inner_completion_claim() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    std::fs::write(f.home.path().join("mismatch-ack"), "fixture").unwrap();
    let rid = uuid::Uuid::new_v4().to_string();
    let (status, receipt) = request(
        &f,
        &c,
        "POST",
        "actions",
        Some(action(&rid)),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["state"], "unknown");
    assert_eq!(receipt["result"]["state"], "unknown");
    assert_eq!(receipt["result"]["request_id"], rid);
    assert!(!receipt.to_string().contains("other-request"));
    assert!(!receipt.to_string().contains("untrusted completion claim"));
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&rid)),
            &c.token,
            false
        )
        .await
        .1,
        receipt
    );
    assert_eq!(effects(&f).len(), 1);
}

#[tokio::test]
async fn status_reconciles_abandoned_submission_to_unknown_without_replaying() {
    let f = fixture();
    let c = call(&f);
    let (did, _, _) = delegation(&f, &c, "user");
    let rid = uuid::Uuid::new_v4().to_string();
    f.s.db.lock().unwrap().execute("INSERT INTO companion_actions(call_id,request_id,delegation_id,connection_id,instruction,source,state,result,created_at) VALUES(?1,?2,?3,'minecraft','Build the small tower the human requested.','{}','submitting','{}',?4)",params![c.id,rid,did,now_ms()-46_000]).unwrap();
    let (status, receipt) = request(
        &f,
        &c,
        "GET",
        &format!("actions/{rid}"),
        None,
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["state"], "unknown");
    assert_eq!(receipt["result"]["state"], "unknown");
    assert_eq!(
        receipt["result"]["diagnostic"],
        "submission_receipt_expired"
    );
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&uuid::Uuid::new_v4().to_string())),
            &c.token,
            false
        )
        .await
        .1,
        receipt
    );
    assert!(effects(&f).is_empty());
    // A real receipt that arrives later can settle the original request.
    let settled = json!({"request_id":rid,"state":"queued"});
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_actions SET state='queued',result=?2 WHERE request_id=?1",
            params![rid, settled.to_string()],
        )
        .unwrap();
    assert_eq!(
        request(
            &f,
            &c,
            "GET",
            &format!("actions/{rid}"),
            None,
            &c.token,
            false
        )
        .await
        .1["state"],
        "queued"
    );
}

fn native_permission(tool: &str, arguments: Value) -> Value {
    json!({"sessionId":"native-session","toolCall":{"toolCallId":"native-call","title":format!("companion / {tool}"),"kind":"other","status":"pending","rawInput":{"server":"companion","tool":tool,"arguments":arguments}},"options":[{"optionId":"persistent","name":"Always","kind":"allow_always"},{"optionId":"single","name":"Once","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]})
}

#[tokio::test]
async fn exact_awareness_lookup_finds_older_card_and_cannot_cross_calls_or_redirect() {
    let f = fixture();
    let a = call(&f);
    let b = call(&f);
    let awareness_id = "a1".repeat(32);
    let fact =
        "The roof was reported complete; the final inspection failed. Do not claim verification.";
    let data = json!({"awareness_id":awareness_id,"text":fact,"observed_only":true,"conversation_id":a.room});
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'card',?3,?4)",params![a.id,format!("awareness-card:{awareness_id}"),data.to_string(),now_ms()]).unwrap();
        for i in 0..20 {
            db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'card',?3,?4)",params![a.id,format!("new-card-{i}"),json!({"text":format!("Newer card {i}"),"conversation_id":a.room}).to_string(),now_ms()]).unwrap();
        }
    }
    let (status, recent) = request(&f, &a, "GET", "shared", None, &a.token, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recent["shown"].as_array().unwrap().len(), 16);
    assert!(!recent.to_string().contains(fact));
    let query = format!("shared?awareness_id={awareness_id}");
    let (status, exact) = request(&f, &a, "GET", &query, None, &a.token, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exact["items"], json!([]));
    assert_eq!(exact["shown"].as_array().unwrap().len(), 1);
    assert_eq!(exact["shown"][0]["awareness_id"], awareness_id);
    assert_eq!(exact["shown"][0]["text"], fact);
    assert!(
        exact["shown"][0]["card_id"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            > 0
    );
    assert_eq!(
        request(&f, &b, "GET", &query, None, &b.token, false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&f, &a, "GET", &query, None, &b.token, false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f,
            &a,
            "GET",
            &format!("shared?awareness_id={}", "b".repeat(64)),
            None,
            &a.token,
            false
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for bad in ["short".to_owned(), "g".repeat(64), "a".repeat(65)] {
        assert_eq!(
            request(
                &f,
                &a,
                "GET",
                &format!("shared?awareness_id={bad}"),
                None,
                &a.token,
                false
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/companion/calls/{}/tools/{query}&call_id={}",
                    a.id, b.id
                ))
                .header("authorization", format!("Bearer {}", a.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // Corrupted/mismatched stored identities must not return another card as
    // the requested source, even when its event key happens to match.
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_events SET data=?3 WHERE call_id=?1 AND event_key=?2",
            params![
                a.id,
                format!("awareness-card:{awareness_id}"),
                json!({"awareness_id":"c".repeat(64),"text":"Wrong stored identity"}).to_string()
            ],
        )
        .unwrap();
    assert_eq!(
        request(&f, &a, "GET", &query, None, &a.token, false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert!(effects(&f).is_empty());
}
#[tokio::test]
async fn scoped_native_permission_policy_requires_exact_source_and_never_title_only_or_persistent_approval()
 {
    let f = fixture();
    let c = call(&f);
    companion_tools::record_mcp_port(&f.s, &c.id, 34567).unwrap();
    let allow = |p: &Value, injected| {
        companion_tools::permission_once(&f.s, &c.room, "owner", "default", injected, p).unwrap()
    };
    for (tool, args) in [
        ("companion_connections", json!({})),
        ("companion_observations", json!({})),
        (
            "companion_connection_status",
            json!({"connection_id":"minecraft"}),
        ),
        ("companion_shared", json!({})),
        ("companion_shared", json!({"awareness_id":"a1".repeat(32)})),
        (
            "companion_action_status",
            json!({"request_id":uuid::Uuid::new_v4().to_string()}),
        ),
        (
            "companion_show",
            json!({"url":"https://example.test/video","title":"Safe title","kind":"video"}),
        ),
    ] {
        let permission = native_permission(tool, args);
        assert_eq!(
            allow(&permission, true).unwrap()["outcome"]["optionId"],
            "single"
        );
        assert!(allow(&permission, false).is_none());
    }
    let valid = native_permission("companion_connections", json!({}));
    assert!(
        companion_tools::permission_once(&f.s, &c.room, "other", "default", true, &valid)
            .unwrap()
            .is_none()
    );
    assert!(
        companion_tools::permission_once(&f.s, &c.room, "owner", "other-agent", true, &valid)
            .unwrap()
            .is_none()
    );
    let config_path = f.home.path().join("companion-actions.json");
    let original = std::fs::read(&config_path).unwrap();
    let mut disabled: Value = serde_json::from_slice(&original).unwrap();
    disabled["enabled"] = json!(false);
    std::fs::write(&config_path, disabled.to_string()).unwrap();
    assert_eq!(allow(&valid, true).unwrap()["outcome"]["optionId"], "no");
    std::fs::write(config_path, original).unwrap();
    let mut wrong_server = valid.clone();
    wrong_server["toolCall"]["title"] = json!("companion / companion_connections");
    wrong_server["toolCall"]["rawInput"]["server"] = json!("other");
    let mut title_only = valid.clone();
    title_only["toolCall"]["rawInput"] = json!({"command":"echo arbitrary host operation"});
    let mut forged_shell = valid.clone();
    forged_shell["toolCall"]["rawInput"]["command"] = json!("echo arbitrary host operation");
    let mut forged_builtin_arguments = valid.clone();
    forged_builtin_arguments["toolCall"]["title"] = json!("workspace run");
    let mut no_once = valid.clone();
    no_once["options"]
        .as_array_mut()
        .unwrap()
        .retain(|o| o["kind"] != "allow_once");
    for permission in [
        wrong_server,
        title_only,
        forged_shell,
        forged_builtin_arguments,
        no_once,
        native_permission("workspace_run", json!({"command":"echo arbitrary"})),
        native_permission(
            "companion_connections",
            json!({"url":"http://127.0.0.1/other"}),
        ),
        native_permission(
            "companion_show",
            json!({"url":"javascript:alert(1)","title":"unsafe"}),
        ),
        native_permission("companion_shared", json!({"awareness_id":null})),
        native_permission(
            "companion_connection_status",
            json!({"connection_id":"minecraft","url":"https://other.test"}),
        ),
        native_permission("companion_connection_status", json!({"connection_id":""})),
        native_permission("companion_shared", json!({"awareness_id":"../other"})),
        native_permission("companion_shared", json!({"awareness_id":"g".repeat(64)})),
        native_permission(
            "companion_shared",
            json!({"awareness_id":"a".repeat(64),"call_id":"another-call"}),
        ),
    ] {
        assert!(allow(&permission, true).is_none());
    }
    let action_permission = native_permission(
        "companion_request_action",
        action(&uuid::Uuid::new_v4().to_string()),
    );
    assert_eq!(
        allow(&action_permission, true).unwrap()["outcome"]["optionId"],
        "no"
    );
    let (_, mid, _) = delegation(&f, &c, "user");
    assert_eq!(
        allow(&action_permission, true).unwrap()["outcome"]["optionId"],
        "single"
    );
    let mut unknown_source = action_permission.clone();
    unknown_source["toolCall"]["rawInput"]["arguments"]["delegation_id"] = json!("other-source");
    assert_eq!(
        allow(&unknown_source, true).unwrap()["outcome"]["optionId"],
        "no"
    );
    let mut unknown_connection = action_permission.clone();
    unknown_connection["toolCall"]["rawInput"]["arguments"]["connection_id"] =
        json!("not-enrolled");
    assert_eq!(
        allow(&unknown_connection, true).unwrap()["outcome"]["optionId"],
        "no"
    );
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM input_authority WHERE message_id=?1", [mid])
        .unwrap();
    assert_eq!(
        allow(&action_permission, true).unwrap()["outcome"]["optionId"],
        "no"
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&c.id],
        )
        .unwrap();
    assert_eq!(
        allow(&valid, true).unwrap(),
        agent_conversations::permissions::cancelled()
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn late_exact_own_tools_are_denied_after_call_end_without_a_hidden_human_prompt() {
    let f = fixture();
    let c = call(&f);
    companion_tools::record_mcp_port(&f.s, &c.id, 34567).unwrap();
    delegation(&f, &c, "user");
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&c.id],
        )
        .unwrap();
    let check = |request: &Value, injected| {
        companion_tools::permission_once(&f.s, &c.room, "owner", "default", injected, request)
            .unwrap()
    };
    for (tool, args) in [
        ("companion_observations", json!({})),
        (
            "companion_connection_status",
            json!({"connection_id":"minecraft"}),
        ),
        (
            "companion_action_status",
            json!({"request_id":uuid::Uuid::new_v4().to_string()}),
        ),
        (
            "companion_request_action",
            action(&uuid::Uuid::new_v4().to_string()),
        ),
        (
            "companion_capture",
            json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"fixture-delegation"}),
        ),
        (
            "companion_show",
            json!({"url":"https://example.test","title":"Late display"}),
        ),
    ] {
        let permission = native_permission(tool, args);
        assert_eq!(
            check(&permission, true).unwrap(),
            agent_conversations::permissions::cancelled()
        );
        assert!(check(&permission, false).is_none());
        let mut no_reject = permission.clone();
        no_reject["options"]
            .as_array_mut()
            .unwrap()
            .retain(|v| v["kind"] != "reject_once");
        assert_eq!(
            check(&no_reject, true).unwrap(),
            agent_conversations::permissions::cancelled()
        );
    }
    for permission in [
        native_permission("unrelated_tool", json!({})),
        native_permission("companion_observations", json!({"command":"unrelated"})),
        native_permission(
            "companion_capture",
            json!({"operation_id":"bad","delegation_id":"fixture-delegation"}),
        ),
    ] {
        assert!(check(&permission, true).is_none());
    }
    assert!(effects(&f).is_empty());
    let db = f.s.db.lock().unwrap();
    let denied: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM audit WHERE kind='permission.companion.denied'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(denied, 12);
}

#[tokio::test]
async fn fresh_connection_status_stays_on_the_authenticated_private_call_without_action_effects() {
    let f = fixture();
    let c = call(&f);
    let path = "connection-status?connection_id=minecraft";
    let (status, response) = request(&f, &c, "GET", path, None, &c.token, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["status"], "not_configured");
    assert_eq!(response["read_only"], true);
    assert_eq!(
        request(&f, &c, "GET", path, None, "wrong-token", false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&f, &c, "GET", path, None, &c.token, true).await.0,
        StatusCode::FORBIDDEN
    );
    let malformed = f.app.clone().oneshot(Request::builder()
        .uri(format!("/api/v1/companion/calls/{}/tools/connection-status?connection_id=minecraft&command=other", c.id))
        .header("authorization", format!("Bearer {}", c.token)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&c.id],
        )
        .unwrap();
    assert_eq!(
        request(&f, &c, "GET", path, None, &c.token, false).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn short_plain_note_cards_share_the_existing_board_and_permission_boundary() {
    let f = fixture();
    let c = call(&f);
    companion_tools::record_mcp_port(&f.s, &c.id, 34567).unwrap();
    let text = "Compare the two paths:\n- Keep the entrance clear.\n- Check the roof next.\n<script>not executable</script>";
    let args = json!({"title":"Next steps","text":text,"kind":"note"});
    let (status, saved) = request(
        &f,
        &c,
        "POST",
        "shared",
        Some(args.clone()),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["state"], "saved");
    let (_, shared) = request(&f, &c, "GET", "shared", None, &c.token, false).await;
    assert_eq!(shared["shown"][0]["text"], text);
    assert_eq!(shared["shown"][0]["kind"], "note");
    assert!(shared["shown"][0]["url"].is_null());
    assert_eq!(shared["shown"][0]["presentation"], "current");
    let permission = |arguments: Value| json!({"toolCall":{"toolCallId":"note","title":"companion / companion_show","kind":"other","status":"pending","rawInput":{"server":"companion","tool":"companion_show","arguments":arguments}},"options":[{"optionId":"once","name":"Once","kind":"allow_once"}]});
    assert_eq!(
        companion_tools::permission_once(
            &f.s,
            &c.room,
            "owner",
            "default",
            true,
            &permission(args)
        )
        .unwrap()
        .unwrap()["outcome"]["optionId"],
        "once"
    );
    for bad in [
        json!({"title":"Missing content"}),
        json!({"title":"Both","url":"https://example.test","text":"Note"}),
        json!({"title":"Too long","text":"漢".repeat(401)}),
        json!({"title":"Blank","text":"  "}),
        json!({"title":"Wrong kind","text":"Note","kind":"image"}),
        json!({"title":"Wrong kind","url":"https://example.test","kind":"note"}),
    ] {
        assert_ne!(
            request(&f, &c, "POST", "shared", Some(bad.clone()), &c.token, false)
                .await
                .0,
            StatusCode::OK
        );
        assert!(
            companion_tools::permission_once(
                &f.s,
                &c.room,
                "owner",
                "default",
                true,
                &permission(bad)
            )
            .unwrap()
            .is_none()
        );
    }
    assert!(
        companion_tools::permission_once(
            &f.s,
            &c.room,
            "owner",
            "default",
            true,
            &permission(json!({"title":"Note","text":"Safe","html":"<script>"}))
        )
        .unwrap()
        .is_none()
    );
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn connector_source_preserves_human_words_separate_from_backend_context() {
    let f = fixture();
    let c = call(&f);
    let (did, mid, _) = delegation(&f, &c, "user");
    let words = "Actually, check the blue door first; leave the roof alone.";
    let wrapper = format!(
        "The live voice conversation delegated help at 100ms. Backend instructions and old context. Current human source: {words}"
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET text=?2 WHERE id=?1",
            params![mid, wrapper],
        )
        .unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_delegations SET request_text=?3 WHERE call_id=?1 AND id=?2",
            params![c.id, did, words],
        )
        .unwrap();
    let rid = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&rid)),
            &c.token,
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    let submitted = effects(&f);
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0]["source"]["original_text"], words);
    assert_eq!(
        submitted[0]["source"]["source_revision"],
        auth::digest(words)
    );
    assert_eq!(
        submitted[0]["source"]["evidence_revision"],
        auth::digest(&wrapper)
    );
    assert_eq!(submitted[0]["source"]["intent_id"], did);
}

#[tokio::test]
async fn action_preserves_original_human_time_and_refuses_changed_transcript() {
    for invalid in [false, true] {
        let f = fixture();
        let c = call(&f);
        let (did, _, _) = delegation(&f, &c, "user");
        let old = now_ms() - 7_000_000;
        let words = "Keep the blue door; explain what remains.";
        {
            let db = f.s.db.lock().unwrap();
            let event = json!({"role":"user","text":words,"start_ms":10,"end_ms":20,"event_id":"original-human-fragment"});
            db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'original-human-fragment','transcript',?2,?3)",params![c.id,event.to_string(),old]).unwrap();
            let seq = db.last_insert_rowid();
            let source = json!({"version":1,"previous_anchor_ms":0,"offset_ms":100,"fragments":[{"seq":seq,"role":"user","text":words,"start_ms":10,"end_ms":20,"event_id":"original-human-fragment"}]});
            db.execute("UPDATE companion_delegations SET source=?3,request_text=?4 WHERE call_id=?1 AND id=?2",params![c.id,did,source.to_string(),words]).unwrap();
            if invalid {
                db.execute("UPDATE companion_events SET data=json_set(data,'$.text','Different words') WHERE seq=?1",[seq]).unwrap();
            }
        }
        let rid = uuid::Uuid::new_v4().to_string();
        let (status, _) = request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&rid)),
            &c.token,
            false,
        )
        .await;
        if invalid {
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(effects(&f).is_empty());
        } else {
            assert_eq!(status, StatusCode::OK);
            let submitted = effects(&f);
            assert_eq!(submitted.len(), 1);
            assert_eq!(submitted[0]["source"]["occurred_at"], old);
            assert_eq!(
                submitted[0]["source"]["time_basis"],
                "retained_human_transcript"
            );
            assert_eq!(submitted[0]["source"]["original_text"], words);
        }
    }
}

#[tokio::test]
async fn explicit_status_response_queues_qualified_context_before_backend_paraphrase() {
    let f = fixture();
    let c = call(&f);
    let current = time::OffsetDateTime::now_utc();
    let stamp = |at: time::OffsetDateTime| {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        )
    };
    let old = stamp(current - time::Duration::hours(4));
    let now = stamp(current);
    let event = json!({"id":"current-assignment","revision":"r1","kind":"task_observation","text":json!({"status":"running","progress":{"at":old,"text":"The linkage was unfinished."}}).to_string(),"occurred_at":now,"observed_at":now,"actor":"Jarvis","audience":{"kind":"public"},"provenance":{"kind":"minecraft_current_task","current_state_snapshot":true}});
    let report = json!({"version":1,"connection_id":"minecraft","availability":"available","observed_at":now,"events":[event],"actions":[],"coverage":{"bounded":true,"gap":true},"presence":{}});
    std::fs::write(f.home.path().join("status-result.json"), report.to_string()).unwrap();
    std::fs::write(f.home.path().join("status-read.py"),"import pathlib,sys,json\njson.load(sys.stdin)\nprint((pathlib.Path(__file__).parent/'status-result.json').read_text())\n").unwrap();
    std::fs::write(f.home.path().join("companion-awareness.json"),json!({"enabled":true,"decision_command":["/bin/false"],"agents":{"default":[{"id":"minecraft","label":"Workshop","command":["/usr/bin/python3",f.home.path().join("status-read.py")],"owners":["owner"],"audiences":[{"kind":"public"}]}]}}).to_string()).unwrap();
    let (code, value) = request(
        &f,
        &c,
        "GET",
        "connection-status?connection_id=minecraft",
        None,
        &c.token,
        false,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(value["reports"][0]["text"], report["events"][0]["text"]);
    assert!(value.get("binding").is_none());
    let db = f.s.db.lock().unwrap();
    let (raw, state): (String, String) = db
        .query_row(
            "SELECT updates,state FROM companion_voice_batches WHERE call_id=?1",
            [&c.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "pending");
    let commands: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(commands.as_array().unwrap().len(), 2);
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["channel"] == "thinking")
    );
    assert!(raw.contains(&old) && raw.contains(&now));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_awareness WHERE decision IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    assert!(effects(&f).is_empty());
}

#[tokio::test]
async fn action_accepts_exact_mixed_whitespace_but_rejects_changed_or_only_blank_sources() {
    for case in [
        "exact",
        "all_whitespace",
        "text",
        "event_id",
        "start_ms",
        "end_ms",
        "role",
    ] {
        let f = fixture();
        let c = call(&f);
        let (did, _, _) = delegation(&f, &c, "user");
        let old = now_ms() - 7_000_000;
        let words = "Keep the blue door; explain what remains.";
        let source = {
            let db = f.s.db.lock().unwrap();
            let mut fragments = Vec::new();
            for (index, role, text, at) in [
                (0, "assistant", "Earlier assistant context", old - 2000),
                (1, "user", " ", old - 1000),
                (
                    2,
                    "user",
                    if case == "all_whitespace" {
                        " \t"
                    } else {
                        words
                    },
                    old,
                ),
                (3, "user", " ", old + 1000),
            ] {
                let event_id = format!("retained-fragment-{index}");
                let mut event = json!({"role":role,"event_id":event_id,"text":text,"start_ms":index*10+1,"end_ms":index*10+5});
                db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)",params![c.id,event_id,event.to_string(),at]).unwrap();
                event["seq"] = json!(db.last_insert_rowid());
                fragments.push(event);
            }
            match case {
                "text" => fragments[1]["text"] = json!("  "),
                "event_id" => fragments[1]["event_id"] = json!("changed-event"),
                "start_ms" => fragments[1]["start_ms"] = json!(12),
                "end_ms" => fragments[1]["end_ms"] = json!(16),
                "role" => fragments[1]["role"] = json!("assistant"),
                _ => {}
            }
            let source =
                json!({"version":1,"previous_anchor_ms":0,"offset_ms":100,"fragments":fragments});
            db.execute("UPDATE companion_delegations SET source=?3,request_text=?4 WHERE call_id=?1 AND id=?2",params![c.id,did,source.to_string(),words]).unwrap();
            source
        };
        let rid = uuid::Uuid::new_v4().to_string();
        let (status, response) = request(
            &f,
            &c,
            "POST",
            "actions",
            Some(action(&rid)),
            &c.token,
            false,
        )
        .await;
        if case == "exact" {
            assert_eq!(status, StatusCode::OK, "{response}");
            let submitted = effects(&f);
            assert_eq!(submitted.len(), 1);
            assert_eq!(submitted[0]["source"]["occurred_at"], old);
            assert_eq!(
                submitted[0]["source"]["time_basis"],
                "retained_human_transcript"
            );
            assert_eq!(submitted[0]["source"]["original_text"], words);
            assert_eq!(
                submitted[0]["source"]["source_revision"],
                auth::digest(words)
            );
            let (duplicate_status, duplicate) = request(
                &f,
                &c,
                "POST",
                "actions",
                Some(action(&rid)),
                &c.token,
                false,
            )
            .await;
            assert_eq!(duplicate_status, StatusCode::OK);
            assert_eq!(duplicate, response);
            assert_eq!(effects(&f).len(), 1, "No replay on the same receipt");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{case}: {response}");
            assert!(effects(&f).is_empty(), "{case}");
            assert_eq!(
                f.s.db
                    .lock()
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{case}"
            );
        }
        let retained: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT source FROM companion_delegations WHERE call_id=?1 AND id=?2",
                    params![c.id, did],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&retained).unwrap(),
            source,
            "Bound source must not be edited"
        );
    }
}
