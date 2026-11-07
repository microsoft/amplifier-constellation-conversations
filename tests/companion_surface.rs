//! Surface command boundary tests: disposable browser and call fixtures only.
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
    std::fs::write(
        home.path().join("companion-actions.json"),
        json!({"enabled":true,"agents":{}}).to_string(),
    )
    .unwrap();
    let s = AppState::open(home.path().into()).unwrap();
    for (token, owner) in [("browser", "owner"), ("other-browser", "other")] {
        s.db.lock()
            .unwrap()
            .execute(
                "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,?2,'csrf',?3)",
                params![auth::digest(token), owner, now_ms() + 60_000],
            )
            .unwrap();
    }
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
    companion_tools::record_mcp_port(&f.s, &id, 34567).unwrap();
    f.s.db.lock().unwrap().execute("INSERT INTO companion_surface_capabilities(call_id,image_input,observed_at) VALUES(?1,1,?2)",params![id,now_ms()]).unwrap();
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
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)})),
    )
}

async fn browser_request(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: &str,
    csrf: bool,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", format!("{}={cookie}", auth::cookie_name(&f.s)))
        .header("origin", "https://example.test:18445")
        .header("host", "example.test:18445");
    if csrf {
        r = r.header("x-csrf-token", "csrf")
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
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)})),
    )
}
async fn presence(
    f: &Fixture,
    c: &Call,
    browser: &str,
    camera: bool,
    consent: bool,
) -> (StatusCode, Value) {
    browser_request(f,"POST",&format!("/api/v1/companion/calls/{}/surface/presence",c.id),Some(json!({"browser_id":browser,"camera_enabled":camera,"capture_consent":consent,"user_speaking":false,"agent_speaking":null})),"browser",true).await
}
async fn ack(
    f: &Fixture,
    c: &Call,
    browser: &str,
    operation: &str,
    state: &str,
    ids: Vec<String>,
) -> (StatusCode, Value) {
    browser_request(
        f,
        "POST",
        &format!(
            "/api/v1/companion/calls/{}/surface/commands/{operation}/ack",
            c.id
        ),
        Some(json!({"browser_id":browser,"state":state,"attachment_ids":ids})),
        "browser",
        true,
    )
    .await
}
async fn capture(f: &Fixture, c: &Call, operation: &str) -> (StatusCode, Value) {
    request(
        f,
        c,
        "POST",
        "capture",
        Some(json!({"operation_id":operation,"delegation_id":"fixture-delegation"})),
        &c.token,
        false,
    )
    .await
}
async fn surface_status(f: &Fixture, c: &Call, operation: &str) -> Value {
    let (status, value) = request(
        f,
        c,
        "GET",
        &format!("surface/{operation}"),
        None,
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

#[tokio::test]
async fn voice_hangup_is_exact_call_bound_once_only_and_preserves_background_work() {
    let f = fixture();
    let c = call(&f);
    let other = call(&f);
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    let body = json!({"operation_id":operation,"delegation_id":"fixture-delegation"});
    delegation(&f, &c, "user");
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "hangup",
            Some(body.clone()),
            &c.token,
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    presence(&f, &c, &browser, false, false).await;
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "hangup",
            Some(body.clone()),
            &other.token,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, queued) = request(
        &f,
        &c,
        "POST",
        "hangup",
        Some(body.clone()),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queued["kind"], "hangup");
    assert_eq!(queued["state"], "queued");
    assert_eq!(
        request(&f, &c, "POST", "hangup", Some(body), &c.token, false)
            .await
            .1,
        queued
    );
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .1["execute"],
        true
    );
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .1["execute"],
        false
    );
    assert_eq!(
        ack(&f, &c, &browser, &operation, "executed", vec![])
            .await
            .1["state"],
        "executed"
    );
    assert_eq!(
        ack(&f, &c, &browser, &operation, "failed", vec![]).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        surface_status(&f, &c, &operation).await["state"],
        "executed"
    );
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT state FROM input_deliveries WHERE conversation_id=?1",
            [&c.room],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "submitted"
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM companion_calls WHERE id=?1",
            [&other.id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "connected"
    );
}

