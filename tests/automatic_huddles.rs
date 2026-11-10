use agent_conversations::{
    auth,
    config::{Agent, Config},
    huddle_auto, server,
    state::{AppState, now_ms},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

struct F {
    home: TempDir,
    state: Arc<AppState>,
    app: Router,
    token: String,
}
fn fixture() -> F {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uid(),
        label: "Huddle fixture".into(),
        owner: "owner".into(),
        allowed_users: vec!["robin".into()],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://huddles.example".into(),
        pam_service: "agent-conversations".into(),
        tls_cert: home.path().join("cert"),
        tls_key: home.path().join("key"),
        tls_ca: None,
        import_source: "fixture".into(),
        import_surface: "fixture".into(),
        agent: Agent {
            label: "Not launched".into(),
            command: "does-not-exist".into(),
            args: vec![],
            cwd: home.path().to_string_lossy().into_owned(),
            session_cwd: None,
            env: BTreeMap::new(),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        },
    }
    .save(home.path())
    .unwrap();
    let state = AppState::open(home.path().into()).unwrap();
    let app = server::router(state.clone());
    let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    F {
        home,
        state,
        app,
        token,
    }
}
fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}
async fn req(
    f: &F,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let b = if let Some(v) = body {
        r = r.header(header::CONTENT_TYPE, "application/json");
        Body::from(v.to_string())
    } else {
        Body::empty()
    };
    let res = f.app.clone().oneshot(r.body(b).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 2 * 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn ok(f: &F, method: &str, path: &str, token: &str, body: Option<Value>) -> Value {
    let (status, v) = req(f, method, path, token, body).await;
    assert!(status.is_success(), "{method} {path}: {status} {v}");
    v
}
fn registration(name: &str, ns: &str) -> Value {
    json!({"agent_id":name,"label":name,"connection_id":format!("minecraft-{name}"),"service":"minecraft","conversation_id":format!("world:{ns}"),"namespace":ns,"local_agent_id":"default","identity":{"player_uuid":uid(),"player_name":name},"enabled":true,"token":auth::random_token()})
}
async fn registered(f: &F, name: &str, ns: &str) -> Value {
    let v = registration(name, ns);
    ok(
        f,
        "PUT",
        &format!("/api/v1/huddle-installations/{name}"),
        &f.token,
        Some(v.clone()),
    )
    .await;
    v
}
fn ensure_body(name: &str, ns: &str, peers: Vec<&str>) -> Value {
    json!({"request_id":uid(),"title":"Check the layout together","purpose":"Review details quietly and return a useful result","origin":{"kind":"external","service":"minecraft","connection_id":format!("minecraft-{name}"),"conversation_id":format!("world:{ns}"),"request_id":uid(),"source_id":uid(),"source_revision":"retained-source-sha","occurred_at":now_ms(),"actor":{"id":"original-player-uuid","kind":"human"}},"peer_installation_ids":peers})
}
fn token(v: &Value) -> &str {
    v["token"].as_str().unwrap()
}
async fn ensure(f: &F, reg: &Value, b: Value) -> Value {
    ok(f, "POST", "/api/v1/huddles/ensure", token(reg), Some(b)).await
}
fn message(kind: &str) -> Value {
    let mut v = json!({"request_id":uid(),"text":"The two measurements agree.","target_agent_ids":[],"kind":kind});
    if kind == "outcome" {
        v["outcome"] = json!({"kind":"result","evidence":[{"label":"Measurements","reference":"receipt:actual"}]});
    }
    v
}
#[tokio::test]
async fn no_room_setup_ensure_invites_peer_and_reuses_one_source() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let b = registered(&f, "peer", "world").await;
    registered(&f, "foreign", "private").await;
    let empty = ok(&f, "GET", "/api/v1/huddles/discovery", token(&a), None).await;
    assert_eq!(empty["items"], json!([]));
    let roster = ok(&f, "GET", "/api/v1/huddles/roster", token(&a), None).await;
    assert_eq!(roster["items"].as_array().unwrap().len(), 2);
    assert_eq!(roster["items"][0]["availability"], "unknown");
    assert!(!roster.to_string().contains(token(&b)));
    let mut body = ensure_body("lead", "world", vec!["peer"]);
    let room = ensure(&f, &a, body.clone()).await;
    assert_eq!(room["membership"], "active");
    assert_eq!(room["participants"].as_array().unwrap().len(), 2);
    assert_eq!(room["preference"]["mode"], "quiet");
    let peer = ok(&f, "GET", "/api/v1/huddles/discovery", token(&b), None).await;
    assert_eq!(peer["items"][0]["id"], room["id"]);
    assert_eq!(peer["items"][0]["origin"], body["origin"]);
    let dup = ensure(&f, &a, body.clone()).await;
    assert_eq!(dup["id"], room["id"]);
    body["request_id"] = json!(uid());
    body["title"] = json!("Reusable work room");
    let reused = ensure(&f, &a, body).await;
    assert_eq!(reused["id"], room["id"]);
    assert_eq!(reused["reused"], true);
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM huddles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn lost_ensure_ack_reconciles_after_reopen_without_new_room_or_token() {
    let mut f = fixture();
    let a = registered(&f, "lead", "world").await;
    registered(&f, "peer", "world").await;
    let body = ensure_body("lead", "world", vec!["peer"]);
    let room = ensure(&f, &a, body.clone()).await;
    f.state = AppState::open(f.home.path().into()).unwrap();
    f.app = server::router(f.state.clone());
    let status = ok(
        &f,
        "GET",
        &format!(
            "/api/v1/huddles/ensure/{}",
            body["request_id"].as_str().unwrap()
        ),
        token(&a),
        None,
    )
    .await;
    assert_eq!(status["id"], room["id"]);
    assert_eq!(status["status"], "saved");
    assert_eq!(ensure(&f, &a, body).await["id"], room["id"]);
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM huddle_auto_members", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
#[tokio::test]
async fn drift_and_request_conflict_never_mutate_the_saved_room() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let body = ensure_body("lead", "world", vec![]);
    ensure(&f, &a, body.clone()).await;
    let mut changed = body.clone();
    changed["purpose"] = json!("Another operation");
    assert_eq!(
        req(
            &f,
            "POST",
            "/api/v1/huddles/ensure",
            token(&a),
            Some(changed)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for field in ["source_revision", "occurred_at", "actor", "request_id"] {
        let mut changed = body.clone();
        changed["request_id"] = json!(uid());
        changed["origin"][field] = match field {
            "occurred_at" => json!(now_ms() - 50),
            "actor" => json!({"id":"other","kind":"human"}),
            _ => json!("changed"),
        };
        assert_eq!(
            req(
                &f,
                "POST",
                "/api/v1/huddles/ensure",
                token(&a),
                Some(changed)
            )
            .await
            .0,
            StatusCode::CONFLICT,
            "{field}"
        );
    }
    let db = f.state.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM huddle_ensures", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM huddles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn distinct_actual_source_is_not_fuzzily_collapsed() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let mut body = ensure_body("lead", "world", vec![]);
    let first = ensure(&f, &a, body.clone()).await;
    body["request_id"] = json!(uid());
    body["origin"]["source_id"] = json!(uid());
    let second = ensure(&f, &a, body).await;
    assert_ne!(first["id"], second["id"]);
}
#[tokio::test]
async fn source_and_roster_scopes_fail_atomically_without_creating_room() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    registered(&f, "foreign", "private").await;
    let b = ensure_body("lead", "world", vec!["foreign"]);
    assert_eq!(
        req(&f, "POST", "/api/v1/huddles/ensure", token(&a), Some(b))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut b = ensure_body("lead", "world", vec![]);
    b["origin"]["conversation_id"] = json!("world:elsewhere");
    assert_eq!(
        req(&f, "POST", "/api/v1/huddles/ensure", token(&a), Some(b))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut b = ensure_body("lead", "world", vec![]);
    b["origin"] = json!({"kind":"conversation","conversation_id":uid(),"request_id":uid(),"message_id":uid(),"source_revision":"a","occurred_at":now_ms(),"actor":{"id":"owner","kind":"human"}});
    assert_eq!(
        req(&f, "POST", "/api/v1/huddles/ensure", token(&a), Some(b))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM huddles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn stopped_room_is_reused_without_reopen_or_invitation() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    registered(&f, "late", "world").await;
    let mut body = ensure_body("lead", "world", vec![]);
    let room = ensure(&f, &a, body.clone()).await;
    ok(
        &f,
        "PUT",
        &format!("/api/v1/huddles/{}/state", room["id"].as_str().unwrap()),
        &f.token,
        Some(json!({"state":"stopped","expected_revision":room["revision"]})),
    )
    .await;
    body["request_id"] = json!(uid());
    body["peer_installation_ids"] = json!(["late"]);
    let same = ensure(&f, &a, body).await;
    assert_eq!(same["id"], room["id"]);
    assert_eq!(same["state"], "stopped");
    assert_eq!(same["participants"].as_array().unwrap().len(), 1);
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{}/posts", room["id"].as_str().unwrap()),
            token(&a),
            Some(message("message"))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn revoked_membership_survives_reuse_and_discovery_without_regrant() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let b = registered(&f, "peer", "world").await;
    let mut body = ensure_body("lead", "world", vec!["peer"]);
    let room = ensure(&f, &a, body.clone()).await;
    let id = room["id"].as_str().unwrap();
    ok(
        &f,
        "DELETE",
        &format!("/api/v1/huddles/{id}/participants/peer"),
        &f.token,
        None,
    )
    .await;
    body["request_id"] = json!(uid());
    ensure(&f, &a, body.clone()).await;
    let peer = ok(&f, "GET", "/api/v1/huddles/discovery", token(&b), None).await;
    assert_eq!(peer["items"][0]["membership"], "revoked");
    assert!(peer["items"][0].get("origin").is_none());
    assert_eq!(
        req(&f, "GET", &format!("/api/v1/huddles/{id}"), token(&b), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    ok(
        &f,
        "DELETE",
        &format!("/api/v1/huddles/{id}/participants/lead"),
        &f.token,
        None,
    )
    .await;
    body["request_id"] = json!(uid());
    let revoked = ensure(&f, &a, body).await;
    assert_eq!(revoked["membership"], "revoked");
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM huddles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn peer_post_preserves_actor_targets_source_and_no_implicit_execution() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let b = registered(&f, "peer", "world").await;
    let original = ensure_body("lead", "world", vec!["peer"]);
    let room = ensure(&f, &a, original.clone()).await;
    let path = format!("/api/v1/huddles/{}/posts", room["id"].as_str().unwrap());
    let mut post = message("outcome");
    post["source"] = json!({"id":uid(),"revision":"exact","occurred_at":now_ms()-10,"conversation_id":room["id"]});
    post["target_agent_ids"] = json!(["lead"]);
    let result = ok(&f, "POST", &path, token(&b), Some(post.clone())).await;
    assert_eq!(result["agent_id"], "peer");
    assert_eq!(result["origin"], original["origin"]);
    assert_eq!(
        result["return"],
        json!({"state":"available","delivery":"domain_owned"})
    );
    assert_eq!(result["source"], post["source"]);
    assert_eq!(
        ok(&f, "POST", &path, token(&b), Some(post.clone())).await,
        result
    );
    let saved = ok(
        &f,
        "GET",
        &format!("{path}/{}", post["request_id"].as_str().unwrap()),
        token(&b),
        None,
    )
    .await;
    assert_eq!(saved, result);
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let mut spoof = message("message");
    spoof["actor"] = json!("owner");
    assert!(
        !req(&f, "POST", &path, token(&b), Some(spoof))
            .await
            .0
            .is_success()
    );
    assert_eq!(
        req(&f, "GET", "/api/v1/huddle-installations", token(&b), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
async fn installation_rotation_disable_and_identity_preserve_isolation() {
    let f = fixture();
    let mut a = registered(&f, "lead", "world").await;
    let oldtoken = token(&a).to_owned();
    let room = ensure(&f, &a, ensure_body("lead", "world", vec![])).await;
    a["token"] = json!(auth::random_token());
    ok(
        &f,
        "PUT",
        "/api/v1/huddle-installations/lead",
        &f.token,
        Some(a.clone()),
    )
    .await;
    assert_eq!(
        req(&f, "GET", "/api/v1/huddles/discovery", &oldtoken, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ok(&f, "GET", "/api/v1/huddles/discovery", token(&a), None).await["items"][0]["id"],
        room["id"]
    );
    let mut drift = a.clone();
    drift["conversation_id"] = json!("elsewhere");
    assert_eq!(
        req(
            &f,
            "PUT",
            "/api/v1/huddle-installations/lead",
            &f.token,
            Some(drift)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    a["enabled"] = json!(false);
    ok(
        &f,
        "PUT",
        "/api/v1/huddle-installations/lead",
        &f.token,
        Some(a.clone()),
    )
    .await;
    assert_eq!(
        req(&f, "GET", "/api/v1/huddles/discovery", token(&a), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn bounded_membership_pages_and_local_choices_have_no_tokens() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let mut roomids = Vec::new();
    for _ in 0..3 {
        roomids.push(ensure(&f, &a, ensure_body("lead", "world", vec![])).await["id"].clone());
    }
    let one = ok(
        &f,
        "GET",
        "/api/v1/huddles/discovery?limit=2",
        token(&a),
        None,
    )
    .await;
    assert_eq!(one["items"].as_array().unwrap().len(), 2);
    assert_eq!(one["has_more"], true);
    assert_eq!(one["coverage"], "current_memberships");
    let two = ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/discovery?limit=2&after={}", one["cursor"]),
        token(&a),
        None,
    )
    .await;
    assert_eq!(two["items"].as_array().unwrap().len(), 1);
    assert_eq!(two["items"][0]["id"], roomids[2]);
    let choices = huddle_auto::local_installations(&f.state, "owner", "default").unwrap();
    assert_eq!(choices.len(), 1);
    assert!(!serde_json::to_string(&choices).unwrap().contains(token(&a)));
    assert!(huddle_auto::local_installations(&f.state, "robin", "default").is_err());
}
#[tokio::test]
async fn same_agent_different_connection_does_not_ambiguously_join_twice() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let mut second = registration("lead", "world");
    second["connection_id"] = json!("second-connection");
    ok(
        &f,
        "PUT",
        "/api/v1/huddle-installations/other",
        &f.token,
        Some(second),
    )
    .await;
    let body = ensure_body("lead", "world", vec!["other"]);
    assert_eq!(
        req(&f, "POST", "/api/v1/huddles/ensure", token(&a), Some(body))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM huddles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn current_call_guard_is_atomic_and_web_results_use_original_parent() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let b = registered(&f, "peer", "world").await;
    let parent = f.state.create("Human original").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = uid();
    ok(
        &f,
        "POST",
        &format!("/api/v1/conversations/{parent}/messages"),
        &f.token,
        Some(json!({"request_id":source,"text":"Review this together"})),
    )
    .await;
    let at: i64 = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT created_at FROM messages WHERE id=?1",
            [&source],
            |r| r.get(0),
        )
        .unwrap();
    let origin = json!({"kind":"conversation","conversation_id":parent,"request_id":source,"message_id":source,"source_revision":auth::digest("Review this together"),"occurred_at":at,"actor":{"id":"owner","kind":"human"}});
    let mut raw = ensure_body("lead", "world", vec!["peer"]);
    raw["origin"] = origin;
    let body: huddle_auto::Ensure = serde_json::from_value(raw).unwrap();
    let principal = huddle_auto::local_installations(&f.state, "owner", "default")
        .unwrap()
        .into_iter()
        .find(|i| i.id == "lead")
        .unwrap();
    assert!(
        huddle_auto::ensure_for_guarded(&f.state, &principal, body.clone(), |_| Err(
            agent_conversations::error::ApiError::forbidden()
        ))
        .is_err()
    );
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM huddles", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let room = huddle_auto::ensure_for_guarded(&f.state, &principal, body, |_| Ok(())).unwrap();
    let path = format!("/api/v1/huddles/{}/posts", room["id"].as_str().unwrap());
    ok(&f, "POST", &path, token(&b), Some(message("message"))).await;
    let post = message("outcome");
    let result = ok(&f, "POST", &path, token(&b), Some(post.clone())).await;
    assert_eq!(result["return"]["conversation_id"], parent);
    assert_eq!(result["return"]["state"], "stored");
    ok(&f, "POST", &path, token(&b), Some(post)).await;
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation_id=?1 AND role='assistant'",
                [parent],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(!room.to_string().contains(token(&a)));
}

#[tokio::test]
async fn explicit_manual_replacement_does_not_leave_old_installation_authorized() {
    let f = fixture();
    let a = registered(&f, "lead", "world").await;
    let body = ensure_body("lead", "world", vec![]);
    let room = ensure(&f, &a, body).await;
    let id = room["id"].as_str().unwrap();
    let replacement = ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{id}/participants"),
        &f.token,
        Some(json!({"agent_id":"lead","label":"Replacement","connection_id":"another-connection"})),
    )
    .await;
    let old = ok(&f, "GET", "/api/v1/huddles/discovery", token(&a), None).await;
    assert_eq!(old["items"][0]["membership"], "revoked");
    assert_eq!(
        req(&f, "GET", &format!("/api/v1/huddles/{id}"), token(&a), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        req(
            &f,
            "GET",
            &format!("/api/v1/huddles/{id}"),
            replacement["token"].as_str().unwrap(),
            None
        )
        .await
        .0
        .is_success()
    );
}
