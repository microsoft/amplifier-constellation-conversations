use agent_conversations::{
    config::{Agent, Config},
    server,
    state::AppState,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    home: TempDir,
    state: Arc<AppState>,
    app: Router,
    token: String,
}
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    Config::local(home.path(), "Companion", 45128)
        .unwrap()
        .save(home.path())
        .unwrap();
    let state = AppState::open(home.path().into()).unwrap();
    let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    let app = server::router(state.clone());
    Fixture {
        home,
        state,
        app,
        token,
    }
}
async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Option<Value>,
    host: &str,
    admin: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json");
    if admin {
        builder = builder.header("authorization", format!("Bearer {}", f.token.trim()));
    }
    let response = f
        .app
        .clone()
        .oneshot(
            builder
                .body(body.map(|b| Body::from(b.to_string())).unwrap_or_default())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn seed_agent(f: &Fixture) -> Agent {
    let binary = std::env::current_exe().unwrap();
    let agent = Agent {
        label: "Existing agent".into(),
        command: binary.to_string_lossy().into_owned(),
        args: vec![],
        cwd: f.home.path().to_string_lossy().into_owned(),
        session_cwd: None,
        env: BTreeMap::new(),
        mcp_servers: vec![],
        timeout_seconds: 300,
        client: Default::default(),
    };
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO agent_endpoints(id,config,updated_at) VALUES('saved',?1,1)",
            [serde_json::to_string(&agent).unwrap()],
        )
        .unwrap();
    agent
}

#[tokio::test]
async fn fresh_zero_agent_setup_is_local_and_has_no_fake_endpoint_or_session() {
    let f = fixture();
    let (status, v) = request(&f, "GET", "/auth/session", None, "127.0.0.1:45128", false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["first_account_needed"], true);
    let (_, meta) = request(&f, "GET", "/api/v1/meta", None, "127.0.0.1:45128", true).await;
    assert!(meta["agent"].is_null());
    let (_, setup) = request(
        &f,
        "GET",
        "/api/v1/companion/setup",
        None,
        "127.0.0.1:45128",
        true,
    )
    .await;
    assert_eq!(setup["agents"], json!([]));
    assert_eq!(setup["provider_verified"], false);
    let (status, _) = request(
        &f,
        "POST",
        "/api/v1/conversations",
        Some(json!({"title":"No selected agent"})),
        "127.0.0.1:45128",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for table in ["agent_endpoints", "conversations", "companion_calls"] {
        let count: i64 = f
            .state
            .db
            .lock()
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
    assert!(!f.home.path().join("companion.json").exists());
}

#[tokio::test]
async fn local_host_gate_precedes_bearer_and_setup_requires_admin() {
    let f = fixture();
    for host in [
        "attacker.invalid:45128",
        "127.0.0.1:45129",
        "localhost:45128",
    ] {
        assert_eq!(
            request(&f, "GET", "/api/v1/companion/setup", None, host, true)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &f,
            "GET",
            "/api/v1/companion/setup",
            None,
            "127.0.0.1:45128",
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/auth/session")
                .header("host", "127.0.0.1:45128")
                .header("origin", "http://attacker.invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn selecting_saved_contact_twice_preserves_endpoint_room_session_grants_and_profile() {
    let f = fixture();
    let agent = seed_agent(&f);
    let room = f
        .state
        .create_room("Existing room", "owner", &["saved".into()])
        .unwrap();
    let id = room["id"].as_str().unwrap();
    f.state.db.lock().unwrap().execute("UPDATE room_agents SET session_id='original-session',endpoint='original-endpoint',last_seq=91 WHERE conversation_id=?1",[id]).unwrap();
    let bridge = f.home.path().join("bridge.mjs");
    std::fs::write(&bridge, b"never invoked").unwrap();
    let profile = json!({"voice":"cedar","context_registered":true,"identity":"Retain identity","description":"Retain description","connections":[{"name":"Saved world","description":"Retain source evidence"}]});
    let before = json!({"enabled":true,"node":std::env::current_exe().unwrap(),"bridge":bridge,"max_call_seconds":45,"profiles":{"saved":profile}});
    agent_conversations::config::private_write(
        &f.home.path().join("companion.json"),
        &serde_json::to_vec(&before).unwrap(),
    )
    .unwrap();
    for _ in 0..2 {
        let (status, result) = request(
            &f,
            "POST",
            "/api/v1/companion/setup",
            Some(json!({"agent_id":"saved","voice":"marin"})),
            "127.0.0.1:45128",
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["added"], false);
        assert_eq!(result["provider_verified"], false);
    }
    let after: Value =
        serde_json::from_slice(&std::fs::read(f.home.path().join("companion.json")).unwrap())
            .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        serde_json::to_value(f.state.agent("saved").unwrap().unwrap()).unwrap(),
        serde_json::to_value(agent).unwrap()
    );
    let retained: (String, String, i64) = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT session_id,endpoint,last_seq FROM room_agents WHERE conversation_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        retained,
        ("original-session".into(), "original-endpoint".into(), 91)
    );
    let count: i64 = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM companion_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn setup_does_not_replace_bad_saved_configuration() {
    let f = fixture();
    seed_agent(&f);
    std::fs::write(
        f.home.path().join("companion.json"),
        b"malformed saved data",
    )
    .unwrap();
    let body =
        json!({"agent_id":"saved","voice":"marin","openai_key":"sk-fixture-must-not-be-saved"});
    let (status, result) = request(
        &f,
        "POST",
        "/api/v1/companion/setup",
        Some(body),
        "127.0.0.1:45128",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!result.to_string().contains("sk-fixture"));
    assert_eq!(
        std::fs::read(f.home.path().join("companion.json")).unwrap(),
        b"malformed saved data"
    );
    assert!(!f.home.path().join("secrets/openai-api-key").exists());
    let count: i64 = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM native_update_activity", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}