#[tokio::test]
async fn feedback_has_a_durable_private_idempotent_receipt_without_external_submission() {
    let f = fixture();
    let c = call(&f);
    let other = call(&f);
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    let body = json!({"operation_id":operation,"delegation_id":"fixture-delegation","text":"Please add a shutter sound when the photo is captured."});
    delegation(&f, &c, "user");
    presence(&f, &c, &browser, false, false).await;
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "feedback",
            Some(body.clone()),
            &other.token,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, receipt) = request(
        &f,
        &c,
        "POST",
        "feedback",
        Some(body.clone()),
        &c.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["state"], "saved");
    assert_eq!(receipt["scope"], "private_installation");
    assert_eq!(receipt["submitted_externally"], false);
    assert_eq!(surface_status(&f, &c, &operation).await, receipt);
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "feedback",
            Some(body.clone()),
            &c.token,
            false
        )
        .await
        .1,
        receipt
    );
    let mut duplicate = body.clone();
    duplicate["operation_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(duplicate), &c.token, false)
            .await
            .1,
        receipt
    );
    let mut changed = body.clone();
    changed["text"] = json!("Different feedback under the same request");
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(changed), &c.token, false)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut long = body.clone();
    long["text"] = json!("é".repeat(2001));
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(long), &c.token, false)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut external = body.clone();
    external["external_channel"] = json!("Slack");
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(external), &c.token, false)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (status, _) = request(
        &f,
        &other,
        "GET",
        &format!("surface/{operation}"),
        None,
        &other.token,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    {
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM companion_feedback", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='card'",
                [&c.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        agent_conversations::companion_surface::migrate(&db).unwrap();
    }
    assert_eq!(surface_status(&f, &c, &operation).await, receipt);
}

#[tokio::test]
async fn feedback_and_hangup_require_current_user_source_and_cannot_expand_authority() {
    let f = fixture();
    let c = call(&f);
    let browser = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, false, false).await;
    delegation(&f, &c, "assistant");
    let base = json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"fixture-delegation"});
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "hangup",
            Some(base.clone()),
            &c.token,
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut feedback = base.clone();
    feedback["text"] = json!("Private feedback");
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(feedback), &c.token, false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut expanded = base;
    expanded["cancel_work"] = json!(true);
    assert_eq!(
        request(&f, &c, "POST", "hangup", Some(expanded), &c.token, false)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn surface_and_feedback_operations_share_one_unambiguous_receipt_namespace() {
    let f = fixture();
    let c = call(&f);
    let browser = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, false, false).await;
    delegation(&f, &c, "user");
    let operation = uuid::Uuid::new_v4().to_string();
    let base = json!({"operation_id":operation,"delegation_id":"fixture-delegation"});
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "hangup",
            Some(base.clone()),
            &c.token,
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut feedback = base;
    feedback["text"] = json!("Feedback cannot replace a hangup receipt.");
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(feedback), &c.token, false)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(surface_status(&f, &c, &operation).await["kind"], "hangup");
    let operation = uuid::Uuid::new_v4().to_string();
    let base = json!({"operation_id":operation,"delegation_id":"fixture-delegation"});
    let mut feedback = base.clone();
    feedback["text"] = json!("Feedback is still privately saved.");
    assert_eq!(
        request(&f, &c, "POST", "feedback", Some(feedback), &c.token, false)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&f, &c, "POST", "hangup", Some(base), &c.token, false)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(surface_status(&f, &c, &operation).await["state"], "saved");
}

fn expire(f: &Fixture, c: &Call, operation: &str) {
    f.s.db.lock().unwrap().execute("UPDATE companion_surface_commands SET expires_at=?3 WHERE call_id=?1 AND operation_id=?2",params![c.id,operation,now_ms()-1]).unwrap();
}

