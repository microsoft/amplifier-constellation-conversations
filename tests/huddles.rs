use agent_conversations::{
    auth,
    config::{Agent, Config},
    huddles, server,
    state::{AppState, now_ms},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use rusqlite::params;
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
async fn parent(f: &F) -> (String, String) {
    let c = ok(
        f,
        "POST",
        "/api/v1/conversations",
        &f.token,
        Some(json!({"title":"Original request","agent_ids":[]})),
    )
    .await;
    let id = c["id"].as_str().unwrap().to_owned();
    let source = uid();
    ok(f,"POST",&format!("/api/v1/conversations/{id}/messages"),&f.token,Some(json!({"request_id":source,"text":"Please work together and report the checked result."}))).await;
    tokio::task::yield_now().await;
    (id, source)
}
async fn huddle(f: &F, parent: &str, source: &str) -> Value {
    ok(f,"POST",&format!("/api/v1/conversations/{parent}/huddles"),&f.token,Some(json!({"request_id":uid(),"title":"Work huddle","purpose":"Coordinate the requested review","source_message_id":source}))).await
}
async fn enroll(f: &F, room: &str, agent: &str) -> String {
    ok(
        f,
        "POST",
        &format!("/api/v1/huddles/{room}/participants"),
        &f.token,
        Some(json!({"agent_id":agent,"label":agent,"connection_id":format!("minecraft-{agent}")})),
    )
    .await["token"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn post(kind: &str) -> Value {
    let mut p = json!({"request_id":uid(),"text":"The route and materials are verified.","target_agent_ids":[],"kind":kind});
    if kind == "outcome" {
        p["outcome"] = json!({"kind":"result","evidence":[{"label":"Independent observation","reference":"fixture:verified-output"}]});
    }
    p
}
fn count(f: &F, table: &str) -> i64 {
    f.state
        .db
        .lock()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn quiet_coordination_inspection_join_and_exact_original_return_without_extra_engine() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let a = enroll(&f, room, "maker").await;
    let b = enroll(&f, room, "coach").await;
    let before = count(&f, "messages");
    let detail = ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/{room}"),
        &f.token,
        None,
    )
    .await;
    assert_eq!(detail["preference"]["mode"], "quiet");
    assert_eq!(count(&f, "messages"), before);
    assert_eq!(count(&f, "input_deliveries"), 0);
    let first = post("message");
    ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &a,
        Some(first.clone()),
    )
    .await;
    ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &b,
        Some(post("message")),
    )
    .await;
    assert_eq!(
        f.state.messages(&p, 100).unwrap().len(),
        1,
        "Routine posts must stay out of the parent"
    );
    let before_join = count(&f, "messages");
    ok(
        &f,
        "PUT",
        &format!("/api/v1/huddles/{room}/preference"),
        &f.token,
        Some(json!({"mode":"follow"})),
    )
    .await;
    assert_eq!(count(&f, "messages"), before_join);
    ok(&f,"POST",&format!("/api/v1/conversations/{room}/messages"),&f.token,Some(json!({"request_id":uid(),"text":"Please consider the alternate entrance.","target_agent_ids":["maker","coach"]}))).await;
    let page = ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/{room}/messages?after=0"),
        &a,
        None,
    )
    .await;
    assert_eq!(page["items"][0]["agent_id"], "maker");
    assert_eq!(page["items"][2]["role"], "user");
    assert_eq!(page["items"][2]["actor"], "owner");
    assert!(page["items"][2]["agent_id"].is_null());
    assert!(
        page["items"][0].get("post").is_none(),
        "Projection must not duplicate canonical text"
    );
    ok(
        &f,
        "PUT",
        &format!("/api/v1/huddles/{room}/preference"),
        &f.token,
        Some(json!({"mode":"quiet"})),
    )
    .await;
    let result = post("outcome");
    let receipt = ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &a,
        Some(result.clone()),
    )
    .await;
    assert_eq!(receipt["return"]["state"], "stored");
    assert_eq!(receipt["origin"]["message_id"], source);
    assert_eq!(
        receipt["text_sha256"],
        auth::digest(result["text"].as_str().unwrap())
    );
    let again = ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &a,
        Some(result.clone()),
    )
    .await;
    assert_eq!(receipt, again);
    let parent = f.state.messages(&p, 100).unwrap();
    assert_eq!(parent.len(), 2);
    assert!(
        parent[1]["text"]
            .as_str()
            .unwrap()
            .contains("Inspect coordination")
    );
    assert_eq!(count(&f, "input_deliveries"), 0);
    assert!(f.state.running.lock().unwrap().is_empty());
    if let Some(dir) = std::env::var_os("HUDDLE_FIXTURE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::path::PathBuf::from(dir);
        let descriptor = ok(&f, "GET", &format!("/api/v1/huddles/{room}"), &a, None).await;
        for (name, value) in [
            ("descriptor.json", descriptor),
            ("receipt.json", receipt),
            ("messages.json", page),
        ] {
            std::fs::write(dir.join(name), serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        }
    }
}

