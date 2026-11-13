//! Real HTTP/SQLite call boundaries; no provider, model, game or ACP process.
use agent_conversations::{
    auth, companion_tools,
    config::{Agent, Config},
    server,
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

struct Fixture {
    home: TempDir,
    state: Arc<AppState>,
    app: Router,
    admin: String,
}

struct Call {
    id: String,
    room: String,
    agent: String,
    token: String,
}

struct Source {
    delegation: String,
    message: String,
    text: String,
    at: i64,
}

fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uid(),
        label: "Automatic huddle tools fixture".into(),
        owner: "owner".into(),
        allowed_users: vec!["other".into()],
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
        auth_mode: Default::default(),
        transport: Default::default(),
        agent: Some(Agent {
            label: "Coach".into(),
            command: "must-not-launch-an-agent-in-this-test".into(),
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
    std::fs::write(
        home.path().join("companion-actions.json"),
        json!({"enabled":true,"agents":{"default":[],"other-agent":[]}}).to_string(),
    )
    .unwrap();
    let state = AppState::open(home.path().into()).unwrap();
    let app = server::router(state.clone());
    let admin = std::fs::read_to_string(home.path().join("secrets/control-token"))
        .unwrap()
        .trim()
        .to_owned();
    Fixture {
        home,
        state,
        app,
        admin,
    }
}

fn call_for(f: &Fixture, agent: &str) -> Call {
    let room = f
        .state
        .create_room("Private voice request", "owner", &[agent.to_owned()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = uid();
    let endpoint =
        auth::digest(&serde_json::to_string(&f.state.agent(agent).unwrap().unwrap()).unwrap());
    f.state.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture',?3,?4,'connected',?5,?5,?6)",params![id,uid(),agent,room,now_ms(),endpoint]).unwrap();
    let config = companion_tools::create_mcp_config(&f.state, &id).unwrap();
    Call {
        id,
        room,
        agent: agent.into(),
        token: config["token"].as_str().unwrap().to_owned(),
    }
}

fn call(f: &Fixture) -> Call {
    call_for(f, "default")
}

fn source(f: &Fixture, call: &Call, text: &str) -> Source {
    let source = Source {
        delegation: uid(),
        message: uid(),
        text: text.into(),
        at: now_ms(),
    };
    let turn = uid();
    let db = f.state.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')",params![source.message,call.room,source.text,source.at]).unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant','','streaming',?3,?3,'Coach',?4)",params![turn,call.room,source.at,call.agent]).unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,100,'dispatched',?3,?4)",params![call.id,source.delegation,source.message,source.at]).unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,target_turn,updated_at) VALUES(?1,?2,?3,?4,'direct','submitted','prompt',?5,?6)",params![uid(),source.message,call.room,call.agent,turn,source.at]).unwrap();
    db.execute(
        "INSERT INTO input_authority(message_id,token_hash) VALUES(?1,?2)",
        params![source.message, auth::digest(&f.admin)],
    )
    .unwrap();
    source
}

async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(
                    body.map(|value| Body::from(value.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"error":String::from_utf8_lossy(&bytes)}));
    (status, value)
}

async fn tools(
    f: &Fixture,
    call: &Call,
    method: &str,
    suffix: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    request(
        f,
        method,
        &format!("/api/v1/companion/calls/{}/tools/huddles{suffix}", call.id),
        &call.token,
        body,
    )
    .await
}

async fn ok_tools(
    f: &Fixture,
    call: &Call,
    method: &str,
    suffix: &str,
    body: Option<Value>,
) -> Value {
    let (status, value) = tools(f, call, method, suffix, body).await;
    assert!(status.is_success(), "{method} {suffix}: {status} {value}");
    value
}

async fn admin(f: &Fixture, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, value) = request(f, method, path, &f.admin, body).await;
    assert!(status.is_success(), "{method} {path}: {status} {value}");
    value
}