#[tokio::test]
async fn camera_requires_actual_browser_consent_fresh_presence_and_active_exact_authority() {
    let f = fixture();
    let c = call(&f);
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    assert_eq!(capture(&f, &c, &operation).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        presence(&f, &c, &browser, false, true).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        presence(&f, &c, &browser, true, false).await.0,
        StatusCode::OK
    );
    assert_eq!(capture(&f, &c, &operation).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        presence(&f, &c, &browser, true, true).await.0,
        StatusCode::OK
    );
    assert_eq!(capture(&f, &c, &operation).await.0, StatusCode::FORBIDDEN);
    let (_, source, _) = delegation(&f, &c, "user");
    let (status, receipt) = capture(&f, &c, &operation).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["state"], "queued");
    assert_eq!(receipt["artifact_ids"], json!([]));
    assert_eq!(receipt["inspection"], false);
    let (_, duplicate) = capture(&f, &c, &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(duplicate["operation_id"], operation);
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM input_authority WHERE message_id=?1", [source])
        .unwrap();
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn acceptance_is_once_and_uncertain_capture_is_never_replayed_even_with_a_new_uuid() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, true, true).await;
    capture(&f, &c, &operation).await;
    let (a, b) = tokio::join!(
        ack(&f, &c, &browser, &operation, "accepted", vec![]),
        ack(&f, &c, &browser, &operation, "accepted", vec![])
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::OK);
    assert_ne!(a.1["execute"], b.1["execute"]);
    assert_eq!(
        ack(&f, &c, &browser, &operation, "executed", vec![])
            .await
            .1["state"],
        "executed"
    );
    expire(&f, &c, &operation);
    assert_eq!(surface_status(&f, &c, &operation).await["state"], "unknown");
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .1["execute"],
        false
    );
    let (_, again) = capture(&f, &c, &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(again["operation_id"], operation);
    assert_eq!(again["state"], "unknown");
    let count: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM companion_surface_commands", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn saved_camera_upload_is_required_before_image_receipt_and_late_receipt_can_resolve_unknown()
{
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, true, true).await;
    capture(&f, &c, &operation).await;
    ack(&f, &c, &browser, &operation, "accepted", vec![]).await;
    assert_eq!(
        ack(
            &f,
            &c,
            &browser,
            &operation,
            "uploaded",
            vec![uuid::Uuid::new_v4().to_string()]
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(
        surface_status(&f, &c, &operation)
            .await
            .get("image")
            .is_none()
    );
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a3XcAAAAASUVORK5CYII=";
    let (_, image) = browser_request(
        &f,
        "POST",
        &format!("/api/v1/conversations/{}/attachments", c.room),
        Some(json!({"name":"camera.png","mime_type":"image/png","data":png})),
        "browser",
        true,
    )
    .await;
    let aid = image["id"].as_str().unwrap().to_owned();
    assert_eq!(
        ack(&f, &c, &browser, &operation, "uploaded", vec![aid.clone()])
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for (state, ids) in [("captured", vec![]), ("available", vec![aid.clone()])] {
        let (status,value)=browser_request(&f,"POST",&format!("/api/v1/companion/calls/{}/uploads",c.id),Some(json!({"upload_id":operation,"state":state,"source":"camera","name":"camera.png","attachment_ids":ids})),"browser",true).await;
        assert_eq!(status, StatusCode::OK, "{value}");
    }
    expire(&f, &c, &operation);
    assert_eq!(surface_status(&f, &c, &operation).await["state"], "unknown");
    let (status, receipt) = ack(&f, &c, &browser, &operation, "uploaded", vec![aid.clone()]).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["artifact_ids"], json!([aid]));
    assert_eq!(receipt["inspection"], false);
    let receipt = surface_status(&f, &c, &operation).await;
    assert_eq!(receipt["image"]["data"], png);
    assert_eq!(receipt["image"]["mimeType"], "image/png");
    assert_eq!(receipt["state"], "uploaded");
    for capability in [Some(false), None] {
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_surface_capabilities SET image_input=?2 WHERE call_id=?1",
                params![c.id, capability],
            )
            .unwrap();
        let limited = surface_status(&f, &c, &operation).await;
        assert_eq!(limited["artifact_ids"], receipt["artifact_ids"]);
        assert_eq!(limited["state"], "uploaded");
        assert_eq!(limited["inspection"], false);
        assert_eq!(limited["image_input"], json!(capability));
        assert!(limited.get("image").is_none());
        assert_eq!(
            limited["image_delivery"],
            if capability == Some(false) {
                "unsupported"
            } else {
                "unconfirmed"
            }
        );
    }
    assert_eq!(
        ack(&f, &c, &browser, &operation, "executed", vec![])
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn owner_browser_session_csrf_heartbeat_and_hangup_bound_all_controls() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    let path = format!("/api/v1/companion/calls/{}/surface/presence", c.id);
    let body = json!({"browser_id":browser,"camera_enabled":true,"capture_consent":true});
    assert_eq!(
        browser_request(&f, "POST", &path, Some(body.clone()), "browser", false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser_request(&f, "POST", &path, Some(body.clone()), "other-browser", true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let control = std::fs::read_to_string(f.home.path().join("secrets/control-token")).unwrap();
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("authorization", format!("Bearer {}", control.trim()))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        presence(&f, &c, &browser, true, true).await.0,
        StatusCode::OK
    );
    assert_eq!(
        presence(&f, &c, &uuid::Uuid::new_v4().to_string(), true, true)
            .await
            .0,
        StatusCode::CONFLICT
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_surfaces SET updated_at=?1",
            [now_ms() - 30_001],
        )
        .unwrap();
    assert_eq!(capture(&f, &c, &operation).await.0, StatusCode::BAD_REQUEST);
    presence(&f, &c, &browser, true, true).await;
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET heartbeat_at=?1",
            [now_ms() - 30_001],
        )
        .unwrap();
    assert_eq!(capture(&f, &c, &operation).await.0, StatusCode::BAD_REQUEST);
    f.s.db
        .lock()
        .unwrap()
        .execute("UPDATE companion_calls SET heartbeat_at=?1", [now_ms()])
        .unwrap();
    capture(&f, &c, &operation).await;
    assert_eq!(
        ack(
            &f,
            &c,
            &uuid::Uuid::new_v4().to_string(),
            &operation,
            "accepted",
            vec![]
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    f.s.db
        .lock()
        .unwrap()
        .execute("UPDATE companion_calls SET state='closed'", [])
        .unwrap();
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn view_commands_are_finite_owned_and_recoverable_without_reexecution() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, false, false).await;
    let (_, shown) = request(
        &f,
        &c,
        "POST",
        "shared",
        Some(json!({"url":"https://example.test/demo","title":"Demo"})),
        &c.token,
        false,
    )
    .await;
    let operation = uuid::Uuid::new_v4().to_string();
    let body = json!({"operation_id":operation,"delegation_id":"fixture-delegation","action":"focus_card","card_id":shown["card_id"]});
    let (status, receipt) =
        request(&f, &c, "POST", "view", Some(body.clone()), &c.token, false).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let (_, pending) = browser_request(
        &f,
        "GET",
        &format!(
            "/api/v1/companion/calls/{}/surface/commands?browser_id={browser}",
            c.id
        ),
        None,
        "browser",
        true,
    )
    .await;
    assert_eq!(pending["commands"][0]["operation_id"], operation);
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .1["execute"],
        true
    );
    assert_eq!(
        ack(&f, &c, &browser, &operation, "executed", vec![])
            .await
            .1["state"],
        "executed"
    );
    assert_eq!(
        request(&f, &c, "POST", "view", Some(body), &c.token, false)
            .await
            .1["state"],
        "executed"
    );
    assert_eq!(
        ack(&f, &c, &browser, &operation, "accepted", vec![])
            .await
            .1["execute"],
        false
    );
    for extra in [
        json!({"action":"open_url","url":"https://example.test"}),
        json!({"action":"scroll_down","script":"alert(1)"}),
        json!({"action":"focus_card","card_id":"999999"}),
        json!({"action":"scroll_down","card_id":shown["card_id"]}),
    ] {
        let mut body = json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"fixture-delegation"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_ne!(
            request(&f, &c, "POST", "view", Some(body), &c.token, false)
                .await
                .0,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn exact_native_surface_permission_remains_once_only_and_requires_browser_consent() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    let permission = |tool: &str, args: Value| json!({"toolCall":{"toolCallId":"surface-tool","title":format!("companion / {tool}"),"kind":"other","status":"pending","rawInput":{"server":"companion","tool":tool,"arguments":args}},"options":[{"optionId":"always","name":"Always","kind":"allow_always"},{"optionId":"once","name":"Once","kind":"allow_once"}]});
    let check = |tool: &str, args: Value| {
        companion_tools::permission_once(
            &f.s,
            &c.room,
            "owner",
            "default",
            true,
            &permission(tool, args),
        )
        .unwrap()
    };
    let capture = json!({"operation_id":operation,"delegation_id":"fixture-delegation"});
    assert!(check("companion_capture", capture.clone()).is_none());
    presence(&f, &c, &browser, false, false).await;
    assert!(check("companion_capture", capture.clone()).is_none());
    assert_eq!(
        check("companion_hangup", capture.clone()).unwrap()["outcome"]["optionId"],
        "once"
    );
    assert_eq!(check("companion_feedback",json!({"operation_id":operation,"delegation_id":"fixture-delegation","text":"Please add a shutter cue."})).unwrap()["outcome"]["optionId"],"once");
    assert!(
        check(
            "companion_hangup",
            json!({"operation_id":operation,"delegation_id":"other"})
        )
        .is_none()
    );
    assert!(check("companion_feedback",json!({"operation_id":operation,"delegation_id":"fixture-delegation","text":"Feedback","external_channel":"Slack"})).is_none());
    assert_eq!(check("companion_view",json!({"operation_id":operation,"delegation_id":"fixture-delegation","action":"scroll_down"})).unwrap()["outcome"]["optionId"],"once");
    presence(&f, &c, &browser, true, true).await;
    assert!(check("companion_capture", capture.clone()).is_some());
    assert!(
        check(
            "companion_capture",
            json!({"operation_id":operation,"delegation_id":"other"})
        )
        .is_none()
    );
    assert!(check("companion_capture",json!({"operation_id":operation,"delegation_id":"fixture-delegation","enable_camera":true})).is_none());
    assert!(
        check(
            "companion_surface_status",
            json!({"operation_id":operation})
        )
        .is_some()
    );
    std::fs::write(
        f.home.path().join("companion-actions.json"),
        json!({"enabled":false}).to_string(),
    )
    .unwrap();
    assert!(check("companion_capture", capture).is_none());
}

#[tokio::test]
async fn expiry_and_restart_never_restore_execution_authority_or_camera_consent() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, true, true).await;
    capture(&f, &c, &operation).await;
    expire(&f, &c, &operation);
    let (_, expired) = ack(&f, &c, &browser, &operation, "accepted", vec![]).await;
    assert_eq!(expired["execute"], false);
    assert_eq!(expired["state"], "unknown");
    let did = "second-explicit-request";
    delegation_named(&f, &c, "user", did);
    let second = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        request(
            &f,
            &c,
            "POST",
            "capture",
            Some(json!({"operation_id":second,"delegation_id":did})),
            &c.token,
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        ack(&f, &c, &browser, &second, "accepted", vec![]).await.1["execute"],
        true
    );
    agent_conversations::companion_surface::migrate(&f.s.db.lock().unwrap()).unwrap();
    assert_eq!(surface_status(&f, &c, &second).await["state"], "unknown");
    assert_eq!(
        ack(&f, &c, &browser, &second, "accepted", vec![]).await.0,
        StatusCode::FORBIDDEN
    );
    presence(&f, &c, &browser, true, true).await;
    assert_eq!(
        ack(&f, &c, &browser, &second, "accepted", vec![]).await.1["execute"],
        false
    );
    let (_, same) = request(
        &f,
        &c,
        "POST",
        "capture",
        Some(json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":did})),
        &c.token,
        false,
    )
    .await;
    assert_eq!(same["operation_id"], second);
    assert_eq!(same["state"], "unknown");
}

#[tokio::test]
async fn surface_queue_is_bounded_and_focus_cannot_cross_calls() {
    let f = fixture();
    let c = call(&f);
    let other = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, false, false).await;
    let (_, shown) = request(
        &f,
        &other,
        "POST",
        "shared",
        Some(json!({"url":"https://example.test/other","title":"Another private surface"})),
        &other.token,
        false,
    )
    .await;
    let (_, missing) = request(&f, &c, "POST", "view", Some(json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"fixture-delegation","action":"focus_card","card_id":shown["card_id"]})), &c.token, false).await;
    assert!(missing.get("error").is_some());
    for _ in 0..8 {
        assert_eq!(request(&f, &c, "POST", "view", Some(json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"fixture-delegation","action":"scroll_down"})), &c.token, false).await.0, StatusCode::OK);
    }
    assert_eq!(request(&f, &c, "POST", "view", Some(json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"fixture-delegation","action":"scroll_down"})), &c.token, false).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_new_login_cannot_claim_an_old_browsers_command_by_reusing_its_identifier() {
    let f = fixture();
    let c = call(&f);
    delegation(&f, &c, "user");
    let browser = uuid::Uuid::new_v4().to_string();
    let operation = uuid::Uuid::new_v4().to_string();
    presence(&f, &c, &browser, true, true).await;
    capture(&f, &c, &operation).await;
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'owner','csrf',?2)",
            params![auth::digest("new-login"), now_ms() + 60_000],
        )
        .unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_surfaces SET updated_at=?1",
            [now_ms() - 30_001],
        )
        .unwrap();
    let path = format!("/api/v1/companion/calls/{}/surface", c.id);
    assert_eq!(
        browser_request(
            &f,
            "POST",
            &format!("{path}/presence"),
            Some(json!({"browser_id":browser,"camera_enabled":true,"capture_consent":true})),
            "new-login",
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, pending) = browser_request(
        &f,
        "GET",
        &format!("{path}/commands?browser_id={browser}"),
        None,
        "new-login",
        true,
    )
    .await;
    assert_eq!(pending["commands"], json!([]));
    assert_eq!(
        browser_request(
            &f,
            "POST",
            &format!("{path}/commands/{operation}/ack"),
            Some(json!({"browser_id":browser,"state":"accepted"})),
            "new-login",
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(surface_status(&f, &c, &operation).await["state"], "queued");
}