#[tokio::test]
async fn participant_cannot_forge_human_or_use_other_room_or_host_controls() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let other = huddle(&f, &p, &source).await;
    let other = other["id"].as_str().unwrap();
    for path in [
        "/api/v1/config".into(),
        format!("/api/v1/conversations/{p}"),
        format!("/api/v1/huddles/{other}"),
    ] {
        assert!(!req(&f, "GET", &path, &token, None).await.0.is_success());
    }
    assert!(
        !req(
            &f,
            "POST",
            &format!("/api/v1/conversations/{room}/messages"),
            &token,
            Some(json!({"request_id":uid(),"text":"I am human"}))
        )
        .await
        .0
        .is_success()
    );
    let mut forged = post("message");
    forged["actor"] = json!({"kind":"human","id":"owner"});
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(forged)
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(count(&f, "huddle_posts"), 0);
    ok(
        &f,
        "DELETE",
        &format!("/api/v1/huddles/{room}/participants/maker"),
        &f.token,
        None,
    )
    .await;
    assert_eq!(
        req(&f, "GET", &format!("/api/v1/huddles/{room}"), &token, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(post("message"))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn lost_post_ack_restart_status_and_conflict_do_not_duplicate_or_replay() {
    let mut f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap().to_owned();
    let token = enroll(&f, &room, "maker").await;
    let body = post("outcome");
    let request = body["request_id"].as_str().unwrap();
    let (_, ack) = req(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &token,
        Some(body.clone()),
    )
    .await;
    drop(ack);
    f.state = AppState::open(f.home.path().into()).unwrap();
    f.app = server::router(f.state.clone());
    let saved = ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/{room}/posts/{request}"),
        &token,
        None,
    )
    .await;
    assert_eq!(saved["status"], "saved");
    assert_eq!(saved["request_id"], request);
    assert_eq!(count(&f, "huddle_posts"), 1);
    assert_eq!(f.state.messages(&p, 100).unwrap().len(), 2);
    let mut changed = body.clone();
    changed["text"] = json!("Different work");
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(changed)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&f, "huddle_posts"), 1);
    assert_eq!(count(&f, "input_deliveries"), 0);
}