async fn install(
    f: &Fixture,
    id: &str,
    agent: &str,
    namespace: &str,
    local_agent: Option<&str>,
) -> String {
    let token = format!("{}{}", uid(), uid());
    admin(
        f,
        "PUT",
        &format!("/api/v1/huddle-installations/{id}"),
        Some(json!({
            "agent_id":agent,"label":format!("Label {agent}"),
            "connection_id":format!("connection-{id}"),"service":"minecraft",
            "conversation_id":format!("world-{id}"),"namespace":namespace,
            "local_agent_id":local_agent,"enabled":true,"token":token
        })),
    )
    .await;
    token
}

async fn pair(f: &Fixture) -> (String, String) {
    let lead = install(f, "lead", "coach", "test-world", Some("default")).await;
    let peer = install(f, "peer", "rook", "test-world", None).await;
    (lead, peer)
}

fn ensure_body(source: &Source, request: &str) -> Value {
    json!({"installation_id":"lead","request_id":request,"delegation_id":source.delegation,
        "title":"Compare current observations","purpose":"Review the original request together quietly",
        "peer_installation_ids":["peer"]})
}

async fn ensure(f: &Fixture, call: &Call, source: &Source) -> Value {
    ok_tools(
        f,
        call,
        "POST",
        "/ensure",
        Some(ensure_body(source, &uid())),
    )
    .await
}

