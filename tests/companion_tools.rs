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
    assert_eq!(shown["state"], "available_on_screen");
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
    assert!(allow(&valid, true).is_none());
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
    assert!(allow(&action_permission, true).is_none());
    let (_, mid, _) = delegation(&f, &c, "user");
    assert!(allow(&action_permission, true).is_some());
    let mut unknown_source = action_permission.clone();
    unknown_source["toolCall"]["rawInput"]["arguments"]["delegation_id"] = json!("other-source");
    assert!(allow(&unknown_source, true).is_none());
    let mut unknown_connection = action_permission.clone();
    unknown_connection["toolCall"]["rawInput"]["arguments"]["connection_id"] =
        json!("not-enrolled");
    assert!(allow(&unknown_connection, true).is_none());
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM input_authority WHERE message_id=?1", [mid])
        .unwrap();
    assert!(allow(&action_permission, true).is_none());
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&c.id],
        )
        .unwrap();
    assert!(allow(&valid, true).is_none());
    assert!(effects(&f).is_empty());
}