#[tokio::test]
async fn stop_is_explicit_revision_checked_and_history_remains_readable() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let d = ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/{room}"),
        &f.token,
        None,
    )
    .await;
    let stopped = ok(
        &f,
        "PUT",
        &format!("/api/v1/huddles/{room}/state"),
        &f.token,
        Some(json!({"state":"stopped","expected_revision":d["revision"]})),
    )
    .await;
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(post("message"))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/conversations/{room}/messages"),
            &f.token,
            Some(json!({"request_id":uid(),"text":"New coordination"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/{room}/messages"),
        &token,
        None,
    )
    .await;
    assert_eq!(
        req(
            &f,
            "PUT",
            &format!("/api/v1/huddles/{room}/state"),
            &f.token,
            Some(json!({"state":"open","expected_revision":d["revision"]}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    ok(
        &f,
        "PUT",
        &format!("/api/v1/huddles/{room}/state"),
        &f.token,
        Some(json!({"state":"open","expected_revision":stopped["revision"]})),
    )
    .await;
    ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &token,
        Some(post("message")),
    )
    .await;
}

#[tokio::test]
async fn create_does_not_copy_private_history_or_allow_request_substitution() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let body = json!({"request_id":uid(),"title":"Coordination","purpose":"Only share this purpose","source_message_id":source});
    let first = ok(
        &f,
        "POST",
        &format!("/api/v1/conversations/{p}/huddles"),
        &f.token,
        Some(body.clone()),
    )
    .await;
    let second = ok(
        &f,
        "POST",
        &format!("/api/v1/conversations/{p}/huddles"),
        &f.token,
        Some(body.clone()),
    )
    .await;
    assert_eq!(first["id"], second["id"]);
    assert_eq!(
        f.state
            .messages(first["id"].as_str().unwrap(), 100)
            .unwrap()
            .len(),
        0
    );
    let mut changed = body;
    changed["purpose"] = json!("Different purpose");
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/conversations/{p}/huddles"),
            &f.token,
            Some(changed)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let room = first["id"].as_str().unwrap();
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/conversations/{room}/agents"),
            &f.token,
            Some(json!({"agent_id":"default","share_history":true}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn external_origin_is_frozen_and_does_not_mirror_into_a_web_conversation() {
    let f = fixture();
    let origin = json!({"kind":"external","service":"minecraft","connection_id":"minecraft-maker","conversation_id":"public-world","request_id":uid(),"source_id":uid(),"source_revision":"stable-game-source","occurred_at":now_ms()-86_400_000,"actor":{"id":"actual-server-human-uuid","kind":"human"}});
    let d=ok(&f,"POST","/api/v1/huddles",&f.token,Some(json!({"request_id":uid(),"title":"Game coordination","purpose":"Review an existing plan","origin":origin}))).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let result = ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &token,
        Some(post("outcome")),
    )
    .await;
    assert_eq!(result["origin"], origin);
    assert_eq!(
        result["return"],
        json!({"state":"available","delivery":"domain_owned"})
    );
    assert_eq!(count(&f, "messages"), 1);
    let descriptor = ok(&f, "GET", &format!("/api/v1/huddles/{room}"), &token, None).await;
    assert_eq!(
        descriptor["participant"],
        json!({"agent_id":"maker","connection_id":"minecraft-maker"})
    );
    assert_eq!(descriptor["capabilities"], huddles::capabilities());
    assert!(descriptor.get("token").is_none());
}

#[tokio::test]
async fn changed_original_request_blocks_return_and_companion_projection() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET text='Changed request' WHERE id=?1",
            [&source],
        )
        .unwrap();
    let result = ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &token,
        Some(post("outcome")),
    )
    .await;
    assert_eq!(result["return"]["state"], "blocked");
    assert_eq!(f.state.messages(&p, 100).unwrap().len(), 1);
    assert!(
        huddles::outcomes_for_parent(&f.state.db.lock().unwrap(), &p, "owner")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn paging_and_outcomes_keep_original_identity_and_distinct_results() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    for _ in 0..3 {
        ok(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(post("outcome")),
        )
        .await;
    }
    let a = ok(
        &f,
        "GET",
        &format!("/api/v1/huddles/{room}/messages?after=0&limit=2"),
        &token,
        None,
    )
    .await;
    assert_eq!(a["items"].as_array().unwrap().len(), 2);
    assert_eq!(a["has_more"], true);
    let b = ok(
        &f,
        "GET",
        &format!(
            "/api/v1/huddles/{room}/messages?after={}&limit=2",
            a["next_cursor"]
        ),
        &token,
        None,
    )
    .await;
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    assert_eq!(b["has_more"], false);
    let db = f.state.db.lock().unwrap();
    let outcomes = huddles::outcomes_for_parent(&db, &p, "owner").unwrap();
    assert_eq!(outcomes.len(), 3);
    assert!(
        huddles::outcomes_for_parent(&db, &p, "robin")
            .unwrap()
            .is_empty()
    );
    for v in outcomes {
        assert_eq!(v["origin"]["message_id"], source);
        assert_eq!(v["message"]["agent_id"], "maker");
        assert_eq!(v["revision"].as_str().unwrap().len(), 64);
    }
}

#[tokio::test]
async fn malformed_payload_and_unknown_targets_do_not_mutate_the_room() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let count_before = count(&f, "messages");
    for body in [
        json!({"request_id":uid(),"text":"x".repeat(6001),"kind":"message"}),
        json!({"request_id":uid(),"text":"Hello","kind":"message","target_agent_ids":["not-enrolled"]}),
        json!({"request_id":uid(),"text":"False kind","kind":"human"}),
        json!({"request_id":uid(),"text":"No outcome data","kind":"outcome"}),
    ] {
        assert_eq!(
            req(
                &f,
                "POST",
                &format!("/api/v1/huddles/{room}/posts"),
                &token,
                Some(body)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(count(&f, "messages"), count_before);
    assert_eq!(count(&f, "huddle_posts"), 0);
}

#[tokio::test]
async fn revoked_human_membership_blocks_inspection_without_destroying_canonical_posts() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    ok(
        &f,
        "POST",
        &format!("/api/v1/conversations/{room}/members"),
        &f.token,
        Some(json!({"username":"robin","share_history":true})),
    )
    .await;
    let cookie = auth::random_token();
    f.state.db.lock().unwrap().execute("INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'robin','fixture-csrf',?2)",params![auth::digest(&cookie),now_ms()+60000]).unwrap();
    async fn view(f: &F, room: &str, cookie: &str) -> StatusCode {
        f.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/huddles/{room}"))
                    .header(
                        header::COOKIE,
                        format!("{}={cookie}", auth::cookie_name(&f.state)),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
    assert_eq!(view(&f, room, &cookie).await, StatusCode::OK);
    ok(
        &f,
        "DELETE",
        &format!("/api/v1/conversations/{room}/members/robin"),
        &f.token,
        None,
    )
    .await;
    assert_eq!(view(&f, room, &cookie).await, StatusCode::NOT_FOUND);
    assert_eq!(count(&f, "huddles"), 1);
}

#[tokio::test]
async fn discovery_stays_compact_and_exact_pending_outcome_survives_recent_window() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let mut first = String::new();
    for i in 0..51 {
        let mut body = post("outcome");
        body["text"] = json!(format!("Result {i}: {}", "x".repeat(5900)));
        if i == 0 {
            first = body["request_id"].as_str().unwrap().into();
        }
        ok(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(body),
        )
        .await;
    }
    let list = ok(
        &f,
        "GET",
        &format!("/api/v1/conversations/{p}/huddles"),
        &f.token,
        None,
    )
    .await;
    assert_eq!(list["items"][0]["outcome_summary"]["count"], 51);
    assert!(list["items"][0].get("outcomes").is_none());
    assert!(serde_json::to_vec(&list).unwrap().len() < 4000);
    let self_list = ok(
        &f,
        "GET",
        &format!("/api/v1/conversations/{room}/huddles"),
        &f.token,
        None,
    )
    .await;
    assert_eq!(self_list["current"]["id"], room);
    assert!(self_list["current"].get("outcomes").is_none());
    assert_eq!(
        f.state.conversation_info(room).unwrap().unwrap()["huddle"]["state"],
        "open"
    );
    let db = f.state.db.lock().unwrap();
    let recent = huddles::outcomes_for_parent(&db, &p, "owner").unwrap();
    assert_eq!(recent.len(), 50);
    assert!(!recent.iter().any(|r| r["message"]["id"] == first));
    let exact = huddles::outcome_for_parent(&db, &p, "owner", &first)
        .unwrap()
        .unwrap();
    assert_eq!(exact["message"]["id"], first);
    db.execute(
        "UPDATE messages SET text='edited original' WHERE id=?1",
        [source],
    )
    .unwrap();
    assert!(
        huddles::outcome_for_parent(&db, &p, "owner", &first)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn existing_stop_control_stops_external_huddle_without_erasing_history() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let saved = post("message");
    ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &token,
        Some(saved.clone()),
    )
    .await;
    ok(
        &f,
        "POST",
        &format!("/api/v1/conversations/{room}/cancel"),
        &f.token,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            &token,
            Some(post("message"))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let status = ok(
        &f,
        "GET",
        &format!(
            "/api/v1/huddles/{room}/posts/{}",
            saved["request_id"].as_str().unwrap()
        ),
        &token,
        None,
    )
    .await;
    assert_eq!(status["status"], "saved");
}

#[tokio::test]
async fn reassigned_connection_cannot_adopt_prior_connection_operation() {
    let f = fixture();
    let (p, source) = parent(&f).await;
    let d = huddle(&f, &p, &source).await;
    let room = d["id"].as_str().unwrap();
    let token = enroll(&f, room, "maker").await;
    let saved = post("message");
    ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/posts"),
        &token,
        Some(saved.clone()),
    )
    .await;
    let changed = ok(
        &f,
        "POST",
        &format!("/api/v1/huddles/{room}/participants"),
        &f.token,
        Some(json!({"agent_id":"maker","label":"Maker","connection_id":"another-installation"})),
    )
    .await;
    let new_token = changed["token"].as_str().unwrap();
    assert_eq!(
        req(
            &f,
            "GET",
            &format!(
                "/api/v1/huddles/{room}/posts/{}",
                saved["request_id"].as_str().unwrap()
            ),
            new_token,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        req(
            &f,
            "POST",
            &format!("/api/v1/huddles/{room}/posts"),
            new_token,
            Some(saved)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&f, "huddle_posts"), 1);
}
