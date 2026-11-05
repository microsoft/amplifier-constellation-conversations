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
        agent: Agent {
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
        },
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
    let f = fixture();
    let mut connection = serde_json::to_value(&f.state.config.agent).unwrap();
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
    let load = rows.iter().find(|r| r["method"] == "session/load").unwrap();
    // The Python peer launched in its real local cwd while the remote path
    // crossed ACP unchanged on both creation and restore.
    assert_eq!(new["params"]["cwd"], "C:\\Remote Agent\\workspace");
    assert_eq!(load["params"]["cwd"], new["params"]["cwd"]);
    assert_eq!(
        new["params"]["mcpServers"],
        json!(f.state.config.agent.mcp_servers)
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
    let mut agent = f.state.config.agent.clone();
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
    for (name, mime, data) in [
        ("../escape.txt", "text/plain", b"x".as_slice()),
        ("active.svg", "image/svg+xml", b"<svg/>".as_slice()),
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
    let agent = f.state.config.agent.clone();
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
    let agent = f.state.config.agent.clone();
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
    let agent = f.state.config.agent.clone();
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
        let mut agent = f.state.config.agent.clone();
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
        let mut agent = f.state.config.agent.clone();
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