fn count(f: &Fixture, table: &str) -> i64 {
    f.state
        .db
        .lock()
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn posts(source: &Source, kind: &str) -> Value {
    let mut body = json!({"installation_id":"lead","delegation_id":source.delegation,
        "request_id":uid(),"text":"The observations differ; the second count is still uncertain.",
        "target_agent_ids":["rook"],"kind":kind});
    if kind == "outcome" {
        body["outcome"] = json!({"kind":"result","evidence":[{"label":"Original comparison","reference":"fixture:read-only-observations"}]});
    }
    body
}

#[tokio::test]
async fn zero_room_ensure_binds_actual_source_and_current_installation_without_new_agent_work() {
    let f = fixture();
    let c = call(&f);
    let source = source(
        &f,
        &c,
        "Compare the two inventories and tell me the differences.",
    );
    let (lead_token, peer_token) = pair(&f).await;
    assert_eq!(count(&f, "huddles"), 0);
    let deliveries = count(&f, "input_deliveries");
    let bindings = count(&f, "room_agents");
    let result = ensure(&f, &c, &source).await;
    assert_eq!(result["status"], "saved");
    assert_eq!(result["reused"], false);
    assert_eq!(result["membership"], "active");
    assert_eq!(result["preference"]["mode"], "quiet");
    assert_eq!(result["origin"]["kind"], "conversation");
    assert_eq!(result["origin"]["conversation_id"], c.room);
    assert_eq!(result["origin"]["message_id"], source.message);
    assert_eq!(
        result["origin"]["actor"],
        json!({"id":"owner","kind":"human"})
    );
    assert_eq!(result["origin"]["occurred_at"], source.at);
    assert_eq!(
        result["origin"]["source_revision"],
        auth::digest(&source.text)
    );
    assert_eq!(result["participants"].as_array().unwrap().len(), 2);
    assert_eq!(count(&f, "huddles"), 1);
    assert_eq!(count(&f, "input_deliveries"), deliveries);
    assert_eq!(count(&f, "room_agents"), bindings);
    assert_eq!(count(&f, "companion_actions"), 0);
    assert_eq!(count(&f, "companion_events"), 0);
    let wire = result.to_string();
    for secret in [&lead_token, &peer_token, &c.token, &f.admin] {
        assert!(!wire.contains(secret));
    }
    let listed = ok_tools(
        &f,
        &c,
        "GET",
        "?installation_id=lead&after=0&limit=10",
        None,
    )
    .await;
    assert_eq!(listed["huddles"]["items"][0]["id"], result["id"]);
    let messages = ok_tools(
        &f,
        &c,
        "GET",
        &format!(
            "/{}/messages?installation_id=lead&after=0&limit=10",
            result["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert!(messages["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn source_identity_reuses_room_but_identical_text_in_another_human_request_does_not() {
    let f = fixture();
    let c = call(&f);
    pair(&f).await;
    let first_source = source(&f, &c, "Please compare your observations.");
    let first_request = uid();
    let first = ok_tools(
        &f,
        &c,
        "POST",
        "/ensure",
        Some(ensure_body(&first_source, &first_request)),
    )
    .await;
    let duplicate = ok_tools(
        &f,
        &c,
        "POST",
        "/ensure",
        Some(ensure_body(&first_source, &first_request)),
    )
    .await;
    assert_eq!(duplicate, first);
    let new_operation = ensure(&f, &c, &first_source).await;
    assert_eq!(new_operation["id"], first["id"]);
    assert_eq!(new_operation["reused"], true);
    assert_eq!(count(&f, "huddles"), 1);
    assert_eq!(count(&f, "huddle_auto_members"), 2);
    let second_source = source(&f, &c, &first_source.text);
    let second = ensure(&f, &c, &second_source).await;
    assert_ne!(second["id"], first["id"]);
    assert_ne!(
        second["origin"]["message_id"],
        first["origin"]["message_id"]
    );
    assert_eq!(count(&f, "huddles"), 2);
    let mut changed = ensure_body(&first_source, &first_request);
    changed["purpose"] = json!("A different task under the spent operation ID");
    assert_eq!(
        tools(&f, &c, "POST", "/ensure", Some(changed)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&f, "huddles"), 2);
}

#[tokio::test]
async fn discovery_requires_explicit_installation_and_cannot_select_another_agents_route() {
    let f = fixture();
    let c = call(&f);
    let agent = serde_json::to_value(f.state.agent("default").unwrap().unwrap()).unwrap();
    admin(
        &f,
        "POST",
        "/api/v1/agents",
        Some(json!({"id":"other-agent","config":agent})),
    )
    .await;
    install(&f, "lead-a", "coach-a", "pool-a", Some("default")).await;
    install(&f, "lead-b", "coach-b", "pool-b", Some("default")).await;
    install(&f, "other-local", "other", "pool-a", Some("other-agent")).await;
    install(&f, "peer-a", "peer", "pool-a", None).await;
    let choice = ok_tools(&f, &c, "GET", "", None).await;
    let ids: Vec<_> = choice["installations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["lead-a", "lead-b"]);
    assert_eq!(choice["availability"], "unknown");
    assert_eq!(
        tools(&f, &c, "GET", "?after=0&limit=1", None).await.0,
        StatusCode::BAD_REQUEST
    );
    let scoped = ok_tools(&f, &c, "GET", "?installation_id=lead-a", None).await;
    assert_eq!(scoped["installation"]["id"], "lead-a");
    let peers = scoped["roster"]["items"].as_array().unwrap();
    assert!(
        peers
            .iter()
            .all(|v| v["namespace"] == "pool-a" && v["availability"] == "unknown")
    );
    assert!(!peers.iter().any(|v| v["id"] == "lead-b"));
    assert_eq!(
        tools(&f, &c, "GET", "?installation_id=other-local", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let other = call_for(&f, "other-agent");
    let other_choice = ok_tools(&f, &other, "GET", "", None).await;
    assert_eq!(other_choice["installations"].as_array().unwrap().len(), 1);
    assert_eq!(other_choice["installations"][0]["id"], "other-local");
    assert_eq!(
        tools(&f, &other, "GET", "?installation_id=lead-a", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(count(&f, "huddles"), 0);
}

#[tokio::test]
async fn ensure_lost_ack_is_read_by_original_request_without_a_room_id_even_after_restart() {
    let mut f = fixture();
    let c = call(&f);
    pair(&f).await;
    let source = source(&f, &c, "Review this request quietly together.");
    let request_id = uid();
    let saved = ok_tools(
        &f,
        &c,
        "POST",
        "/ensure",
        Some(ensure_body(&source, &request_id)),
    )
    .await;
    // The caller discarded the reply. Only its durable operation UUID is used.
    f.state = AppState::open(f.home.path().into()).unwrap();
    f.app = server::router(f.state.clone());
    let recovered = ok_tools(
        &f,
        &c,
        "GET",
        &format!("/ensure/{request_id}?installation_id=lead"),
        None,
    )
    .await;
    assert_eq!(recovered, saved);
    let before = count(&f, "huddles");
    assert_eq!(
        tools(
            &f,
            &c,
            "GET",
            &format!("/ensure/{}?installation_id=lead", uid()),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        count(&f, "huddles"),
        before,
        "Missing status must not create a replacement"
    );
    assert_eq!(count(&f, "huddle_ensures"), 1);
}

#[tokio::test]
async fn ended_call_blocks_new_writes_but_permits_its_original_exact_receipts() {
    let f = fixture();
    let c = call(&f);
    pair(&f).await;
    let source = source(&f, &c, "Review the current observations.");
    let request_id = uid();
    let ensured = ok_tools(
        &f,
        &c,
        "POST",
        "/ensure",
        Some(ensure_body(&source, &request_id)),
    )
    .await;
    let room = ensured["id"].as_str().unwrap();
    let body = posts(&source, "message");
    let posted = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(body.clone()),
    )
    .await;
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&c.id],
        )
        .unwrap();
    for (path, body) in [
        ("/ensure".to_owned(), ensure_body(&source, &uid())),
        (format!("/{room}/posts"), posts(&source, "message")),
    ] {
        let status = tools(&f, &c, "POST", &path, Some(body)).await.0;
        assert!(matches!(
            status,
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ));
    }
    let recovered = ok_tools(
        &f,
        &c,
        "GET",
        &format!("/ensure/{request_id}?installation_id=lead"),
        None,
    )
    .await;
    assert_eq!(recovered["id"], room);
    let post_receipt = ok_tools(
        &f,
        &c,
        "GET",
        &format!(
            "/{room}/posts/{}?installation_id=lead",
            body["request_id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(post_receipt, posted);
    assert_eq!(count(&f, "huddle_posts"), 1);
    assert_eq!(count(&f, "huddles"), 1);
}

#[tokio::test]
async fn routine_posts_stay_quiet_and_an_outcome_uses_the_original_parent_card_path() {
    let f = fixture();
    let c = call(&f);
    pair(&f).await;
    let source = source(
        &f,
        &c,
        "Compare the observations and report the meaningful result.",
    );
    let ensured = ensure(&f, &c, &source).await;
    let room = ensured["id"].as_str().unwrap();
    let deliveries = count(&f, "input_deliveries");
    let routine = posts(&source, "message");
    let routine_receipt = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(routine.clone()),
    )
    .await;
    assert_eq!(routine_receipt["return"]["state"], "not_requested");
    assert_eq!(count(&f, "companion_events"), 0);
    assert_eq!(count(&f, "companion_voice_batches"), 0);
    let outcome = posts(&source, "outcome");
    let receipt = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(outcome.clone()),
    )
    .await;
    assert_eq!(receipt["return"]["state"], "stored");
    assert_eq!(receipt["return"]["delivery"], "companion_owned");
    assert_eq!(receipt["return"]["call_id"], c.id);
    assert_eq!(receipt["return"]["conversation_id"], c.room);
    assert_eq!(receipt["origin"]["message_id"], source.message);
    assert!(receipt["return"]["heard_by_human"].is_null());
    assert!(receipt["return"]["read_by_human"].is_null());
    {
        let db = f.state.db.lock().unwrap();
        let events: Vec<(String, Value)> = db
            .prepare("SELECT kind,data FROM companion_events WHERE call_id=?1 ORDER BY seq")
            .unwrap()
            .query_map([&c.id], |r| {
                Ok((
                    r.get(0)?,
                    serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap(),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|(kind, _)| kind == "huddle_return")
                .count(),
            1
        );
        let card = &events.iter().find(|(kind, _)| kind == "card").unwrap().1;
        assert_eq!(card["huddle_id"], room);
        assert!(card.to_string().contains(outcome["text"].as_str().unwrap()));
        let mirror: String = db
            .query_row(
                "SELECT conversation_id FROM messages WHERE id=?1",
                [receipt["return"]["message_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mirror, c.room);
    }
    let replay = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(outcome.clone()),
    )
    .await;
    assert_eq!(replay, receipt);
    assert_eq!(count(&f, "huddle_posts"), 2);
    assert_eq!(
        count(&f, "input_deliveries"),
        deliveries,
        "Room posts do not launch a second model/session"
    );
    let page = ok_tools(
        &f,
        &c,
        "GET",
        &format!("/{room}/messages?installation_id=lead&after=0&limit=10"),
        None,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["id"], routine["request_id"]);
    assert_eq!(page["items"][1]["id"], outcome["request_id"]);
}

#[tokio::test]
async fn wrong_delegation_source_actor_and_model_selected_origin_cannot_create_a_room() {
    for case in [
        "missing",
        "other_call",
        "assistant_source",
        "other_actor",
        "foreign_parent",
        "extra_origin",
    ] {
        let f = fixture();
        let c = call(&f);
        pair(&f).await;
        let source = source(&f, &c, "Please compare the observations.");
        let mut body = ensure_body(&source, &uid());
        match case {
            "missing" => body["delegation_id"] = json!("not-a-retained-delegation"),
            "other_call" => {
                let other = call(&f);
                let foreign = self::source(&f, &other, "Different private request");
                body["delegation_id"] = json!(foreign.delegation);
            }
            "assistant_source" => {
                f.state
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE messages SET role='assistant' WHERE id=?1",
                        [&source.message],
                    )
                    .unwrap();
            }
            "other_actor" => {
                f.state
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE messages SET actor='other' WHERE id=?1",
                        [&source.message],
                    )
                    .unwrap();
            }
            "foreign_parent" => {
                let other = call(&f);
                f.state
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE messages SET conversation_id=?1 WHERE id=?2",
                        params![other.room, source.message],
                    )
                    .unwrap();
            }
            "extra_origin" => {
                body["origin"] = json!({"kind":"conversation","conversation_id":c.room,"actor":{"id":"owner","kind":"human"}})
            }
            _ => unreachable!(),
        }
        let (status, value) = tools(&f, &c, "POST", "/ensure", Some(body)).await;
        assert!(status.is_client_error(), "{case}: {status} {value}");
        assert_eq!(count(&f, "huddles"), 0, "{case}");
        assert_eq!(count(&f, "huddle_ensures"), 0, "{case}");
    }
}

#[tokio::test]
async fn continued_contribution_retains_original_return_and_binds_current_source_separately() {
    let f = fixture();
    let c = call(&f);
    pair(&f).await;
    let original = source(&f, &c, "Review the first request.");
    let ensured = ensure(&f, &c, &original).await;
    let room = ensured["id"].as_str().unwrap();
    let correction = source(
        &f,
        &c,
        "Continue that discussion, but compare only the first two observations.",
    );
    let mut body = posts(&correction, "message");
    body["text"] = json!("The caller has narrowed the comparison to the first two observations.");
    let receipt = ok_tools(&f, &c, "POST", &format!("/{room}/posts"), Some(body)).await;
    assert_eq!(receipt["origin"], ensured["origin"]);
    assert_eq!(receipt["origin"]["message_id"], original.message);
    assert_eq!(receipt["origin"]["conversation_id"], c.room);
    assert_eq!(receipt["source"]["id"], correction.message);
    assert_eq!(
        receipt["source"]["revision"],
        auth::digest(&correction.text)
    );
    assert_eq!(receipt["source"]["occurred_at"], correction.at);
    assert_eq!(receipt["source"]["conversation_id"], c.room);
    assert_eq!(receipt["return"]["state"], "not_requested");
    let descriptor = admin(&f, "GET", &format!("/api/v1/huddles/{room}"), None).await;
    assert_eq!(descriptor["origin"], ensured["origin"]);
    assert_eq!(count(&f, "huddles"), 1);
    assert_eq!(count(&f, "huddle_posts"), 1);
    assert_eq!(count(&f, "companion_events"), 0);
    let page = ok_tools(
        &f,
        &c,
        "GET",
        &format!("/{room}/messages?installation_id=lead"),
        None,
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["source"]["id"], correction.message);
    let outcome = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(posts(&correction, "outcome")),
    )
    .await;
    assert_eq!(outcome["origin"], ensured["origin"]);
    assert_eq!(outcome["source"]["id"], correction.message);
    assert_eq!(outcome["return"]["state"], "stored");
    assert_eq!(outcome["return"]["call_id"], c.id);
    assert_eq!(outcome["return"]["conversation_id"], c.room);
    assert_eq!(count(&f, "huddles"), 1);
    assert_eq!(count(&f, "huddle_posts"), 2);
}

#[tokio::test]
async fn post_cannot_borrow_another_calls_grant_or_a_peer_installation() {
    let f = fixture();
    let c = call(&f);
    let other = call(&f);
    pair(&f).await;
    let source = source(&f, &c, "Compare the original observations.");
    let room = ensure(&f, &c, &source).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, _) = request(
        &f,
        "POST",
        &format!(
            "/api/v1/companion/calls/{}/tools/huddles/{room}/posts",
            c.id
        ),
        &other.token,
        Some(posts(&source, "message")),
    )
    .await;
    assert!(matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    ));
    let mut forged_installation = posts(&source, "message");
    forged_installation["installation_id"] = json!("peer");
    assert_eq!(
        tools(
            &f,
            &c,
            "POST",
            &format!("/{room}/posts"),
            Some(forged_installation)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(count(&f, "huddle_posts"), 0);
    assert_eq!(count(&f, "companion_events"), 0);
}

#[tokio::test]
async fn another_valid_call_can_inspect_but_cannot_write_to_an_old_return_destination() {
    let f = fixture();
    let original_call = call(&f);
    pair(&f).await;
    let original = source(&f, &original_call, "Compare the original observations.");
    let ensured = ensure(&f, &original_call, &original).await;
    let room = ensured["id"].as_str().unwrap();
    let first = ok_tools(
        &f,
        &original_call,
        "POST",
        &format!("/{room}/posts"),
        Some(posts(&original, "message")),
    )
    .await;
    let later_call = call(&f);
    let later = source(&f, &later_call, "Tell me about the earlier coordination.");
    assert_ne!(later_call.id, original_call.id);
    assert_ne!(later_call.room, original_call.room);
    assert_ne!(later_call.token, original_call.token);
    // The second call owns a valid grant and exact human delegation for this
    // same installation; this is not the cross-token failure covered above.
    let directory = ok_tools(&f, &later_call, "GET", "?installation_id=lead", None).await;
    assert_eq!(directory["huddles"]["items"][0]["id"], room);
    let page = ok_tools(
        &f,
        &later_call,
        "GET",
        &format!("/{room}/messages?installation_id=lead"),
        None,
    )
    .await;
    assert_eq!(page["items"][0]["id"], first["message_id"]);
    let saved = ok_tools(
        &f,
        &later_call,
        "GET",
        &format!(
            "/{room}/posts/{}?installation_id=lead",
            first["request_id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(saved, first);
    let before_descriptor = admin(&f, "GET", &format!("/api/v1/huddles/{room}"), None).await;
    let tables = [
        "messages",
        "huddle_posts",
        "huddles",
        "huddle_ensures",
        "huddle_auto_members",
        "companion_events",
        "companion_voice_batches",
        "companion_actions",
        "input_deliveries",
        "room_agents",
    ];
    let before: Vec<_> = tables.iter().map(|table| count(&f, table)).collect();
    for kind in ["message", "outcome"] {
        let body = posts(&later, kind);
        let (status, value) = tools(
            &f,
            &later_call,
            "POST",
            &format!("/{room}/posts"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind}: {value}");
        assert_eq!(
            tables
                .iter()
                .map(|table| count(&f, table))
                .collect::<Vec<_>>(),
            before,
            "{kind} changed state"
        );
        assert_eq!(
            admin(&f, "GET", &format!("/api/v1/huddles/{room}"), None).await,
            before_descriptor
        );
        let (status, _) = tools(
            &f,
            &later_call,
            "GET",
            &format!(
                "/{room}/posts/{}?installation_id=lead",
                body["request_id"].as_str().unwrap()
            ),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "rejected post must not acquire a receipt"
        );
    }
    assert_eq!(
        count(&f, "companion_events"),
        0,
        "nothing returned to either call"
    );
    assert_eq!(count(&f, "companion_voice_batches"), 0);
    assert_eq!(count(&f, "companion_actions"), 0);
}

#[tokio::test]
async fn stopped_room_remains_stopped_and_saved_post_receipts_remain_readable() {
    let f = fixture();
    let c = call(&f);
    pair(&f).await;
    let source = source(&f, &c, "Compare these observations.");
    let room = ensure(&f, &c, &source).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = posts(&source, "message");
    let saved = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(body.clone()),
    )
    .await;
    let descriptor = admin(&f, "GET", &format!("/api/v1/huddles/{room}"), None).await;
    admin(
        &f,
        "PUT",
        &format!("/api/v1/huddles/{room}/state"),
        Some(json!({"state":"stopped","expected_revision":descriptor["revision"]})),
    )
    .await;
    let ensured = ensure(&f, &c, &source).await;
    assert_eq!(ensured["id"], room);
    assert_eq!(ensured["state"], "stopped");
    let status = ok_tools(
        &f,
        &c,
        "GET",
        &format!(
            "/{room}/posts/{}?installation_id=lead",
            body["request_id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, saved);
    assert!(
        tools(
            &f,
            &c,
            "POST",
            &format!("/{room}/posts"),
            Some(posts(&source, "message"))
        )
        .await
        .0
        .is_client_error()
    );
    assert_eq!(count(&f, "huddles"), 1);
    assert_eq!(count(&f, "huddle_posts"), 1);
}

#[tokio::test]
async fn revoked_membership_is_a_tombstone_not_an_implicit_reenrollment() {
    let f = fixture();
    let c = call(&f);
    pair(&f).await;
    let source = source(&f, &c, "Compare these observations.");
    let request_id = uid();
    let ensured = ok_tools(
        &f,
        &c,
        "POST",
        "/ensure",
        Some(ensure_body(&source, &request_id)),
    )
    .await;
    let room = ensured["id"].as_str().unwrap();
    let body = posts(&source, "message");
    let saved = ok_tools(
        &f,
        &c,
        "POST",
        &format!("/{room}/posts"),
        Some(body.clone()),
    )
    .await;
    admin(
        &f,
        "DELETE",
        &format!("/api/v1/huddles/{room}/participants/coach"),
        None,
    )
    .await;
    let recovered = ok_tools(
        &f,
        &c,
        "GET",
        &format!("/ensure/{request_id}?installation_id=lead"),
        None,
    )
    .await;
    assert_eq!(recovered["id"], room);
    assert_eq!(recovered["membership"], "revoked");
    let repeat = ensure(&f, &c, &source).await;
    assert_eq!(repeat["id"], room);
    assert_eq!(repeat["membership"], "revoked");
    assert_eq!(
        tools(
            &f,
            &c,
            "GET",
            &format!("/{room}/messages?installation_id=lead"),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert!(
        tools(
            &f,
            &c,
            "POST",
            &format!("/{room}/posts"),
            Some(posts(&source, "message"))
        )
        .await
        .0
        .is_client_error()
    );
    let retained: String = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT receipt FROM huddle_posts WHERE message_id=?1",
            [body["request_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&retained).unwrap(), saved);
    let revoked: bool = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT revoked FROM huddle_auto_members WHERE room_id=?1 AND installation_id='lead'",
            [room],
            |r| r.get(0),
        )
        .unwrap();
    assert!(revoked);
    assert_eq!(count(&f, "huddles"), 1);
    assert_eq!(count(&f, "huddle_posts"), 1);
}
