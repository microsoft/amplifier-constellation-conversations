use agent_conversations::{
    auth,
    config::{Agent, Config},
    context, server,
    state::{AppState, now_ms},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    _home: TempDir,
    state: Arc<AppState>,
    app: Router,
    admin: String,
    context: String,
}
#[tokio::test]
async fn configured_embedding_origins_load_and_only_allow_explicit_https_hosts() {
    let f = fixture();
    let mut config = f.state.config.clone();
    config.embed_origins = vec!["https://fleet.example:18455".into()];
    config.save(f._home.path()).unwrap();
    let loaded = AppState::open(f._home.path().into()).unwrap();
    let response = server::router(loaded)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'self' https://fleet.example:18455;")
    );
    for invalid in [
        "*",
        "https://*.example",
        "http://fleet.example",
        "https://fleet.example/path",
        "https://fleet.example; script-src *",
        "https://user:password@fleet.example",
    ] {
        config.embed_origins = vec![invalid.into()];
        assert!(config.validate().is_err(), "{invalid}");
    }
    let mut legacy = serde_json::to_value(&f.state.config).unwrap();
    legacy.as_object_mut().unwrap().remove("embed_origins");
    assert!(
        serde_json::from_value::<Config>(legacy)
            .unwrap()
            .embed_origins
            .is_empty()
    );
}
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let mut env = BTreeMap::new();
    env.insert(
        "FIXTURE_LOG".into(),
        home.path()
            .join("frames.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    let c = Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Test conversations".into(),
        owner: "owner".into(),
        allowed_users: vec!["robin".into(), "casey".into()],
        embed_origins: Vec::new(),
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://example.test:18445".into(),
        pam_service: "agent-conversations".into(),
        tls_cert: home.path().join("cert.pem"),
        tls_key: home.path().join("key.pem"),
        tls_ca: None,
        import_source: "toad".into(),
        import_surface: "toad".into(),
        auth_mode: Default::default(),
        transport: Default::default(),
        agent: Some(Agent {
            label: "Existing individual".into(),
            command: "python3".into(),
            args: vec![format!(
                "{}/tests/fixture_agent.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            cwd: home.path().to_string_lossy().into_owned(),
            session_cwd: None,
            env,
            mcp_servers: vec![
                json!({"name":"source","type":"http","url":"https://example.test/mcp","headers":[{"name":"Authorization","value":"Bearer fixture-only"}]}),
            ],
            timeout_seconds: 30,
            client: Default::default(),
        }),
    };
    c.save(home.path()).unwrap();
    let state = AppState::open(home.path().to_owned()).unwrap();
    let app = server::router(state.clone());
    let admin = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    let context = std::fs::read_to_string(home.path().join("secrets/context-token")).unwrap();
    Fixture {
        _home: home,
        state,
        app,
        admin,
        context,
    }
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let data = if let Some(body) = body {
        req = req.header(header::CONTENT_TYPE, "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    let response = app.clone().oneshot(req.body(data).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn auth_scopes_and_provider_credentials_are_separate() {
    let f = fixture();
    assert_eq!(
        request(&f.app, "GET", "/api/v1/conversations", None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.app,
            "GET",
            "/api/v1/conversations",
            Some(&f.context),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/conversations",
            Some(&f.context),
            Some(json!({"title":"no"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            None,
            Some(json!({"operation":"list","access":{"mode":"owner"}}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            Some(&f.context),
            Some(json!({"operation":"list","access":{"mode":"owner"}}))
        )
        .await
        .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn browser_cookie_multiple_headers_and_csrf() {
    let f = fixture();
    let token = "browser-fixture";
    let csrf = "csrf-fixture";
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'owner',?2,?3)",
            rusqlite::params![auth::digest(token), csrf, now_ms() + 60000],
        )
        .unwrap();
    let build = |method: &str, path: &str, csrf: Option<&str>, origin: &str| {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header(header::COOKIE, "other_application=first")
            .header(
                header::COOKIE,
                format!("{}={token}", auth::cookie_name(&f.state)),
            )
            .header(header::HOST, "example.test:18445")
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(csrf) = csrf {
            r = r.header("x-csrf-token", csrf);
        }
        r.body(Body::from(if method == "GET" {
            ""
        } else {
            "{\"title\":\"Owner conversation\"}"
        }))
        .unwrap()
    };
    assert_eq!(
        f.app
            .clone()
            .oneshot(build(
                "GET",
                "/api/v1/conversations",
                None,
                "https://example.test:18445"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        f.app
            .clone()
            .oneshot(build(
                "POST",
                "/api/v1/conversations",
                None,
                "https://example.test:18445"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.app
            .clone()
            .oneshot(build(
                "POST",
                "/api/v1/conversations",
                Some(csrf),
                "https://evil.test"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.app
            .clone()
            .oneshot(build(
                "POST",
                "/api/v1/conversations",
                Some(csrf),
                "https://example.test:18445"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
}
#[tokio::test]
async fn provider_grants_expire_and_sources_have_provenance() {
    let f = fixture();
    let c = f.state.create("A source conversation").unwrap();
    let id = c["id"].as_str().unwrap();
    let grant = context::grant(&f.state, id).unwrap();
    let body = json!({"operation":"authorize","access":{"mode":"grant","grant":grant}});
    let (status, value) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["owner"], true);
    assert_eq!(value["conversation_id"], id);
    assert_eq!(value["actor_id"], "owner");
    let (_, list) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(json!({"operation":"list","access":{"mode":"owner"}})),
    )
    .await;
    assert_eq!(list["items"][0]["surface"], "web");
    assert!(list["items"][0]["url"].as_str().unwrap().contains(id));
    f.state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE grants SET expires_at=0", [])
        .unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            Some(&f.context),
            Some(body)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn view_state_supports_remote_control_and_conflict_detection() {
    let f = fixture();
    let c = f.state.create("Puppet view").unwrap();
    let body = json!({"expected_revision":0,"state":{"selected_conversation_id":c["id"],"panel":"connection","draft":"A suggested message","search":"Puppet"}});
    let (status, v) = request(
        &f.app,
        "PUT",
        "/api/v1/views/browser-1",
        Some(&f.admin),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["revision"], 1);
    assert_eq!(
        request(
            &f.app,
            "PUT",
            "/api/v1/views/browser-1",
            Some(&f.admin),
            Some(body)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, views) = request(&f.app, "GET", "/api/v1/views", Some(&f.admin), None).await;
    assert_eq!(views["items"][0]["state"]["draft"], "A suggested message");
    let (_, activity) = request(&f.app, "GET", "/api/v1/activity", Some(&f.admin), None).await;
    assert!(
        activity["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| !e["kind"].as_str().unwrap().contains("view"))
    );
}
#[tokio::test]
async fn actual_acp_session_resumes_preserves_mcp_and_exposes_permissions() {
    for (mode, method) in [
        ("resume", "session/resume"),
        ("both", "session/resume"),
        ("load", "session/load"),
    ] {
        assert_acp_restore(mode, method).await;
    }
}
async fn assert_acp_restore(mode: &str, method: &str) {
    let f = fixture();
    let mut connection = serde_json::to_value(&f.state.config.agent).unwrap();
    connection["env"]["FIXTURE_RESTORE"] = json!(mode);
    connection["session_cwd"] = json!("C:\\Remote Agent\\workspace");
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"default","config":connection}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let c = f.state.create("Protocol conversation").unwrap();
    let id = c["id"].as_str().unwrap();
    for text in [
        "request approval first message",
        "request approval second message",
    ] {
        let request_id = uuid::Uuid::new_v4().to_string();
        let body = json!({"text":text,"request_id":request_id});
        let path = format!("/api/v1/conversations/{id}/messages");
        assert_eq!(
            request(&f.app, "POST", &path, Some(&f.admin), Some(body.clone()))
                .await
                .0,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            request(&f.app, "POST", &path, Some(&f.admin), Some(body))
                .await
                .0,
            StatusCode::OK
        );
        wait_idle(&f, id).await;
    }
    let c = f.state.conversation(id).unwrap().unwrap();
    assert_eq!(c["status"], "idle");
    assert_eq!(c["messages"].as_array().unwrap().len(), 4);
    assert_eq!(
        c["messages"][3]["text"],
        "Fixture response: request approval second message"
    );
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    let rows: Vec<Value> = frames
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let new = rows.iter().find(|r| r["method"] == "session/new").unwrap();
    let load = rows.iter().find(|r| r["method"] == method).unwrap();
    assert_eq!(
        rows.iter().filter(|r| r["method"] == "session/new").count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r["method"] == "session/load" || r["method"] == "session/resume")
            .count(),
        1
    );
    let binding: String = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT session_id FROM room_agents WHERE conversation_id=?1 AND agent_id='default'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(load["params"]["sessionId"], binding);
    // The Python peer launched in its real local cwd while the remote path
    // crossed ACP unchanged on both creation and restore.
    assert_eq!(new["params"]["cwd"], "C:\\Remote Agent\\workspace");
    assert_eq!(load["params"]["cwd"], new["params"]["cwd"]);
    assert_eq!(
        new["params"]["mcpServers"],
        json!(f.state.config.agent.as_ref().unwrap().mcp_servers)
    );
    assert_eq!(load["params"]["mcpServers"], new["params"]["mcpServers"]);
    assert_eq!(
        new["params"]["_meta"]["io.github.bkrabach.conversation"]["provider_id"],
        f.state.config.provider_id()
    );
    assert_ne!(
        new["params"]["_meta"]["io.github.bkrabach.conversation"]["grant"],
        load["params"]["_meta"]["io.github.bkrabach.conversation"]["grant"]
    );
    assert!(
        rows.iter()
            .any(|r| r["id"] == "permission-1" && r["result"]["outcome"]["outcome"] == "cancelled")
    );
    let (_, activity) = request(&f.app, "GET", "/api/v1/activity", Some(&f.admin), None).await;
    let rendered = activity.to_string();
    assert!(!rendered.contains("fixture-only"));
    assert!(
        !rendered.contains(
            new["params"]["_meta"]["io.github.bkrabach.conversation"]["grant"]
                .as_str()
                .unwrap()
        )
    );
}
#[tokio::test]
async fn actual_acp_resume_refusal_keeps_binding_without_replaying_work() {
    for code in [-32601, -32602, -32000] {
        let f = fixture();
        let mut connection = serde_json::to_value(&f.state.config.agent).unwrap();
        connection["env"]["FIXTURE_RESTORE"] =
            json!(if code == -32601 { "both" } else { "resume" });
        connection["env"]["FIXTURE_RESUME_ERROR"] = json!(code.to_string());
        assert_eq!(
            request(
                &f.app,
                "POST",
                "/api/v1/agents",
                Some(&f.admin),
                Some(json!({"id":"default","config":connection}))
            )
            .await
            .0,
            StatusCode::OK
        );
        let c = f.state.create("Failed restore").unwrap();
        let id = c["id"].as_str().unwrap();
        let binding = || -> String {
            f.state.db.lock().unwrap().query_row(
            "SELECT session_id FROM room_agents WHERE conversation_id=?1 AND agent_id='default'",
            [id], |r| r.get(0)).unwrap()
        };
        let mut prior = None;
        for text in ["first retained input", "second input must not replay"] {
            assert_eq!(
                request(
                    &f.app,
                    "POST",
                    &format!("/api/v1/conversations/{id}/messages"),
                    Some(&f.admin),
                    Some(json!({"text":text,"request_id":uuid::Uuid::new_v4()}))
                )
                .await
                .0,
                StatusCode::ACCEPTED
            );
            wait_idle(&f, id).await;
            if let Some(saved) = &prior {
                assert_eq!(&binding(), saved);
            } else {
                prior = Some(binding());
            }
        }
        let conversation = f.state.conversation(id).unwrap().unwrap();
        assert_eq!(conversation["status"], "error");
        assert!(
            conversation["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["text"] == "second input must not replay")
        );
        let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
        let rows: Vec<Value> = frames
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        for method in ["session/new", "session/resume", "session/prompt"] {
            assert_eq!(
                rows.iter().filter(|r| r["method"] == method).count(),
                1,
                "{code}: {method}"
            );
        }
        assert!(!rows.iter().any(|r| r["method"] == "session/load"));
    }
}
#[tokio::test]
async fn actual_acp_without_restore_keeps_bounded_transcript_fallback() {
    let f = fixture();
    let mut connection = serde_json::to_value(&f.state.config.agent).unwrap();
    connection["env"]["FIXTURE_RESTORE"] = json!("neither");
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"default","config":connection}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let c = f.state.create("No restore capability").unwrap();
    let id = c["id"].as_str().unwrap();
    for text in ["first room marker", "second room marker"] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                &format!("/api/v1/conversations/{id}/messages"),
                Some(&f.admin),
                Some(json!({"text":text,"request_id":uuid::Uuid::new_v4()}))
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
        wait_idle(&f, id).await;
    }
    assert_eq!(f.state.conversation(id).unwrap().unwrap()["status"], "idle");
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    let rows: Vec<Value> = frames
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rows.iter().filter(|r| r["method"] == "session/new").count(),
        2
    );
    assert!(
        !rows
            .iter()
            .any(|r| r["method"] == "session/load" || r["method"] == "session/resume")
    );
    let prompts: Vec<_> = rows
        .iter()
        .filter(|r| r["method"] == "session/prompt")
        .collect();
    assert_ne!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    let second = prompts[1]["params"]["prompt"][0]["text"].as_str().unwrap();
    assert!(second.contains("first room marker") && second.contains("second room marker"));
}
async fn wait_idle(f: &Fixture, id: &str) {
    for _ in 0..240 {
        // Explicit fixture-user cancellation; production never makes this choice automatically.
        for permission in f.state.permissions.list(id, "owner") {
            let path = format!(
                "/api/v1/conversations/{id}/permissions/{}",
                permission["id"].as_str().unwrap()
            );
            assert_eq!(
                request(
                    &f.app,
                    "POST",
                    &path,
                    Some(&f.admin),
                    Some(json!({"option_id":null}))
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        if !f.state.running.lock().unwrap().contains_key(id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Fixture did not finish");
}
#[tokio::test]
async fn cancelling_a_turn_terminates_the_process_and_keeps_the_transcript() {
    let f = fixture();
    let c = f.state.create("Cancel conversation").unwrap();
    let id = c["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}");
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{path}/messages"),
            Some(&f.admin),
            Some(json!({"text":"wait forever","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{path}/messages"),
            Some(&f.admin),
            Some(json!({"text":"overlap","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{path}/cancel"),
            Some(&f.admin),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_idle(&f, id).await;
    let c = f.state.conversation(id).unwrap().unwrap();
    assert_eq!(c["status"], "error");
    assert_eq!(c["messages"][1]["status"], "interrupted");
    assert!(
        c["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["state"] == "cancelled")
    );
    assert!(c["note"].as_str().unwrap().contains("stopped"));
}
#[tokio::test]
async fn imports_are_append_only_idempotent_read_only_and_surface_aware() {
    let f = fixture();
    let path = "/api/v1/imports/toad-owner/native-session";
    let mut body = json!({"label":"Native client conversation","surface":"toad","messages":[{"id":"u1","role":"user","text":"Native question","created_at":10}]});
    let (status, v) = request(&f.app, "PUT", path, Some(&f.admin), Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["appended"], 1);
    let id = v["conversation_id"].as_str().unwrap();
    let (_, again) = request(&f.app, "PUT", path, Some(&f.admin), Some(body.clone())).await;
    assert_eq!(again["appended"], 0);
    assert_eq!(again["conversation_id"], id);
    body["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"a1","role":"assistant","text":"Native answer","created_at":11}));
    assert_eq!(
        request(&f.app, "PUT", path, Some(&f.admin), Some(body.clone()))
            .await
            .0,
        StatusCode::OK
    );
    body["messages"][0]["text"] = json!("Replacement forbidden");
    assert_eq!(
        request(&f.app, "PUT", path, Some(&f.admin), Some(body))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, read) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(json!({"operation":"read","access":{"mode":"owner"},"conversation_id":id})),
    )
    .await;
    assert_eq!(read["surface"], "toad");
    assert_eq!(read["messages"].as_array().unwrap().len(), 2);
    assert_eq!(read["read_only"], true);
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("/api/v1/conversations/{id}/messages"),
            Some(&f.admin),
            Some(json!({"text":"no","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &f.app,
            "PUT",
            path,
            Some(&f.context),
            Some(json!({"label":"x","surface":"toad","messages":[]}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn source_reads_are_bounded_and_honest_about_coverage() {
    let f = fixture();
    let body = json!({"label":"Long source","surface":"test-client","messages":[{"id":"1","role":"user","text":"one","created_at":1},{"id":"2","role":"assistant","text":"two","created_at":2}]});
    let (_, v) = request(
        &f.app,
        "PUT",
        "/api/v1/imports/fixture/one",
        Some(&f.admin),
        Some(body),
    )
    .await;
    let id = v["conversation_id"].as_str().unwrap();
    let (_, v) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(json!({"operation":"read","access":{"mode":"owner"},"conversation_id":id,"limit":1})),
    )
    .await;
    assert_eq!(v["complete"], false);
    assert_eq!(v["messages"][0]["text"], "two");
    assert_eq!(v["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn importer_cannot_read_prompt_configure_or_impersonate_a_context_provider() {
    let f = fixture();
    let token = std::fs::read_to_string(f._home.path().join("secrets/import-token")).unwrap();
    let body = json!({"label":"Imported","surface":"toad","messages":[{"id":"u1","role":"user","text":"one","created_at":1}]});
    let (status, c) = request(
        &f.app,
        "PUT",
        "/api/v1/imports/toad/session",
        Some(&token),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for (path, surface) in [
        ("/api/v1/imports/someone-else/session", "toad"),
        ("/api/v1/imports/toad/session", "slack"),
        ("/api/v1/imports/toad/session", "web"),
    ] {
        assert_eq!(
            request(
                &f.app,
                "PUT",
                path,
                Some(&token),
                Some(json!({"label":"Misattributed","surface":surface,"messages":[]}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    for path in [
        "/api/v1/conversations",
        "/api/v1/meta",
        "/api/v1/config",
        "/api/v1/views",
        "/api/v1/activity",
    ] {
        assert_eq!(
            request(&f.app, "GET", path, Some(&token), None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let id = c["conversation_id"].as_str().unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("/api/v1/conversations/{id}/messages"),
            Some(&token),
            Some(json!({"text":"no","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            Some(&token),
            Some(json!({"operation":"list","access":{"mode":"owner"}}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "PUT",
            "/api/v1/config",
            Some(&token),
            Some(json!(f.state.config))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn named_import_sources_are_isolated_rotatable_and_stay_revoked_after_restart() {
    let f = fixture();
    let toad = std::fs::read_to_string(f._home.path().join("secrets/import-token")).unwrap();
    let (_, zed) = request(
        &f.app,
        "POST",
        "/api/v1/import-sources",
        Some(&f.admin),
        Some(json!({"source":"zed","surface":"zed"})),
    )
    .await;
    let token = zed["token"].as_str().unwrap();
    assert_ne!(token, toad);
    let body = json!({"label":"Zed exchange","surface":"zed","messages":[{"id":"u1","role":"user","text":"Zed source fact","created_at":1}]});
    let (status, imported) = request(
        &f.app,
        "PUT",
        "/api/v1/imports/zed/shared-native-id",
        Some(token),
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        request(
            &f.app,
            "PUT",
            "/api/v1/imports/zed/shared-native-id",
            Some(token),
            Some(body.clone())
        )
        .await
        .1["appended"],
        0
    );
    for (path, surface) in [
        ("/api/v1/imports/toad/shared-native-id", "toad"),
        ("/api/v1/imports/zed/shared-native-id", "slack"),
    ] {
        let mut bad = body.clone();
        bad["surface"] = json!(surface);
        assert_eq!(
            request(&f.app, "PUT", path, Some(token), Some(bad)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    for path in [
        "/api/v1/conversations",
        "/api/v1/import-sources",
        "/api/v1/config",
    ] {
        assert_eq!(
            request(&f.app, "GET", path, Some(token), None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/import-sources/zed/rotate",
            Some(token),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "DELETE",
            "/api/v1/import-sources/zed",
            Some(token),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, listing) = request(
        &f.app,
        "GET",
        "/api/v1/import-sources",
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(listing["items"].as_array().unwrap().len(), 2);
    assert!(!listing.to_string().contains(token));
    assert!(!listing.to_string().contains(&toad));
    let (_, rotated) = request(
        &f.app,
        "POST",
        "/api/v1/import-sources/zed/rotate",
        Some(&f.admin),
        None,
    )
    .await;
    let current = rotated["token"].as_str().unwrap();
    assert_ne!(token, current);
    assert_eq!(
        request(
            &f.app,
            "PUT",
            "/api/v1/imports/zed/shared-native-id",
            Some(token),
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.app,
            "PUT",
            "/api/v1/imports/zed/shared-native-id",
            Some(current),
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &f.app,
            "DELETE",
            "/api/v1/import-sources/zed",
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let state = AppState::open(f._home.path().to_owned()).unwrap();
    let app = server::router(state.clone());
    assert_eq!(
        request(
            &app,
            "PUT",
            "/api/v1/imports/zed/shared-native-id",
            Some(current),
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let existing = state
        .conversation(imported["conversation_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(existing["messages"][0]["text"], "Zed source fact");
    assert_eq!(existing["surface"], "zed");
    let mut toad_body = body.clone();
    toad_body["surface"] = json!("toad");
    assert_eq!(
        request(
            &app,
            "PUT",
            "/api/v1/imports/toad/shared-native-id",
            Some(&toad),
            Some(toad_body)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        state.list().unwrap().len(),
        2,
        "Same native session ID on another source must stay independent"
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/import-sources",
            Some(&f.admin),
            Some(json!({"source":"zed","surface":"slack"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, restored) = request(
        &app,
        "POST",
        "/api/v1/import-sources",
        Some(&f.admin),
        Some(json!({"source":"zed","surface":"zed"})),
    )
    .await;
    assert_eq!(
        request(
            &app,
            "PUT",
            "/api/v1/imports/zed/shared-native-id",
            Some(restored["token"].as_str().unwrap()),
            Some(body)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "DELETE",
            "/api/v1/import-sources/toad",
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let app = server::router(AppState::open(f._home.path().to_owned()).unwrap());
    assert_eq!(
        request(
            &app,
            "PUT",
            "/api/v1/imports/toad/x",
            Some(&toad),
            Some(json!({"label":"x","surface":"toad","messages":[]}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn legacy_import_migration_preserves_token_and_conversation_identity() {
    let f = fixture();
    let token = std::fs::read_to_string(f._home.path().join("secrets/import-token")).unwrap();
    let body = json!({"label":"Legacy Toad","surface":"toad","messages":[{"id":"old","role":"user","text":"Existing history","created_at":1}]});
    let (_, old) = request(
        &f.app,
        "PUT",
        "/api/v1/imports/toad/legacy",
        Some(&token),
        Some(body.clone()),
    )
    .await;
    f.state
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE import_sources; DROP TABLE migrations;")
        .unwrap();
    let app = server::router(AppState::open(f._home.path().to_owned()).unwrap());
    let (status, new) = request(
        &app,
        "PUT",
        "/api/v1/imports/toad/legacy",
        Some(&token),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(new["conversation_id"], old["conversation_id"]);
    assert_eq!(new["appended"], 0);
    assert_eq!(
        std::fs::read_to_string(f._home.path().join("secrets/import-token")).unwrap(),
        token
    );
}

#[tokio::test]
async fn http2_authority_allows_same_origin_cookie_mutations_without_a_host_header() {
    let f = fixture();
    let token = "h2-cookie";
    let csrf = "h2-csrf";
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'owner',?2,?3)",
            rusqlite::params![auth::digest(token), csrf, now_ms() + 60_000],
        )
        .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("https://example.test:18445/api/v1/conversations")
        .version(axum::http::Version::HTTP_2)
        .header(header::COOKIE, "another_app=first")
        .header(
            header::COOKIE,
            format!("{}={token}", auth::cookie_name(&f.state)),
        )
        .header(header::ORIGIN, "https://example.test:18445")
        .header("x-csrf-token", csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{\"title\":\"HTTP2 browser\"}"))
        .unwrap();
    assert_eq!(
        f.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn oversized_latest_source_reply_keeps_a_valid_excerpt_and_prior_context() {
    let f = fixture();
    let long = "🦋".repeat(30_000);
    let (_,v)=request(&f.app,"PUT","/api/v1/imports/fixture/large",Some(&f.admin),Some(json!({"label":"A long response","surface":"fixture","messages":[{"id":"u","role":"user","text":"The question before the long reply","created_at":1},{"id":"a","role":"assistant","text":long,"created_at":2}]}))).await;
    let (_,read)=request(&f.app,"POST","/context/v1/query",Some(&f.context),Some(json!({"operation":"read","access":{"mode":"owner"},"conversation_id":v["conversation_id"]}))).await;
    assert_eq!(read["complete"], false);
    assert_eq!(read["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        read["messages"][0]["text"],
        "The question before the long reply"
    );
    assert_eq!(read["messages"][1]["truncated"], true);
    assert_eq!(read["messages"][1]["original_bytes"], 120_000);
    assert_eq!(read["messages"][1]["text"].as_str().unwrap().len(), 20_000);
}

async fn user_request(
    f: &Fixture,
    user: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let token = format!("fixture-browser-{user}");
    let csrf = "room-test-csrf";
    f.state.db.lock().unwrap().execute("INSERT OR REPLACE INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,?2,?3,?4)",rusqlite::params![auth::digest(&token),user,csrf,now_ms()+60000]).unwrap();
    let r = Request::builder()
        .method(method)
        .uri(path)
        .header(
            header::COOKIE,
            format!("{}={token}", auth::cookie_name(&f.state)),
        )
        .header(header::HOST, "example.test:18445")
        .header(header::ORIGIN, "https://example.test:18445")
        .header("x-csrf-token", csrf)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let r = f.app.clone().oneshot(r).await.unwrap();
    let status = r.status();
    let bytes = to_bytes(r.into_body(), 4 * 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn shared_room_approvals_belong_to_the_triggering_person_and_cannot_be_replayed() {
    let f = fixture();
    let room = f.state.create("Approval room").unwrap();
    let id = room["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}");
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &format!("{path}/members"),
            Some(json!({"username":"robin","share_history":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &format!("{path}/messages"),
            Some(json!({"text":"request approval","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let mut pending = Value::Null;
    for _ in 0..100 {
        pending = user_request(&f, "robin", "GET", &path, None).await.1;
        if pending["permissions"]
            .as_array()
            .is_some_and(|p| !p.is_empty())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(pending["status"], "working");
    let permission = pending["permissions"][0]["id"].as_str().unwrap();
    let route = format!("{path}/permissions/{permission}");
    assert_eq!(
        user_request(&f, "owner", "GET", &path, None).await.1["permissions"],
        json!([])
    );
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &route,
            Some(json!({"option_id":"yes"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        user_request(
            &f,
            "casey",
            "POST",
            &route,
            Some(json!({"option_id":"yes"}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &route,
            Some(json!({"option_id":"invented"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &route,
            Some(json!({"option_id":"yes"}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &route,
            Some(json!({"option_id":"yes"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    wait_idle(&f, id).await;
    assert_eq!(f.state.conversation(id).unwrap().unwrap()["status"], "idle");
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    assert!(
        frames
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|r| r["id"] == "permission-1" && r["result"]["outcome"]["optionId"] == "yes")
    );
    let activity = user_request(&f, "robin", "GET", "/api/v1/activity", None)
        .await
        .1;
    assert!(!activity.to_string().contains("fixture-only"));
    // Cancelling a waiting turn consumes no approval and invalidates the request ID.
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &format!("{path}/messages"),
            Some(json!({"text":"request approval again","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    for _ in 0..100 {
        pending = user_request(&f, "robin", "GET", &path, None).await.1;
        if pending["permissions"]
            .as_array()
            .is_some_and(|p| !p.is_empty())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let permission = pending["permissions"][0]["id"].as_str().unwrap();
    let route = format!("{path}/permissions/{permission}");
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &format!("{path}/cancel"),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    for _ in 0..240 {
        if !f.state.running.lock().unwrap().contains_key(id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!f.state.running.lock().unwrap().contains_key(id));
    assert!(f.state.permissions.list(id, "robin").is_empty());
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &route,
            Some(json!({"option_id":"yes"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}
#[tokio::test]
async fn rooms_do_not_expose_private_history_views_activity_or_context() {
    let f = fixture();
    let c = f.state.create("Owner private").unwrap();
    let id = c["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}");
    f.state.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('private-note',?1,'user','private-owner-secret','complete',1,1,'owner')",[id]).unwrap();
    let (_, robin) = user_request(
        &f,
        "robin",
        "POST",
        "/api/v1/conversations",
        Some(json!({"title":"Robin private"})),
    )
    .await;
    let sam_id = robin["id"].as_str().unwrap();
    assert_eq!(
        user_request(&f, "robin", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "GET",
            &format!("{path}/messages?search=private"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = user_request(
        &f,
        "robin",
        "GET",
        "/api/v1/conversations?search=Owner",
        None,
    )
    .await;
    assert!(list["items"].as_array().unwrap().is_empty());
    assert_eq!(
        request(
            &f.app,
            "GET",
            &format!("/api/v1/conversations/{sam_id}"),
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(&f, "robin", "GET", "/api/v1/config", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        user_request(&f, "unknown", "GET", "/api/v1/conversations", None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{path}/members"),
            Some(&f.admin),
            Some(json!({"username":"robin"}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let private_grant = context::grant(&f.state, id).unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{path}/members"),
            Some(&f.admin),
            Some(json!({"username":"robin","share_history":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, joined) = user_request(&f, "robin", "GET", &path, None).await;
    assert_eq!(joined["messages"][0]["text"], "private-owner-secret");
    assert_eq!(joined["shared"], true);
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            Some(&f.context),
            Some(json!({"operation":"authorize","access":{"mode":"grant","grant":private_grant}}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let grant = context::grant(&f.state, id).unwrap();
    let access = json!({"mode":"grant","grant":grant});
    let (_, auth) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(json!({"operation":"authorize","access":access})),
    )
    .await;
    assert_eq!(auth["owner"], false);
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            Some(&f.context),
            Some(json!({"operation":"read","access":access,"conversation_id":sam_id}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, list) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(json!({"operation":"list","access":access})),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["id"], id);
    assert_eq!(
        user_request(&f, "robin", "DELETE", &path, None).await.0,
        StatusCode::FORBIDDEN
    );
    let view = json!({"expected_revision":0,"state":{"selected_conversation_id":id,"panel":"conversation","draft":"Robin private draft","search":""}});
    assert_eq!(
        user_request(
            &f,
            "robin",
            "PUT",
            "/api/v1/views/robin-view",
            Some(view.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, views) = request(&f.app, "GET", "/api/v1/views", Some(&f.admin), None).await;
    assert!(views["items"].as_array().unwrap().is_empty());
    assert_eq!(
        request(
            &f.app,
            "PUT",
            "/api/v1/views/robin-view",
            Some(&f.admin),
            Some(view)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    request(
        &f.app,
        "DELETE",
        &format!("{path}/members/robin"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(
        user_request(&f, "robin", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, activity) = user_request(&f, "robin", "GET", "/api/v1/activity", None).await;
    assert!(
        activity["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["conversation_id"] != id)
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/context/v1/query",
            Some(&f.context),
            Some(json!({"operation":"authorize","access":access}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let restarted = AppState::open(f._home.path().into()).unwrap();
    assert!(!restarted.member(id, "robin").unwrap());
    assert!(restarted.member(sam_id, "robin").unwrap());
    assert_eq!(
        restarted.conversation_info(id).unwrap().unwrap()["shared"],
        true
    );
}
#[tokio::test]
async fn independent_agents_have_independent_room_sessions_and_selective_group_participation() {
    let f = fixture();
    let mut agent = f.state.config.agent.clone().unwrap();
    agent.label = "Listening reviewer".into();
    agent.env.insert("FIXTURE_AMBIENT_PASS".into(), "1".into());
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"reviewer","config":agent}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            "/api/v1/agents",
            Some(json!({"id":"no","config":agent}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, c) = request(
        &f.app,
        "POST",
        "/api/v1/conversations",
        Some(&f.admin),
        Some(json!({"title":"Shared work","agent_ids":["default","reviewer"]})),
    )
    .await;
    let id = c["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}/messages");
    assert_eq!(
        request(
            &f.app,
            "POST",
            &path,
            Some(&f.admin),
            Some(json!({"text":"Any thoughts?","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_idle(&f, id).await;
    let c = f.state.conversation(id).unwrap().unwrap();
    assert_eq!(c["status"], "idle");
    assert_eq!(c["messages"].as_array().unwrap().len(), 2);
    assert_eq!(c["messages"][1]["agent_id"], "default");
    assert!(
        c["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["text"] != "[[PASS]]")
    );
    let ids = {
        let db = f.state.db.lock().unwrap();
        let mut q = db
            .prepare(
                "SELECT session_id FROM room_agents WHERE conversation_id=?1 ORDER BY agent_id",
            )
            .unwrap();
        q.query_map([id], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let (_, activity) = request(&f.app, "GET", "/api/v1/activity", Some(&f.admin), None).await;
    assert!(
        activity["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["kind"] == "turn.passed" && a["data"]["agent_id"] == "reviewer")
    );
    assert_eq!(request(&f.app,"POST",&path,Some(&f.admin),Some(json!({"text":"Reviewer, check this","request_id":uuid::Uuid::new_v4(),"target_agent_ids":["reviewer"]}))).await.0,StatusCode::ACCEPTED);
    wait_idle(&f, id).await;
    let c = f.state.conversation(id).unwrap().unwrap();
    assert_eq!(c["messages"].as_array().unwrap().len(), 4);
    assert_eq!(c["messages"][3]["agent_id"], "reviewer");
    assert!(
        c["messages"][3]["text"]
            .as_str()
            .unwrap()
            .contains("directly addressed")
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            &path,
            Some(&f.admin),
            Some(
                json!({"text":"No","request_id":uuid::Uuid::new_v4(),"target_agent_ids":["absent"]})
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let restarted = AppState::open(f._home.path().into()).unwrap();
    assert!(restarted.agent("reviewer").unwrap().is_some());
    assert_eq!(restarted.room_agents(id).unwrap().len(), 2);
    let private = f.state.create("Different room").unwrap();
    let private_id = private["id"].as_str().unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("/api/v1/conversations/{private_id}/agents"),
            Some(&f.admin),
            Some(json!({"agent_id":"reviewer"}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("/api/v1/conversations/{private_id}/agents"),
            Some(&f.admin),
            Some(json!({"agent_id":"reviewer","share_history":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(f.state.db.lock().unwrap().query_row("SELECT session_id IS NULL FROM room_agents WHERE conversation_id=?1 AND agent_id='reviewer'",[private_id],|r|r.get::<_,bool>(0)).unwrap());
}
#[tokio::test]
async fn detaching_one_agent_preserves_remaining_session_and_revokes_old_grants() {
    let f = fixture();
    let mut agent = f.state.config.agent.clone().unwrap();
    agent.env.insert("FIXTURE_RESTORE".into(), "resume".into());
    for id in ["default", "reviewer"] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                "/api/v1/agents",
                Some(&f.admin),
                Some(json!({"id":id,"config":agent}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let (_, room) = request(
        &f.app,
        "POST",
        "/api/v1/conversations",
        Some(&f.admin),
        Some(json!({"title":"Independent participants","agent_ids":["default","reviewer"]})),
    )
    .await;
    let id = room["id"].as_str().unwrap();
    let messages_path = format!("/api/v1/conversations/{id}/messages");
    for target in ["default", "reviewer"] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                &messages_path,
                Some(&f.admin),
                Some(json!({"text":format!("Initial work for {target}"),
                    "request_id":uuid::Uuid::new_v4(),"target_agent_ids":[target]}))
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
        wait_idle(&f, id).await;
    }
    let binding = || -> (Option<String>, Option<String>, i64) {
        f.state.db.lock().unwrap().query_row(
            "SELECT session_id,endpoint,last_seq FROM room_agents WHERE conversation_id=?1 AND agent_id='reviewer'",
            [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
    };
    let before_binding = binding();
    assert!(before_binding.0.is_some() && before_binding.1.is_some() && before_binding.2 > 0);
    let before_messages = f.state.messages(id, 100).unwrap();
    // Retained legacy room-wide columns must still be cleared by audience changes.
    f.state.db.lock().unwrap().execute(
        "UPDATE conversations SET session_id='legacy-session',endpoint='legacy-endpoint' WHERE id=?1",
        [id]).unwrap();
    let grant = context::grant(&f.state, id).unwrap();
    let (_, credential) = request(
        &f.app,
        "POST",
        "/api/v1/agents/reviewer/context-credential",
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    let token = credential["token"].as_str().unwrap();
    let query_path = "/agents/reviewer/context/v1/query";
    let access = json!({"mode":"grant","grant":grant});
    assert_eq!(
        request(
            &f.app,
            "POST",
            query_path,
            Some(token),
            Some(json!({"operation":"read","access":access,"conversation_id":id}))
        )
        .await
        .0,
        StatusCode::OK
    );

    let (cancel, _receiver) = tokio::sync::watch::channel(false);
    f.state.running.lock().unwrap().insert(id.into(), cancel);
    assert_eq!(
        request(
            &f.app,
            "DELETE",
            &format!("/api/v1/conversations/{id}/agents/default"),
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    f.state.running.lock().unwrap().remove(id);
    assert_eq!(binding(), before_binding);
    assert_eq!(f.state.room_agents(id).unwrap().len(), 2);
    assert_eq!(
        request(
            &f.app,
            "POST",
            query_path,
            Some(token),
            Some(json!({"operation":"read","access":access,"conversation_id":id}))
        )
        .await
        .0,
        StatusCode::OK
    );

    let (status, removed) = request(
        &f.app,
        "DELETE",
        &format!("/api/v1/conversations/{id}/agents/default"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed["removed"], true);
    // Audience revocation must not orphan another participant's existing work.
    assert_eq!(
        binding(),
        before_binding,
        "Unchanged participant lost its native binding"
    );
    assert_eq!(f.state.messages(id, 100).unwrap(), before_messages);
    assert_eq!(
        f.state
            .room_agents(id)
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["reviewer"]
    );
    assert_eq!(
        f.state.conversation_info(id).unwrap().unwrap()["shared"],
        true
    );
    assert!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT session_id IS NULL AND endpoint IS NULL FROM conversations WHERE id=?1",
                [id],
                |row| row.get::<_, bool>(0)
            )
            .unwrap()
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            query_path,
            Some(token),
            Some(json!({"operation":"authorize","access":access,"conversation_id":id}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let fresh_access = json!({"mode":"grant","grant":context::grant(&f.state,id).unwrap()});
    let (status, authorized) = request(
        &f.app,
        "POST",
        query_path,
        Some(token),
        Some(json!({"operation":"authorize","access":fresh_access,"conversation_id":id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(authorized["owner"], false);
    for access in [json!({"mode":"owner"}), access, fresh_access] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                "/context/v1/query",
                Some(&f.context),
                Some(json!({"operation":"read","access":access,"conversation_id":id}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }

    assert_eq!(
        request(
            &f.app,
            "POST",
            &messages_path,
            Some(&f.admin),
            Some(
                json!({"text":"Continue the review","request_id":uuid::Uuid::new_v4(),
            "target_agent_ids":["reviewer"]})
            )
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_idle(&f, id).await;
    let after_binding = binding();
    assert_eq!(after_binding.0, before_binding.0);
    assert_eq!(after_binding.1, before_binding.1);
    assert!(after_binding.2 > before_binding.2);
    let after_messages = f.state.messages(id, 100).unwrap();
    assert_eq!(
        &after_messages[..before_messages.len()],
        before_messages.as_slice()
    );
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    let frames: Vec<Value> = frames
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        frames
            .iter()
            .filter(|r| r["method"] == "session/new")
            .count(),
        2
    );
    assert_eq!(
        frames
            .iter()
            .filter(|r| r["method"] == "session/resume")
            .count(),
        1
    );
    assert!(
        !frames
            .iter()
            .any(|r| r["method"] == "session/load" || r["method"] == "session/cancel")
    );
    let resumed = frames
        .iter()
        .find(|r| r["method"] == "session/resume")
        .unwrap();
    assert_eq!(resumed["params"]["sessionId"], before_binding.0.unwrap());
    assert_eq!(
        frames
            .iter()
            .filter(|r| r["method"] == "session/prompt")
            .count(),
        3
    );
}

// These cases control public admission order with the existing turn gate. They
// do not inject a scheduler checkpoint inside the synchronous delete handler.
async fn acp_membership_idle(f: &Fixture, room: &str) {
    let mut changes = f.state.changes.subscribe();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let working = f.state.running.lock().unwrap().contains_key(room);
            if !working {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("The fixture's admitted turn did not settle");
}

fn acp_membership_frames(f: &Fixture) -> Vec<Value> {
    std::fs::read_to_string(f._home.path().join("frames.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn acp_membership_post(
    f: &Fixture,
    room: &str,
    agent: &str,
    id: &str,
    text: &str,
) -> StatusCode {
    request(
        &f.app,
        "POST",
        &format!("/api/v1/conversations/{room}/messages"),
        Some(&f.admin),
        Some(json!({"text":text,"request_id":id,"target_agent_ids":[agent]})),
    )
    .await
    .0
}

async fn acp_membership_admission_case(delete_room: bool, admission_first: bool) {
    let f = fixture();
    let mut agent = f.state.config.agent.clone().unwrap();
    agent.env.insert("FIXTURE_RESTORE".into(), "resume".into());
    for id in ["default", "reviewer"] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                "/api/v1/agents",
                Some(&f.admin),
                Some(json!({"id":id,"config":agent}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let agents = if delete_room {
        vec!["default"]
    } else {
        vec!["default", "reviewer"]
    };
    let room = f
        .state
        .create_room(
            "Admission order",
            "owner",
            &agents.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        )
        .unwrap();
    let room = room["id"].as_str().unwrap().to_owned();
    // Deleting a whole room intentionally removes its bindings. B's unaffected
    // control must be in another room; detaching A preserves B in this room.
    let b_room = if delete_room {
        f.state
            .create_room("Unaffected work", "owner", &["reviewer".into()])
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        room.clone()
    };
    for (id, target) in [(&room, "default"), (&b_room, "reviewer")] {
        assert_eq!(
            acp_membership_post(
                &f,
                id,
                target,
                &uuid::Uuid::new_v4().to_string(),
                "Seed work"
            )
            .await,
            StatusCode::ACCEPTED
        );
        acp_membership_idle(&f, id).await;
    }
    let binding = |room: &str, agent: &str| -> (String, String, i64) {
        f.state.db.lock().unwrap().query_row(
            "SELECT session_id,endpoint,last_seq FROM room_agents WHERE conversation_id=?1 AND agent_id=?2",
            rusqlite::params![room, agent], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
    };
    let a_before = binding(&room, "default");
    let b_before = binding(&b_room, "reviewer");
    assert_ne!(a_before.0, b_before.0);
    let b_history = f.state.messages(&b_room, 100).unwrap();
    let frames_before = acp_membership_frames(&f);
    let grant = context::grant(&f.state, &room).unwrap();
    let grants = || -> Vec<String> {
        let db = f.state.db.lock().unwrap();
        db.prepare("SELECT hash FROM grants WHERE conversation_id=?1 ORDER BY hash")
            .unwrap()
            .query_map([&room], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(!grants().is_empty());
    let delete_path = if delete_room {
        format!("/api/v1/conversations/{room}")
    } else {
        format!("/api/v1/conversations/{room}/agents/default")
    };
    if admission_first {
        // Intake is durable and owns the room, but no ACP peer can dispatch yet.
        let permits = f
            .state
            .turn_gate
            .clone()
            .acquire_many_owned(4)
            .await
            .unwrap();
        let input = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            acp_membership_post(&f, &room, "default", &input, "Admitted before removal").await,
            StatusCode::ACCEPTED
        );
        assert!(f.state.running.lock().unwrap().contains_key(&room));
        let delivery: (i64, String) = f
            .state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*),min(state) FROM input_deliveries WHERE message_id=?1",
                [&input],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(delivery, (1, "received".into()));
        let messages = f.state.messages(&room, 100).unwrap();
        let old_grants = grants();
        assert_eq!(
            request(&f.app, "DELETE", &delete_path, Some(&f.admin), None)
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(f.state.messages(&room, 100).unwrap(), messages);
        assert_eq!(grants(), old_grants);
        assert_eq!(binding(&room, "default"), a_before);
        assert_eq!(binding(&b_room, "reviewer"), b_before);
        assert_eq!(acp_membership_frames(&f), frames_before);
        drop(permits);
        acp_membership_idle(&f, &room).await;
        let delivered = agent_conversations::delivery::list(&f.state, &room).unwrap();
        let rows: Vec<_> = delivered
            .iter()
            .filter(|row| row["message_id"] == input)
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "completed");
        let frames = acp_membership_frames(&f);
        let prompts: Vec<_> = frames
            .iter()
            .filter(|row| {
                row["method"] == "session/prompt"
                    && row["params"]["_meta"]["com.microsoft.amplifier.conversations"]["messageId"]
                        == input
            })
            .collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["params"]["sessionId"], a_before.0);
    }
    let before_removal = f.state.messages(&room, 100).unwrap();
    assert_eq!(
        request(&f.app, "DELETE", &delete_path, Some(&f.admin), None)
            .await
            .0,
        StatusCode::OK
    );
    assert!(grants().is_empty());
    if delete_room {
        assert!(!f.state.exists(&room).unwrap());
    } else {
        assert_eq!(f.state.messages(&room, 100).unwrap(), before_removal);
        assert_eq!(f.state.room_agents(&room).unwrap().len(), 1);
    }
    assert_eq!(binding(&b_room, "reviewer"), b_before);
    // A's admitted source/reply may extend the shared room; it cannot rewrite B's work.
    assert_eq!(
        &f.state.messages(&b_room, 100).unwrap()[..b_history.len()],
        b_history.as_slice()
    );
    assert_eq!(request(&f.app, "POST", "/context/v1/query", Some(&f.context),
        Some(json!({"operation":"read","access":{"mode":"grant","grant":grant},"conversation_id":room}))).await.0,
        StatusCode::FORBIDDEN);
    let frames = acp_membership_frames(&f);
    let refused = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        acp_membership_post(&f, &room, "default", &refused, "Must not dispatch").await,
        if delete_room {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        }
    );
    let absent: (i64, i64) = f.state.db.lock().unwrap().query_row(
        "SELECT (SELECT count(*) FROM messages WHERE id=?1),(SELECT count(*) FROM input_deliveries WHERE message_id=?1)",
        [&refused], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(absent, (0, 0));
    assert_eq!(acp_membership_frames(&f), frames);
    let b_input = uuid::Uuid::new_v4().to_string();
    let b_text = "Continue the remaining agent's work";
    assert_eq!(
        acp_membership_post(&f, &b_room, "reviewer", &b_input, b_text).await,
        StatusCode::ACCEPTED
    );
    acp_membership_idle(&f, &b_room).await;
    let b_after = binding(&b_room, "reviewer");
    assert_eq!((&b_after.0, &b_after.1), (&b_before.0, &b_before.1));
    assert!(b_after.2 > b_before.2);
    let frames = acp_membership_frames(&f);
    assert_eq!(
        frames
            .iter()
            .filter(|row| row["method"] == "session/new")
            .count(),
        2
    );
    assert_eq!(
        frames
            .iter()
            .filter(|row| row["method"] == "session/resume")
            .count(),
        1 + usize::from(admission_first)
    );
    assert_eq!(
        frames
            .iter()
            .filter(|row| row["method"] == "session/prompt")
            .count(),
        3 + usize::from(admission_first)
    );
    assert!(
        !frames
            .iter()
            .any(|row| row["method"] == "session/load" || row["method"] == "session/cancel")
    );
    let last = frames
        .iter()
        .rev()
        .find(|row| row["method"] == "session/prompt")
        .unwrap();
    assert_eq!(last["params"]["sessionId"], b_before.0);
    assert_eq!(
        last["params"]["_meta"]["com.microsoft.amplifier.conversations"]["messageId"],
        b_input
    );
    assert_eq!(last["params"]["prompt"][0]["text"], b_text);
}

#[tokio::test]
async fn acp_membership_admission_before_detach() {
    acp_membership_admission_case(false, true).await;
}

#[tokio::test]
async fn acp_membership_admission_after_detach() {
    acp_membership_admission_case(false, false).await;
}

#[tokio::test]
async fn acp_membership_admission_before_room_deletion() {
    acp_membership_admission_case(true, true).await;
}

#[tokio::test]
async fn acp_membership_admission_after_room_deletion() {
    acp_membership_admission_case(true, false).await;
}

#[tokio::test]
async fn attachments_are_private_bounded_and_honest_about_acp_capabilities() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let f = fixture();
    let c = f.state.create("Files").unwrap();
    let id = c["id"].as_str().unwrap();
    let base = format!("/api/v1/conversations/{id}");
    let (_,text)=request(&f.app,"POST",&format!("{base}/attachments"),Some(&f.admin),Some(json!({"name":"notes.md","mime_type":"text/markdown","data":STANDARD.encode("# Actual notes\nKnown content")}))).await;
    let image_bytes = b"\x89PNG\r\n\x1a\nfixture";
    let (_, image) = request(
        &f.app,
        "POST",
        &format!("{base}/attachments"),
        Some(&f.admin),
        Some(
            json!({"name":"image.png","mime_type":"image/png","data":STANDARD.encode(image_bytes)}),
        ),
    )
    .await;
    assert!(text["id"].is_string());
    assert!(image["id"].is_string());
    assert_eq!(
        user_request(
            &f,
            "robin",
            "GET",
            &format!("{base}/attachments/{}", text["id"].as_str().unwrap()),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for (name, mime, source) in [
        (
            "drawing.svg",
            "image/svg+xml",
            "<svg xmlns='http://www.w3.org/2000/svg'/>",
        ),
        ("template.html", "text/html", "<h1>Original template</h1>"),
        ("diagram.mmd", "text/vnd.mermaid", "flowchart LR\nA --> B"),
    ] {
        let (status, stored) = request(
            &f.app,
            "POST",
            &format!("{base}/attachments"),
            Some(&f.admin),
            Some(json!({"name":name,"mime_type":mime,"data":STANDARD.encode(source)})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let url = stored["url"].as_str().unwrap();
        assert_eq!(
            user_request(&f, "robin", "GET", url, None).await.0,
            StatusCode::NOT_FOUND
        );
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(url)
                    .header(header::AUTHORIZATION, format!("Bearer {}", f.admin))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        if mime != "text/vnd.mermaid" {
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/plain; charset=utf-8"
            );
        }
        assert_eq!(to_bytes(response.into_body(), 2048).await.unwrap(), source);
    }
    for (name, mime, data) in [
        ("../escape.txt", "text/plain", b"x".as_slice()),
        ("binary.svg", "image/svg+xml", b"\0".as_slice()),
        ("fake.png", "image/png", b"<html>".as_slice()),
    ] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                &format!("{base}/attachments"),
                Some(&f.admin),
                Some(json!({"name":name,"mime_type":mime,"data":STANDARD.encode(data)}))
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let body = json!({"text":"Read these","request_id":uuid::Uuid::new_v4(),"attachment_ids":[text["id"],image["id"]]});
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{base}/messages"),
            Some(&f.admin),
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    wait_idle(&f, id).await;
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{base}/messages"),
            Some(&f.admin),
            Some(body)
        )
        .await
        .0,
        StatusCode::OK
    );
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    let prompt = frames
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["method"] == "session/prompt")
        .unwrap();
    assert!(
        prompt["params"]["prompt"]
            .to_string()
            .contains("Known content")
    );
    assert!(
        prompt["params"]["prompt"]
            .to_string()
            .contains("did not advertise image input")
    );
    assert!(
        prompt["params"]["prompt"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["type"] == "text")
    );
    let ids = vec![
        text["id"].as_str().unwrap().to_string(),
        image["id"].as_str().unwrap().to_string(),
    ];
    let blocks = agent_conversations::attachments::prompt_content(
        &f.state,
        id,
        &ids,
        &json!({"promptCapabilities":{"image":true,"embeddedContext":true}}),
    )
    .unwrap();
    assert_eq!(blocks[0]["type"], "resource");
    assert_eq!(blocks[2]["type"], "image");
    assert_eq!(blocks[2]["data"], STANDARD.encode(image_bytes));
    let other = f.state.create("Other room").unwrap();
    assert_eq!(request(&f.app,"POST",&format!("/api/v1/conversations/{}/messages",other["id"].as_str().unwrap()),Some(&f.admin),Some(json!({"text":"wrong file","request_id":uuid::Uuid::new_v4(),"attachment_ids":[text["id"]]}))).await.0,StatusCode::BAD_REQUEST);
    let (_, history) = request(
        &f.app,
        "GET",
        &format!("{base}/messages"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(
        history["items"][0]["attachments"].as_array().unwrap().len(),
        2
    );
}
#[tokio::test]
async fn pdf_upload_preserves_bytes_and_obeys_room_and_capability_boundaries() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let f = fixture();
    let c = f.state.create("PDF fixture").unwrap();
    let id = c["id"].as_str().unwrap();
    let base = format!("/api/v1/conversations/{id}");
    let encoded = STANDARD.encode(b"%PDF-1.7\nfixture\n%%EOF");
    let (status, pdf) = request(
        &f.app,
        "POST",
        &format!("{base}/attachments"),
        Some(&f.admin),
        Some(json!({"name":"brief.pdf","mime_type":"application/pdf","data":encoded})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        user_request(
            &f,
            "robin",
            "GET",
            &format!("{base}/attachments/{}", pdf["id"].as_str().unwrap()),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(request(&f.app,"POST",&format!("{base}/attachments"),Some(&f.admin),Some(json!({"name":"fake.pdf","mime_type":"application/pdf","data":STANDARD.encode(b"not pdf")}))).await.0,StatusCode::BAD_REQUEST);
    assert_eq!(request(&f.app,"POST",&format!("{base}/messages"),Some(&f.admin),Some(json!({"text":"Read the file","request_id":uuid::Uuid::new_v4(),"attachment_ids":[pdf["id"]]}))).await.0,StatusCode::ACCEPTED);
    wait_idle(&f, id).await;
    let ids = vec![pdf["id"].as_str().unwrap().to_string()];
    let content = agent_conversations::attachments::prompt_content(
        &f.state,
        id,
        &ids,
        &json!({"promptCapabilities":{"embeddedContext":true}}),
    )
    .unwrap();
    assert_eq!(content[0]["resource"]["blob"], encoded);
    let fallback =
        agent_conversations::attachments::prompt_content(&f.state, id, &ids, &json!({})).unwrap();
    assert!(
        fallback[0]["text"]
            .as_str()
            .unwrap()
            .contains("not supplied")
    );
    assert!(!serde_json::to_string(&fallback).unwrap().contains(&encoded));
}

#[tokio::test]
async fn message_search_paginates_without_overlap_and_never_reads_another_room() {
    let f = fixture();
    let c = f.state.create("History").unwrap();
    let id = c["id"].as_str().unwrap();
    for n in 0..137 {
        f.state.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')",rusqlite::params![format!("msg-{n}"),id,format!("topic {}",n%2),n]).unwrap();
    }
    let base = format!("/api/v1/conversations/{id}/messages");
    let (_, first) = request(
        &f.app,
        "GET",
        &format!("{base}?limit=20&search=topic%201"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(first["items"].as_array().unwrap().len(), 20);
    let cursor = first["next_cursor"].as_i64().unwrap();
    let (_, next) = request(
        &f.app,
        "GET",
        &format!("{base}?limit=20&search=topic%201&before={cursor}"),
        Some(&f.admin),
        None,
    )
    .await;
    assert!(
        next["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["seq"].as_i64().unwrap() < cursor)
    );
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["text"] == "topic 1")
    );
    assert_eq!(
        user_request(&f, "robin", "GET", &base, None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, read) = request(
        &f.app,
        "GET",
        &format!("/api/v1/conversations/{id}"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(read["messages"].as_array().unwrap().len(), 100);
    assert_eq!(read["has_older"], true);
    let (_,view)=request(&f.app,"PUT","/api/v1/views/history-control",Some(&f.admin),Some(json!({"expected_revision":0,"state":{"selected_conversation_id":id,"panel":"conversation","draft":"","search":"","participants_open":true,"history_search":"topic 1","history_before":cursor,"target_agent_ids":["default"],"attachment_ids":[],"action":{"id":"focus-history","kind":"focus","target":"history_search"}}}))).await;
    assert_eq!(view["state"]["history_before"], cursor);
    assert_eq!(view["state"]["participants_open"], true);
}

#[tokio::test]
async fn spoken_history_module_is_embedded_and_never_cached() {
    let f = fixture();
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/message-presentation.mjs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(
        bytes.as_ref(),
        include_bytes!("../ui/message-presentation.mjs")
    );
}

#[tokio::test]
async fn spoken_history_presentation_requires_exact_call_room_and_preserves_canonical_source() {
    let f = fixture();
    let room = f.state.create("Voice history").unwrap();
    let other = f.state.create("Other history").unwrap();
    let id = room["id"].as_str().unwrap();
    let other_id = other["id"].as_str().unwrap();
    let internal = "The live voice conversation delegated help at 64000ms. Internal guidance. Configured Companion read catalog: {\"private_route\":true}";
    {
        let db = f.state.db.lock().unwrap();
        for (call, call_room, owner) in [
            ("voice-call", id, "owner"),
            ("other-call", other_id, "owner"),
            ("wrong-owner", id, "robin"),
        ] {
            db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at) VALUES(?1,?2,?1,'hash','default',?3,'closed',1,1)",rusqlite::params![call,owner,call_room]).unwrap();
        }
        for (n, message) in ["linked", "typed-prefix", "cross-room", "cross-owner"]
            .into_iter()
            .enumerate()
        {
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')",rusqlite::params![message,id,internal,n as i64]).unwrap();
        }
        for (call, message) in [
            ("voice-call", "linked"),
            ("other-call", "cross-room"),
            ("wrong-owner", "cross-owner"),
        ] {
            db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,source,request_text) VALUES(?1,?2,64000,'completed',?2,1,'{\"original\":true}','Where are you now?')",rusqlite::params![call,message]).unwrap();
        }
    }
    let canonical = f.state.messages(id, 100).unwrap();
    let source = f.state.source_message_page(id, 100, None, "").unwrap();
    let (status, history) = request(
        &f.app,
        "GET",
        &format!("/api/v1/conversations/{id}/messages?limit=2"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        history["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m.get("presentation").is_none())
    );
    let (_, earlier) = request(
        &f.app,
        "GET",
        &format!(
            "/api/v1/conversations/{id}/messages?before={}",
            history["next_cursor"]
        ),
        Some(&f.admin),
        None,
    )
    .await;
    let items = earlier["items"].as_array().unwrap();
    assert_eq!(items[0]["id"], "linked");
    assert_eq!(items[0]["text"], internal);
    assert_eq!(items[0]["presentation"]["text"], "Where are you now?");
    assert_eq!(items[0]["presentation"]["label"], "From voice");
    assert_eq!(items[0]["presentation"]["excerpt"], true);
    assert!(
        items[1].get("presentation").is_none(),
        "Typed matching prefix is ordinary text"
    );
    let (_, initial) = request(
        &f.app,
        "GET",
        &format!("/api/v1/conversations/{id}"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(
        initial["messages"][0]["presentation"],
        items[0]["presentation"]
    );
    assert_eq!(f.state.messages(id, 100).unwrap(), canonical);
    assert_eq!(
        f.state.source_message_page(id, 100, None, "").unwrap(),
        source
    );
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT text FROM messages WHERE id='linked'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        internal
    );
    assert!(
        !f._home.path().join("frames.jsonl").exists(),
        "History reads never invoke ACP"
    );
}

#[tokio::test]
async fn spoken_history_missing_or_malformed_labels_are_neutral_not_backend_text() {
    let f = fixture();
    let room = f.state.create("Voice labels").unwrap();
    let id = room["id"].as_str().unwrap();
    {
        let db = f.state.db.lock().unwrap();
        db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at) VALUES('call','owner','call','hash','default',?1,'closed',1,1)",[id]).unwrap();
        for (n, label) in [
            None,
            Some(""),
            Some("  "),
            Some("bad\0label"),
            Some("blob"),
            Some("large"),
            Some("ambiguous"),
        ]
        .into_iter()
        .enumerate()
        {
            let message = format!("request-{n}");
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','INTERNAL BACKEND INPUT','complete',1,1,'owner')",rusqlite::params![message,id]).unwrap();
            db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,request_text) VALUES('call',?1,1,'completed',?1,1,?2)",rusqlite::params![message,label]).unwrap();
        }
        db.execute(
            "UPDATE companion_delegations SET request_text=zeroblob(4) WHERE id='request-4'",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE companion_delegations SET request_text=?1 WHERE id='request-5'",
            ["x".repeat(49_153)],
        )
        .unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,request_text) VALUES('call','duplicate',1,'completed','request-6',1,'Other request')",[]).unwrap();
    }
    let before = f.state.messages(id, 100).unwrap();
    let (_, history) = request(
        &f.app,
        "GET",
        &format!("/api/v1/conversations/{id}/messages"),
        Some(&f.admin),
        None,
    )
    .await;
    for message in history["items"].as_array().unwrap() {
        assert_eq!(message["presentation"]["text"], "Spoken request");
        assert_eq!(message["text"], "INTERNAL BACKEND INPUT");
    }
    assert_eq!(f.state.messages(id, 100).unwrap(), before);
}

#[tokio::test]
async fn removing_a_person_revokes_access_even_during_active_work() {
    let f = fixture();
    let c = f.state.create("Revocation during work").unwrap();
    let id = c["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}");
    assert_eq!(
        request(
            &f.app,
            "POST",
            &format!("{path}/members"),
            Some(&f.admin),
            Some(json!({"username":"robin","share_history":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let grant = context::grant(&f.state, id).unwrap();
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    f.state
        .running
        .lock()
        .unwrap()
        .insert(id.to_string(), cancel);
    assert_eq!(
        request(
            &f.app,
            "DELETE",
            &format!("{path}/members/robin"),
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(*receiver.borrow());
    assert_eq!(
        user_request(&f, "robin", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(request(&f.app,"POST","/context/v1/query",Some(&f.context),Some(json!({"operation":"read","access":{"mode":"grant","grant":grant},"conversation_id":id}))).await.0,StatusCode::FORBIDDEN);
    f.state.running.lock().unwrap().remove(id);
}
#[tokio::test]
async fn provider_search_uses_room_scoped_history_cursors() {
    let f = fixture();
    let c = f.state.create("Provider history").unwrap();
    let id = c["id"].as_str().unwrap();
    for n in 0..11 {
        f.state.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','search phrase','complete',?3,?3,'owner')",rusqlite::params![format!("source-{n}"),id,n]).unwrap();
    }
    let (_,v)=request(&f.app,"POST","/context/v1/query",Some(&f.context),Some(json!({"operation":"search","access":{"mode":"owner"},"conversation_id":id,"query":"search phrase","limit":4}))).await;
    let cursor = v["next_cursor"].as_i64().unwrap();
    assert_eq!(v["messages"].as_array().unwrap().len(), 4);
    assert_eq!(v["complete"], false);
    let (_,next)=request(&f.app,"POST","/context/v1/query",Some(&f.context),Some(json!({"operation":"search","access":{"mode":"owner"},"conversation_id":id,"query":"search phrase","limit":4,"before":cursor}))).await;
    assert_eq!(next["messages"].as_array().unwrap().len(), 4);
    assert!(
        next["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["seq"].as_i64().unwrap() < cursor)
    );
}

#[tokio::test]
async fn agent_context_sources_intersect_membership_and_every_grant_scope() {
    let f = fixture();
    let agent = f.state.config.agent.clone().unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"native","config":agent}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let pony = f.state.create("Helper only").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let own = f
        .state
        .create_room("Native private", "owner", &["native".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shared = f
        .state
        .create_room("Together", "owner", &["default".into(), "native".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let robin = f
        .state
        .create_room("Robin private", "robin", &["native".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (id, text) in [
        (&pony, "Helper-only secret"),
        (&own, "Native context"),
        (&shared, "Shared context"),
        (&robin, "Robin-only secret"),
    ] {
        f.state.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',1,1,'owner')",rusqlite::params![format!("message-{id}"),id,text]).unwrap();
    }
    let credential_path = "/api/v1/agents/native/context-credential";
    let (status, issued) = request(
        &f.app,
        "POST",
        credential_path,
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = issued["token"].as_str().unwrap();
    let path = issued["query_path"].as_str().unwrap();
    assert_eq!(path, "/agents/native/context/v1/query");
    assert_eq!(issued["scope"], "context:native");
    assert_eq!(issued["provider_id"], f.state.config.provider_id());
    assert_eq!(
        issued["query_url"],
        format!("https://example.test:18445{path}")
    );
    let owner = json!({"mode":"owner"});
    let (_, list) = request(
        &f.app,
        "POST",
        path,
        Some(token),
        Some(json!({"operation":"list","access":owner})),
    )
    .await;
    let mut ids: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    ids.sort();
    let mut expected = vec![own.as_str(), shared.as_str()];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(!list.to_string().contains("Helper"));
    assert!(!list.to_string().contains("Robin private"));
    for operation in ["authorize", "read", "orientation", "search"] {
        assert_eq!(request(&f.app,"POST",path,Some(token),Some(json!({"operation":operation,"access":owner,"conversation_id":pony,"query":"secret"}))).await.0,StatusCode::FORBIDDEN, "{operation}");
        assert_eq!(
            request(
                &f.app,
                "POST",
                path,
                Some(token),
                Some(json!({"operation":operation,"access":owner,"conversation_id":robin}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, own_read) = request(
            &f.app,
            "POST",
            path,
            Some(token),
            Some(json!({"operation":operation,"access":owner,"conversation_id":own})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{own_read}");
    }
    for other_path in ["/context/v1/query", "/agents/default/context/v1/query"] {
        assert_eq!(
            request(
                &f.app,
                "POST",
                other_path,
                Some(token),
                Some(json!({"operation":"list","access":owner}))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(&f.app, "GET", "/api/v1/conversations", Some(token), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            path,
            Some(&f.context),
            Some(json!({"operation":"list","access":owner}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            path,
            Some(json!({"operation":"list","access":owner}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // The legacy source keeps the pre-existing default-agent history contract.
    let (_, legacy) = request(
        &f.app,
        "POST",
        "/context/v1/query",
        Some(&f.context),
        Some(json!({"operation":"list","access":owner})),
    )
    .await;
    assert!(
        legacy["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == pony)
    );
    let pony_grant = context::grant(&f.state, &pony).unwrap();
    for operation in ["authorize", "list", "read", "orientation", "search"] {
        assert_eq!(request(&f.app,"POST",path,Some(token),Some(json!({"operation":operation,"access":{"mode":"grant","grant":pony_grant},"conversation_id":own}))).await.0,StatusCode::FORBIDDEN);
    }
    let own_grant = context::grant(&f.state, &own).unwrap();
    let (_, list) = request(
        &f.app,
        "POST",
        path,
        Some(token),
        Some(json!({"operation":"list","access":{"mode":"grant","grant":own_grant}})),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert!(!list.to_string().contains("Helper"));
    let shared_grant = context::grant(&f.state, &shared).unwrap();
    let shared_access = json!({"mode":"grant","grant":shared_grant});
    let (_, authorization) = request(
        &f.app,
        "POST",
        path,
        Some(token),
        Some(json!({"operation":"authorize","access":shared_access})),
    )
    .await;
    assert_eq!(authorization["owner"], false);
    assert_eq!(authorization["actor_id"], format!("room:{shared}"));
    let (_, list) = request(
        &f.app,
        "POST",
        path,
        Some(token),
        Some(json!({"operation":"list","access":shared_access})),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["id"], shared);
    assert_eq!(
        request(
            &f.app,
            "POST",
            path,
            Some(token),
            Some(json!({"operation":"read","access":shared_access,"conversation_id":own}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let sam_grant = context::grant(&f.state, &robin).unwrap();
    let (_, sam_read) = request(
        &f.app,
        "POST",
        path,
        Some(token),
        Some(json!({"operation":"read","access":{"mode":"grant","grant":sam_grant}})),
    )
    .await;
    assert_eq!(sam_read["messages"][0]["text"], "Robin-only secret");
    // Removal changes the source immediately, regardless of previously issued grants.
    assert_eq!(
        request(
            &f.app,
            "DELETE",
            &format!("/api/v1/conversations/{shared}/agents/native"),
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    for access in [owner.clone(), shared_access] {
        for operation in ["authorize", "read", "orientation", "search"] {
            assert_eq!(
                request(
                    &f.app,
                    "POST",
                    path,
                    Some(token),
                    Some(json!({"operation":operation,"access":access,"conversation_id":shared}))
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
        }
    }
    let (_, after) = request(
        &f.app,
        "POST",
        path,
        Some(token),
        Some(json!({"operation":"list","access":owner})),
    )
    .await;
    assert_eq!(after["items"].as_array().unwrap().len(), 1);
    assert_eq!(after["items"][0]["id"], own);
}

#[tokio::test]
async fn agent_context_credential_lifecycle_is_private_persistent_and_revocable() {
    let f = fixture();
    let agent = f.state.config.agent.clone().unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"native","config":agent}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let credential_path = "/api/v1/agents/native/context-credential";
    let query_path = "/agents/native/context/v1/query";
    let query = json!({"operation":"list","access":{"mode":"owner"}});
    assert_eq!(
        user_request(&f, "robin", "POST", credential_path, Some(json!({})))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents/missing/context-credential",
            Some(&f.admin),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (_, issued) = request(
        &f.app,
        "POST",
        credential_path,
        Some(&f.admin),
        Some(json!({"expires_in_seconds":600})),
    )
    .await;
    let first = issued["token"].as_str().unwrap();
    let (_, metadata) = request(&f.app, "GET", credential_path, Some(&f.admin), None).await;
    assert_eq!(metadata["enabled"], true);
    assert!(metadata.get("token").is_none());
    assert!(!metadata.to_string().contains(first));
    assert_eq!(
        request(&f.app, "POST", query_path, Some(first), Some(query.clone()))
            .await
            .0,
        StatusCode::OK
    );
    let restarted = server::router(AppState::open(f._home.path().into()).unwrap());
    assert_eq!(
        request(
            &restarted,
            "POST",
            query_path,
            Some(first),
            Some(query.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, rotated) = request(
        &f.app,
        "POST",
        credential_path,
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    let second = rotated["token"].as_str().unwrap();
    assert_ne!(first, second);
    assert_eq!(
        request(&f.app, "POST", query_path, Some(first), Some(query.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            query_path,
            Some(second),
            Some(query.clone())
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&f.app, "DELETE", credential_path, Some(&f.admin), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            query_path,
            Some(second),
            Some(query.clone())
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, issued) = request(
        &f.app,
        "POST",
        credential_path,
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    let expired = issued["token"].as_str().unwrap();
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE tokens SET expires_at=0 WHERE hash=?1",
            [auth::digest(expired)],
        )
        .unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            query_path,
            Some(expired),
            Some(query.clone())
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, issued) = request(
        &f.app,
        "POST",
        credential_path,
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    let removed = issued["token"].as_str().unwrap();
    assert_eq!(
        request(
            &f.app,
            "DELETE",
            "/api/v1/agents/native",
            Some(&f.admin),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"native","config":agent}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&f.app, "POST", query_path, Some(removed), Some(query))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, activity) = request(&f.app, "GET", "/api/v1/activity", Some(&f.admin), None).await;
    for secret in [first, second, expired, removed] {
        assert!(!activity.to_string().contains(secret));
    }
}

#[tokio::test]
async fn legacy_default_context_excludes_native_only_rooms_and_preserves_imports() {
    let f = fixture();
    let agent = f.state.config.agent.clone().unwrap();
    assert_eq!(
        request(
            &f.app,
            "POST",
            "/api/v1/agents",
            Some(&f.admin),
            Some(json!({"id":"native","config":agent}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let default = f.state.create("Helper private").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let native = f
        .state
        .create_room("Native only", "owner", &["native".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let importer = std::fs::read_to_string(f._home.path().join("secrets/import-token")).unwrap();
    let snapshot = json!({"label":"Earlier Toad history","surface":"toad","messages":[{"id":"u1","role":"user","text":"Keep my original imported history","created_at":1000},{"id":"a1","role":"assistant","text":"Helper original reply","created_at":2000}]});
    let (status, imported) = request(
        &f.app,
        "PUT",
        "/api/v1/imports/toad/old-default-session",
        Some(&importer),
        Some(snapshot.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let imported_id = imported["conversation_id"].as_str().unwrap();
    assert_eq!(
        f.state.room_agents(imported_id).unwrap()[0]["id"],
        "default"
    );
    let original = f.state.messages(imported_id, 100).unwrap();
    // Model the existing pre-0.4.2 layout: import data exists, source membership does not.
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM room_agents WHERE conversation_id=?1",
            [imported_id],
        )
        .unwrap();
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM migrations WHERE name='default-agent-import-sources'",
            [],
        )
        .unwrap();
    let restarted = AppState::open(f._home.path().into()).unwrap();
    assert_eq!(restarted.messages(imported_id, 100).unwrap(), original);
    assert_eq!(
        restarted.room_agents(imported_id).unwrap()[0]["id"],
        "default"
    );
    assert_eq!(
        std::fs::read_to_string(f._home.path().join("secrets/context-token")).unwrap(),
        f.context
    );
    let app = server::router(restarted.clone());
    let (_, default_credential) = request(
        &app,
        "POST",
        "/api/v1/agents/default/context-credential",
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    let (_, native_credential) = request(
        &app,
        "POST",
        "/api/v1/agents/native/context-credential",
        Some(&f.admin),
        Some(json!({})),
    )
    .await;
    let owner = json!({"mode":"owner"});
    let grant = context::grant(&restarted, &native).unwrap();
    for (route, token) in [
        ("/context/v1/query", f.context.as_str()),
        (
            "/agents/default/context/v1/query",
            default_credential["token"].as_str().unwrap(),
        ),
    ] {
        let (_, list) = request(
            &app,
            "POST",
            route,
            Some(token),
            Some(json!({"operation":"list","access":owner})),
        )
        .await;
        let items = list["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|v| v["id"] == default));
        assert!(items.iter().any(|v| v["id"] == imported_id));
        assert!(!list.to_string().contains("Native only"));
        for operation in ["authorize", "list", "read", "orientation", "search"] {
            assert_eq!(
                request(
                    &app,
                    "POST",
                    route,
                    Some(token),
                    Some(json!({"operation":operation,"access":owner,"conversation_id":native}))
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(request(&app,"POST",route,Some(token),Some(json!({"operation":operation,"access":{"mode":"grant","grant":grant},"conversation_id":default}))).await.0,StatusCode::FORBIDDEN);
        }
        let (status, read) = request(
            &app,
            "POST",
            route,
            Some(token),
            Some(json!({"operation":"read","access":owner,"conversation_id":imported_id})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["surface"], "toad");
        assert_eq!(
            read["messages"][0]["text"],
            "Keep my original imported history"
        );
        assert_eq!(read["messages"][1]["text"], "Helper original reply");
    }
    let native_token = native_credential["token"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/agents/native/context/v1/query",
            Some(native_token),
            Some(json!({"operation":"read","access":owner,"conversation_id":imported_id}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, repeat) = request(
        &app,
        "PUT",
        "/api/v1/imports/toad/old-default-session",
        Some(&importer),
        Some(snapshot),
    )
    .await;
    assert_eq!(repeat["conversation_id"], imported_id);
    assert_eq!(repeat["appended"], 0);
    let again = AppState::open(f._home.path().into()).unwrap();
    assert_eq!(again.messages(imported_id, 100).unwrap(), original);
    assert_eq!(again.room_agents(imported_id).unwrap().len(), 1);
}

async fn pending_form(f: &Fixture, id: &str, actor: &str) -> Value {
    for _ in 0..200 {
        let (_, room) = user_request(
            f,
            actor,
            "GET",
            &format!("/api/v1/conversations/{id}"),
            None,
        )
        .await;
        if let Some(form) = room["elicitations"]
            .as_array()
            .and_then(|forms| forms.first())
        {
            return form.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Agent did not ask its question");
}

#[tokio::test]
async fn structured_answers_are_private_validated_and_only_sent_on_explicit_submission() {
    let f = fixture();
    let room = f.state.create("Questions").unwrap();
    let id = room["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}");
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &format!("{path}/members"),
            Some(json!({"username":"robin","share_history":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &format!("{path}/messages"),
            Some(json!({"text":"request form","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let form = pending_form(&f, id, "robin").await;
    let route = format!("{path}/elicitations/{}", form["id"].as_str().unwrap());
    assert_eq!(
        user_request(&f, "owner", "GET", &path, None).await.1["elicitations"],
        json!([])
    );
    let accept = json!({"action":"accept","content":{"strategy":"careful","count":3,"note":"private-answer-sentinel"}});
    assert_eq!(
        user_request(&f, "owner", "POST", &route, Some(accept.clone()))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        user_request(&f, "casey", "POST", &route, Some(accept.clone()))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for bad in [
        json!({"strategy":"invented","count":1}),
        json!({"strategy":"careful","count":99}),
        json!({"strategy":"careful"}),
    ] {
        assert_eq!(
            user_request(
                &f,
                "robin",
                "POST",
                &route,
                Some(json!({"action":"accept","content":bad}))
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        user_request(&f, "robin", "POST", &route, Some(accept.clone()))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(&f, "robin", "POST", &route, Some(accept))
            .await
            .0,
        StatusCode::CONFLICT
    );
    wait_idle(&f, id).await;
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    let frames: Vec<Value> = frames
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(frames.iter().any(|f| f["method"] == "initialize"
        && f["params"]["clientCapabilities"]["elicitation"]["form"] == json!({})));
    assert!(frames.iter().any(
        |f| f["id"] == "form-1" && f["result"]["content"]["note"] == "private-answer-sentinel"
    ));
    let (_, activity) = user_request(&f, "robin", "GET", "/api/v1/activity", None).await;
    assert!(!activity.to_string().contains("private-answer-sentinel"));
    assert!(
        !user_request(&f, "owner", "GET", &path, None)
            .await
            .1
            .to_string()
            .contains("private-answer-sentinel")
    );
}

#[tokio::test]
async fn agent_withdrawal_and_turn_cancellation_remove_unanswered_forms() {
    let f = fixture();
    let room = f.state.create("Withdraw").unwrap();
    let id = room["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{id}");
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &format!("{path}/messages"),
            Some(
                json!({"text":"request form and withdraw form","request_id":uuid::Uuid::new_v4()})
            )
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let form = pending_form(&f, id, "owner").await;
    wait_idle(&f, id).await;
    assert!(f.state.elicitations.list(id, "owner").is_empty());
    let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
    assert!(
        frames
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .any(|f| f["id"] == "form-1" && f["result"] == json!({"action":"cancel"}))
    );
    let old = format!("{path}/elicitations/{}", form["id"].as_str().unwrap());
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &old,
            Some(json!({"action":"accept","content":{"strategy":"careful","count":1}}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &format!("{path}/messages"),
            Some(json!({"text":"request form again","request_id":uuid::Uuid::new_v4()}))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    pending_form(&f, id, "owner").await;
    assert_eq!(
        user_request(
            &f,
            "owner",
            "POST",
            &format!("{path}/cancel"),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_idle(&f, id).await;
    assert!(f.state.elicitations.list(id, "owner").is_empty());
}

#[tokio::test]
async fn request_scoped_forms_work_during_initialization_creation_and_loading() {
    for phase in ["initialize", "session/new", "session/load"] {
        let f = fixture();
        let mut agent = f.state.config.agent.clone().unwrap();
        agent.env.insert("FIXTURE_FORM_PHASE".into(), phase.into());
        assert_eq!(
            request(
                &f.app,
                "POST",
                "/api/v1/agents",
                Some(&f.admin),
                Some(json!({"id":"forms","config":agent}))
            )
            .await
            .0,
            StatusCode::OK
        );
        let room = f
            .state
            .create_room("Setup questions", "owner", &["forms".into()])
            .unwrap();
        let id = room["id"].as_str().unwrap();
        let path = format!("/api/v1/conversations/{id}");
        if phase == "session/load" {
            assert_eq!(
                user_request(
                    &f,
                    "owner",
                    "POST",
                    &format!("{path}/messages"),
                    Some(json!({"text":"First","request_id":uuid::Uuid::new_v4()}))
                )
                .await
                .0,
                StatusCode::ACCEPTED
            );
            wait_idle(&f, id).await;
        }
        assert_eq!(
            user_request(
                &f,
                "owner",
                "POST",
                &format!("{path}/messages"),
                Some(json!({"text":"Continue","request_id":uuid::Uuid::new_v4()}))
            )
            .await
            .0,
            StatusCode::ACCEPTED
        );
        let form = pending_form(&f, id, "owner").await;
        assert_eq!(
            user_request(
                &f,
                "owner",
                "POST",
                &format!("{path}/elicitations/{}", form["id"].as_str().unwrap()),
                Some(json!({"action":"accept","content":{"strategy":"careful","count":1}}))
            )
            .await
            .0,
            StatusCode::OK
        );
        wait_idle(&f, id).await;
        assert_eq!(
            f.state.conversation(id).unwrap().unwrap()["status"],
            "idle",
            "{phase}"
        );
    }
}

#[tokio::test]
async fn interactive_agent_auth_is_requester_bound_and_retries_only_setup() {
    for decision in ["accept", "decline", "cancel"] {
        let f = fixture();
        let mut agent = f.state.config.agent.clone().unwrap();
        agent.env.insert("FIXTURE_AUTH".into(), "1".into());
        assert_eq!(
            request(
                &f.app,
                "POST",
                "/api/v1/agents",
                Some(&f.admin),
                Some(json!({"id":"auth","config":agent}))
            )
            .await
            .0,
            StatusCode::OK
        );
        let room = f
            .state
            .create_room("Sign-in", "owner", &["auth".into()])
            .unwrap();
        let id = room["id"].as_str().unwrap();
        let path = format!("/api/v1/conversations/{id}");
        user_request(
            &f,
            "owner",
            "POST",
            &format!("{path}/members"),
            Some(json!({"username":"robin","share_history":true})),
        )
        .await;
        user_request(
            &f,
            "robin",
            "POST",
            &format!("{path}/messages"),
            Some(json!({"text":"Say hello","request_id":uuid::Uuid::new_v4()})),
        )
        .await;
        let form = pending_form(&f, id, "robin").await;
        assert_eq!(
            form["schema"]["properties"]["method_id"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let route = format!("{path}/elicitations/{}", form["id"].as_str().unwrap());
        assert_eq!(
            user_request(&f, "owner", "GET", &path, None).await.1["elicitations"],
            json!([])
        );
        assert_eq!(
            user_request(
                &f,
                "owner",
                "POST",
                &route,
                Some(json!({"action":"accept","content":{"method_id":"browser"}}))
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            user_request(
                &f,
                "robin",
                "POST",
                &route,
                Some(json!({"action":"accept","content":{"method_id":"terminal"}}))
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        user_request(
            &f,
            "robin",
            "POST",
            &route,
            Some(json!({"action":decision,"content":{"method_id":"browser"}})),
        )
        .await;
        if decision == "accept" {
            let url = pending_form(&f, id, "robin").await;
            assert_eq!(url["mode"], "url");
            assert_eq!(url["host"], "example.test");
            assert_eq!(
                user_request(&f, "owner", "GET", &path, None).await.1["elicitations"],
                json!([])
            );
            assert_eq!(
                user_request(
                    &f,
                    "robin",
                    "POST",
                    &format!("{path}/elicitations/{}", url["id"].as_str().unwrap()),
                    Some(json!({"action":"accept","content":{"ignored":"never forwarded"}}))
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        wait_idle(&f, id).await;
        let frames = std::fs::read_to_string(f._home.path().join("frames.jsonl")).unwrap();
        let rows: Vec<Value> = frames
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            rows.iter()
                .filter(|r| r["method"] == "authenticate")
                .count(),
            usize::from(decision == "accept")
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r["method"] == "session/prompt")
                .count(),
            usize::from(decision == "accept")
        );
        assert!(!frames.contains("never forwarded"));
        let db = f.state.db.lock().unwrap();
        let events: Vec<String> = db
            .prepare("SELECT data FROM audit")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!events.join("").contains("https://example.test/authorize"));
    }
}

#[tokio::test]
async fn delivery_metadata_and_recovery_keep_membership_scope_and_source_authority() {
    let f = fixture();
    let c = f
        .state
        .create_room("Private recovery", "robin", &["default".into()])
        .unwrap();
    let id = c["id"].as_str().unwrap();
    {
        let db = f.state.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('source',?1,'user','PRIVATE-INPUT','complete',1,1,'robin')",[id]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,attention,updated_at) VALUES('delivery','source',?1,'default','direct','unknown','prompt','PRIVATE-TICKET',1)",[id]).unwrap();
    }
    let path = format!("/api/v1/conversations/{id}/deliveries");
    assert_eq!(
        request(&f.app, "GET", &path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&f.app, "GET", &path, Some(&f.context), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&f.app, "GET", &path, Some(&f.admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    let listed = user_request(&f, "robin", "GET", &path, None).await;
    assert_eq!(listed.0, StatusCode::OK);
    assert_eq!(listed.1["items"][0]["state"], "unknown");
    assert!(!listed.1.to_string().contains("PRIVATE-"));
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO room_members VALUES(?1,'owner','member',1)",
            [id],
        )
        .unwrap();
    let action = format!("{path}/delivery/resolve");
    let body = Some(json!({"disposition":"proceed_without_replay"}));
    assert_eq!(
        user_request(&f, "owner", "POST", &action, body.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        user_request(&f, "casey", "POST", &action, body.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(&f, "robin", "POST", &action, body).await.0,
        StatusCode::OK
    );
    wait_idle(&f, id).await;
    assert_eq!(
        user_request(&f, "robin", "GET", &path, None).await.1["items"][0]["state"],
        "dismissed"
    );
}

#[tokio::test]
async fn original_submission_lookup_recovers_old_delivery_without_resubmission() {
    let f = fixture();
    let room = f.state.create("Original submission").unwrap();
    let room = room["id"].as_str().unwrap();
    let original = uuid::Uuid::new_v4().to_string();
    // Treat the acknowledged response as lost to the caller. Recovery below
    // uses only GET against the original request identity, never another POST.
    let sent = request(
        &f.app,
        "POST",
        &format!("/api/v1/conversations/{room}/messages"),
        Some(&f.admin),
        Some(json!({"request_id":original,"text":"Original submission PRIVATE-TEXT"})),
    )
    .await;
    assert_eq!(sent.0, StatusCode::ACCEPTED);
    wait_idle(&f, room).await;
    let original_delivery: String = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT id FROM input_deliveries WHERE message_id=?1",
            [&original],
            |r| r.get(0),
        )
        .unwrap();
    // Deterministic retained history puts the real completed submission beyond
    // the ordinary latest-256 projection without dispatching hundreds of turns.
    {
        let mut db = f.state.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        for index in 0..300 {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','Later retained source','complete',?3,?3,'owner')",rusqlite::params![id,room,index+1]).unwrap();
            tx.execute("INSERT INTO input_authority(message_id) VALUES(?1)", [&id])
                .unwrap();
            tx.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,updated_at) VALUES(?1,?2,?3,'default','direct','completed','prompt',?4)",rusqlite::params![format!("later-{index}"),id,room,index+1]).unwrap();
        }
        tx.execute("UPDATE input_deliveries SET attention='PRIVATE-TICKET',endpoint='PRIVATE-ENDPOINT' WHERE id=?1",[&original_delivery]).unwrap();
        tx.commit().unwrap();
    }
    let frames_before = std::fs::read(f._home.path().join("frames.jsonl")).unwrap();
    let counts_before: (i64, i64, i64) = f.state.db.lock().unwrap().query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM input_deliveries),(SELECT COUNT(*) FROM audit)",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    let path = format!("/api/v1/conversations/{room}/deliveries");
    let recent = request(&f.app, "GET", &path, Some(&f.admin), None).await;
    assert_eq!(recent.0, StatusCode::OK);
    assert_eq!(recent.1["items"].as_array().unwrap().len(), 256);
    assert!(
        !recent.1["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["message_id"] == original)
    );

    let lookup = format!("{path}?request_id={original}");
    let recovered = request(&f.app, "GET", &lookup, Some(&f.admin), None).await;
    assert_eq!(recovered.0, StatusCode::OK);
    assert_eq!(recovered.1["request_id"], original);
    assert_eq!(recovered.1["publication"]["state"], "saved");
    assert_eq!(recovered.1["publication"]["message_id"], original);
    assert_eq!(recovered.1["publication"]["conversation_id"], room);
    assert_eq!(recovered.1["publication"]["actor"], "owner");
    assert!(
        recovered.1["publication"]["source_sequence"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(
        recovered.1["observed_at"].as_i64().unwrap()
            >= recovered.1["publication"]["created_at"].as_i64().unwrap()
    );
    assert_eq!(recovered.1["coverage"], "retained_submission");
    assert_eq!(recovered.1["items"].as_array().unwrap().len(), 1);
    assert_eq!(recovered.1["items"][0]["id"], original_delivery);
    assert_eq!(recovered.1["items"][0]["state"], "completed");
    assert_eq!(recovered.1["items"][0]["message_id"], original);
    assert!(!recovered.1.to_string().contains("PRIVATE-"));
    let again = request(&f.app, "GET", &lookup, Some(&f.admin), None).await;
    assert_eq!(again.1["publication"], recovered.1["publication"]);
    assert_eq!(again.1["items"], recovered.1["items"]);
    let counts_after: (i64, i64, i64) = f.state.db.lock().unwrap().query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM input_deliveries),(SELECT COUNT(*) FROM audit)",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).unwrap();
    assert_eq!(counts_after, counts_before);
    assert_eq!(
        std::fs::read(f._home.path().join("frames.jsonl")).unwrap(),
        frames_before
    );
}

#[tokio::test]
async fn original_submission_lookup_preserves_zero_delivery_publication_and_current_access() {
    let f = fixture();
    let room = f
        .state
        .create_room("No agent recipients", "robin", &[])
        .unwrap();
    let room = room["id"].as_str().unwrap();
    let original = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &format!("/api/v1/conversations/{room}/messages"),
            Some(json!({"request_id":original,"text":"Saved for this conversation"}))
        )
        .await
        .0,
        StatusCode::ACCEPTED,
    );
    wait_idle(&f, room).await;
    let path = format!("/api/v1/conversations/{room}/deliveries?request_id={original}");
    assert_eq!(
        request(&f.app, "GET", &path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&f.app, "GET", &path, Some(&f.context), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&f.app, "GET", &path, Some(&f.admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(&f, "casey", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    let saved = user_request(&f, "robin", "GET", &path, None).await;
    assert_eq!(saved.0, StatusCode::OK);
    assert_eq!(saved.1["publication"]["state"], "saved");
    assert_eq!(saved.1["publication"]["actor"], "robin");
    assert_eq!(saved.1["items"], json!([]));
    assert!(!f._home.path().join("frames.jsonl").exists());

    let members = format!("/api/v1/conversations/{room}/members");
    assert_eq!(
        user_request(
            &f,
            "robin",
            "POST",
            &members,
            Some(json!({"username":"casey","share_history":true}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(&f, "casey", "GET", &path, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(&f, "robin", "DELETE", &format!("{members}/casey"), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(&f, "casey", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(&f, "robin", "GET", &path, None).await.1["publication"],
        saved.1["publication"]
    );

    let other = f.state.create_room("Other room", "robin", &[]).unwrap();
    assert_eq!(
        user_request(
            &f,
            "robin",
            "GET",
            &format!(
                "/api/v1/conversations/{}/deliveries?request_id={original}",
                other["id"].as_str().unwrap()
            ),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "GET",
            &format!(
                "/api/v1/conversations/{room}/deliveries?request_id={}",
                uuid::Uuid::new_v4()
            ),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "GET",
            &format!("/api/v1/conversations/{room}/deliveries?request_id=not-a-uuid"),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        user_request(
            &f,
            "robin",
            "DELETE",
            &format!("/api/v1/conversations/{room}"),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        user_request(&f, "robin", "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn original_submission_lookup_does_not_invent_intake_or_settle_unknown() {
    let f = fixture();
    let room = f
        .state
        .create_room("Retained source", "owner", &[])
        .unwrap();
    let room = room["id"].as_str().unwrap();
    let source = uuid::Uuid::new_v4().to_string();
    let import_like = uuid::Uuid::new_v4().to_string();
    let non_user = uuid::Uuid::new_v4().to_string();
    {
        let db = f.state.db.lock().unwrap();
        for (id, role) in [
            (&source, "user"),
            (&import_like, "user"),
            (&non_user, "assistant"),
        ] {
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,?3,'Retained source','complete',1,1,'owner')",rusqlite::params![id,room,role]).unwrap();
        }
        // Source intake is distinguished from an imported/legacy user row.
        for id in [&source, &non_user] {
            db.execute("INSERT INTO input_authority(message_id) VALUES(?1)", [id])
                .unwrap();
        }
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,updated_at) VALUES('uncertain-delivery',?1,?2,'default','direct','unknown','prompt',1)",rusqlite::params![source,room]).unwrap();
    }
    for id in [&import_like, &non_user] {
        assert_eq!(
            request(
                &f.app,
                "GET",
                &format!("/api/v1/conversations/{room}/deliveries?request_id={id}"),
                Some(&f.admin),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    let found = request(
        &f.app,
        "GET",
        &format!("/api/v1/conversations/{room}/deliveries?request_id={source}"),
        Some(&f.admin),
        None,
    )
    .await;
    assert_eq!(found.0, StatusCode::OK);
    assert_eq!(found.1["publication"]["state"], "saved");
    assert_eq!(found.1["items"][0]["state"], "unknown");
    assert_eq!(found.1["items"][0]["receipt"], Value::Null);
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM input_deliveries WHERE id='uncertain-delivery'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "unknown"
    );
    assert!(!f._home.path().join("frames.jsonl").exists());
}
