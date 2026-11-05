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
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tower::ServiceExt;

struct Fixture {
    home: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
    token: String,
    room: String,
}
impl Fixture {
    fn new(mode: &str, approval: bool, fragment: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut env = BTreeMap::from([
            ("STEERING_MODE".into(), mode.into()),
            (
                "FIXTURE_LOG".into(),
                home.path().join("frames.jsonl").display().to_string(),
            ),
            (
                "STEERING_RELEASE".into(),
                home.path().join("release").display().to_string(),
            ),
        ]);
        if approval {
            env.insert("STEERING_APPROVAL".into(), "1".into());
        }
        if fragment {
            env.insert("STEERING_FRAGMENT".into(), "1".into());
        }
        let config = Config {
            id: uuid::Uuid::new_v4().to_string(),
            label: "Fixture".into(),
            owner: "owner".into(),
            allowed_users: vec!["other".into()],
            embed_origins: vec![],
            bind: "127.0.0.1".into(),
            port: 0,
            public_url: "https://example.test".into(),
            pam_service: "agent-conversations".into(),
            tls_cert: home.path().join("cert"),
            tls_key: home.path().join("key"),
            tls_ca: None,
            import_source: "fixture".into(),
            import_surface: "fixture".into(),
            agent: Agent {
                label: "Fixture".into(),
                command: "python3".into(),
                args: vec![format!(
                    "{}/tests/steering_agent.py",
                    env!("CARGO_MANIFEST_DIR")
                )],
                cwd: home.path().display().to_string(),
                session_cwd: None,
                env,
                mcp_servers: vec![],
                timeout_seconds: 30,
                client: Default::default(),
            },
        };
        config.save(home.path()).unwrap();
        let state = AppState::open(home.path().into()).unwrap();
        let app = server::router(state.clone());
        let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
        let room = state.create("Steering").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            home,
            state,
            app,
            token,
            room,
        }
    }
    async fn request(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/v1/conversations/{}/{path}", self.room))
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status();
        let bytes = to_bytes(r.into_body(), 16 * 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn send(&self, text: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            self.request("POST", "messages", json!({"text":text,"request_id":id}))
                .await
                .0,
            StatusCode::ACCEPTED
        );
        id
    }
    fn frames(&self) -> Vec<Value> {
        std::fs::read_to_string(self.home.path().join("frames.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
    async fn frames_at_least(&self, method: &str, n: usize) {
        for _ in 0..500 {
            if self
                .frames()
                .iter()
                .filter(|v| v["method"] == method)
                .count()
                >= n
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "Missing {method} frames: {:?}; room {:?}",
            self.frames(),
            self.state.conversation(&self.room)
        );
    }
    fn release(&self) {
        std::fs::write(self.home.path().join("release"), b"1").unwrap();
    }
    async fn idle(&self) {
        for _ in 0..500 {
            if !self.state.running.lock().unwrap().contains_key(&self.room) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "Worker did not settle: {:?}",
            self.state.conversation(&self.room)
        );
    }
    fn deliveries(&self) -> Vec<Value> {
        agent_conversations::delivery::list(&self.state, &self.room).unwrap()
    }
}

#[tokio::test]
async fn burst_is_durable_ordered_deduplicated_and_keeps_approval_and_effect() {
    let f = Fixture::new("native", true, true);
    f.send("Original task").await;
    f.frames_at_least("session/prompt", 1).await;
    let approval_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let permission = loop {
        assert!(
            tokio::time::Instant::now() < approval_deadline,
            "Approval did not appear"
        );
        let rows = f.state.permissions.list(&f.room, "owner");
        if let Some(row) = rows.first() {
            break row["id"].as_str().unwrap().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut ids = vec![];
    for i in 0..8 {
        ids.push(f.send(&format!("Correction {i}")).await);
    }
    assert_eq!(
        f.request(
            "POST",
            "messages",
            json!({"text":"Correction 2","request_id":ids[2]})
        )
        .await
        .1["duplicate"],
        true
    );
    f.frames_at_least("_session/steering", 8).await;
    let frames = f.frames();
    let updates: Vec<_> = frames
        .iter()
        .filter(|v| v["method"] == "_session/steering")
        .collect();
    assert_eq!(
        updates
            .iter()
            .map(
                |v| v["params"]["_meta"]["com.microsoft.amplifier.conversations"]["messageId"]
                    .as_str()
                    .unwrap()
            )
            .collect::<Vec<_>>(),
        ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(
        f.state.permissions.list(&f.room, "owner")[0]["id"],
        permission
    );
    assert_eq!(
        f.request(
            "POST",
            &format!("permissions/{permission}"),
            json!({"option_id":"yes"})
        )
        .await
        .0,
        StatusCode::OK
    );
    f.send("After approval, keep the completed effect").await;
    f.frames_at_least("_session/steering", 9).await;
    f.release();
    f.idle().await;
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["effect"] == "write-once")
            .count(),
        1
    );
    assert!(!f.frames().iter().any(|v| v["method"] == "session/cancel"));
    assert_eq!(
        f.deliveries()
            .iter()
            .filter(|d| d["state"] == "incorporated")
            .count(),
        9
    );
    assert_eq!(
        f.deliveries()
            .iter()
            .filter(|d| d["state"] == "completed")
            .count(),
        1
    );
}

#[tokio::test]
async fn malformed_acknowledgments_stay_unknown_without_replaying_or_changing_approval() {
    for mode in [
        "bad-ack-type",
        "bad-ack-id",
        "bad-ack-error",
        "bad-ack-conflict",
    ] {
        let f = Fixture::new(mode, true, false);
        f.send("Original with approval").await;
        f.frames_at_least("session/prompt", 1).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let permission = loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "Missing approval: {mode}"
            );
            if let Some(row) = f.state.permissions.list(&f.room, "owner").first() {
                break row["id"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let source = f.send("Uncertain correction").await;
        f.frames_at_least("_session/steering", 1).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let state = loop {
            let rows = f.deliveries();
            let row = rows.iter().find(|d| d["message_id"] == source).unwrap();
            if row["state"] != "submitted" {
                break row["state"].clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Missing acknowledgment: {mode}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(state, "unknown", "{mode}");
        assert_eq!(
            f.state.permissions.list(&f.room, "owner")[0]["id"],
            permission
        );
        f.send("Keep this queued").await;
        assert_eq!(
            f.request(
                "POST",
                &format!("permissions/{permission}"),
                json!({"option_id":"yes"})
            )
            .await
            .0,
            StatusCode::OK
        );
        f.release();
        f.idle().await;
        let rows = f.deliveries();
        assert_eq!(
            rows.iter().find(|d| d["message_id"] == source).unwrap()["state"],
            "unknown"
        );
        assert_eq!(rows[0]["state"], "received");
        let frames = f.frames();
        assert_eq!(
            frames
                .iter()
                .filter(|v| v["method"] == "session/prompt")
                .count(),
            1
        );
        assert_eq!(
            frames
                .iter()
                .filter(|v| v["method"] == "_session/steering")
                .count(),
            1
        );
        assert_eq!(
            frames
                .iter()
                .filter(|v| v["effect"] == "write-once")
                .count(),
            1
        );
        assert!(!frames.iter().any(|v| v["method"] == "session/cancel"));
    }
}

#[tokio::test]
async fn ordinary_acp_queues_without_cancelling_or_overlapping() {
    let f = Fixture::new("generic", false, false);
    f.send("Original").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("Next one").await;
    f.send("Next two").await;
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    f.release();
    f.idle().await;
    let frames = f.frames();
    let prompts: Vec<_> = frames
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 3);
    assert_eq!(
        prompts
            .iter()
            .map(|p| p["params"]["prompt"][0]["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["Original", "Next one", "Next two"]
    );
    assert!(
        !frames
            .iter()
            .any(|v| v["method"] == "session/cancel" || v["method"] == "_session/steering")
    );
    assert!(f.deliveries().iter().all(|d| d["state"] == "completed"));
}

#[tokio::test]
async fn terminal_before_steer_ack_defers_exactly_once_with_same_session() {
    let f = Fixture::new("idle", false, false);
    f.send("Original").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("Idle-race update").await;
    f.frames_at_least("session/prompt", 2).await;
    f.release();
    f.idle().await;
    let frames = f.frames();
    assert_eq!(
        frames
            .iter()
            .filter(|v| v["method"] == "_session/steering")
            .count(),
        1
    );
    let prompts: Vec<_> = frames
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    assert!(f.deliveries().iter().all(|d| d["state"] == "completed"));
}

#[tokio::test]
async fn uncertain_disconnect_survives_restart_and_explicit_resolution_never_replays() {
    let mut f = Fixture::new("disconnect", false, false);
    f.state
        .db
        .lock()
        .unwrap()
        .execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room])
        .unwrap();
    // The room has two people, so continuation uses the public-context path.
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO room_members VALUES(?1,'other','member',1)",
            [&f.room],
        )
        .unwrap();
    f.send("ORIGINAL-UNCERTAIN-INTENT").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("CORRECTION-UNCERTAIN-INTENT").await;
    f.idle().await;
    let before = f.deliveries();
    assert_eq!(before.iter().filter(|d| d["state"] == "unknown").count(), 2);
    let sid = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT session_id FROM room_agents WHERE conversation_id=?1",
            [&f.room],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    f.state = AppState::open(f.home.path().into()).unwrap();
    f.app = server::router(f.state.clone());
    f.send("An unrelated new question").await;
    f.idle().await;
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    f.release();
    for d in before {
        assert_eq!(
            f.request(
                "POST",
                &format!("deliveries/{}/resolve", d["id"].as_str().unwrap()),
                json!({"disposition":"proceed_without_replay"})
            )
            .await
            .0,
            StatusCode::OK
        );
        f.idle().await;
    }
    f.idle().await;
    let frames = f.frames();
    let prompts: Vec<_> = frames
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[1]["params"]["sessionId"], sid);
    let text = prompts[1]["params"]["prompt"].to_string();
    assert!(text.contains("An unrelated new question"));
    assert!(!text.contains("UNCERTAIN-INTENT"));
    assert_eq!(
        frames
            .iter()
            .filter(|v| v["method"] == "_session/steering")
            .count(),
        1
    );
    assert_eq!(
        frames
            .iter()
            .filter(|v| v["method"] == "_session/acknowledge_inputs")
            .count(),
        1
    );
}

#[tokio::test]
async fn queue_capacity_and_explicit_stop_never_drop_accepted_source_messages() {
    let f = Fixture::new("generic", false, false);
    f.send("Original").await;
    f.frames_at_least("session/prompt", 1).await;
    for i in 0..31 {
        f.send(&format!("Queued {i}")).await;
    }
    assert_eq!(
        f.request(
            "POST",
            "messages",
            json!({"text":"Excess","request_id":uuid::Uuid::new_v4()})
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        f.request("POST", "cancel", json!({})).await.0,
        StatusCode::OK
    );
    f.idle().await;
    assert_eq!(f.deliveries().len(), 32);
    assert_eq!(
        f.deliveries()
            .iter()
            .filter(|d| d["state"] == "cancelled")
            .count(),
        31
    );
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/cancel")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_failed_load_never_replaces_the_original_binding_or_replays_history() {
    let f = Fixture::new("generic", false, false);
    f.release();
    f.send("Completed once").await;
    f.idle().await;
    let first_sid = f
        .frames()
        .into_iter()
        .find(|v| v["method"] == "session/prompt")
        .unwrap()["params"]["sessionId"]
        .clone();
    std::fs::write(f.home.path().join("reject-load"), b"1").unwrap();
    f.send("A separate new request").await;
    f.idle().await;
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/new")
            .count(),
        1
    );
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    let saved: String = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT session_id FROM room_agents WHERE conversation_id=?1",
            [&f.room],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(saved, first_sid.as_str().unwrap());
    assert_eq!(f.deliveries()[0]["state"], "notAccepted");
}

#[tokio::test]
async fn an_incorporated_receipt_before_rpc_ack_is_never_downgraded() {
    let f = Fixture::new("preack", false, false);
    f.send("Original").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("Correction").await;
    f.frames_at_least("_session/steering", 1).await;
    for _ in 0..100 {
        if f.deliveries()[0]["state"] == "incorporated" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.deliveries()[0]["state"], "incorporated");
    f.release();
    f.idle().await;
    assert_eq!(f.deliveries()[0]["state"], "incorporated");
}

#[tokio::test]
async fn ambient_idle_race_reuses_only_the_explicitly_restored_admission() {
    let f = Fixture::new("attention_idle", false, false);
    f.state
        .db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO room_members VALUES(?1,'other','member',1)",
            [&f.room],
        )
        .unwrap();
    assert_eq!(f.request("POST","messages",json!({"text":"Direct task","request_id":uuid::Uuid::new_v4(),"target_agent_ids":["default"]})).await.0,StatusCode::ACCEPTED);
    f.frames_at_least("session/prompt", 1).await;
    f.send("A general room update").await;
    f.frames_at_least("session/prompt", 2).await;
    f.release();
    f.idle().await;
    let frames = f.frames();
    assert_eq!(
        frames
            .iter()
            .filter(|v| v["method"] == "_amplifier/attention")
            .count(),
        1
    );
    let steered = frames
        .iter()
        .find(|v| v["method"] == "_session/steering")
        .unwrap();
    let next = frames
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .nth(1)
        .unwrap();
    assert_eq!(
        next["params"]["_meta"]["com.microsoft.amplifier.attention"],
        steered["params"]["_meta"]["com.microsoft.amplifier.attention"]
    );
    assert!(f.deliveries().iter().all(|d| d["state"] == "completed"));
}

#[tokio::test]
async fn each_agent_receives_only_its_ordered_room_inputs() {
    let f = Fixture::new("native", false, false);
    let mut other = f.state.config.agent.clone();
    other.label = "Reviewer".into();
    other.env.insert("STEERING_MODE".into(), "generic".into());
    other.env.insert(
        "FIXTURE_LOG".into(),
        f.home.path().join("reviewer.jsonl").display().to_string(),
    );
    {
        let db = f.state.db.lock().unwrap();
        db.execute(
            "INSERT INTO agent_endpoints VALUES('reviewer',?1,1)",
            [serde_json::to_string(&other).unwrap()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO room_agents(conversation_id,agent_id) VALUES(?1,'reviewer')",
            [&f.room],
        )
        .unwrap();
    }
    f.send("Both, begin").await;
    f.frames_at_least("session/prompt", 1).await;
    assert_eq!(f.request("POST","messages",json!({"text":"Reviewer only","request_id":uuid::Uuid::new_v4(),"target_agent_ids":["reviewer"]})).await.0,StatusCode::ACCEPTED);
    f.send("Both, a correction").await;
    f.frames_at_least("_session/steering", 1).await;
    f.release();
    f.idle().await;
    let default = f.frames();
    assert_eq!(
        default
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    let correction = default
        .iter()
        .find(|v| v["method"] == "_session/steering")
        .unwrap()
        .to_string();
    assert!(correction.contains("Both, a correction"));
    assert!(!correction.contains("Reviewer only"));
    let other: Vec<Value> = std::fs::read_to_string(f.home.path().join("reviewer.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let prompts: Vec<_> = other
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 3);
    assert!(!prompts[0].to_string().contains("Reviewer only"));
    assert!(prompts[1].to_string().contains("Reviewer only"));
    assert_ne!(
        prompts[0]["params"]["sessionId"],
        default
            .iter()
            .find(|v| v["method"] == "session/prompt")
            .unwrap()["params"]["sessionId"]
    );
    assert_eq!(f.deliveries().len(), 5);
}

#[tokio::test]
async fn delayed_receipts_and_terminal_never_disguise_a_still_remote_queued_update() {
    let f = Fixture::new("regress-queued", false, false);
    f.send("Original").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("Still queued remotely").await;
    f.frames_at_least("_session/steering", 1).await;
    f.release();
    f.idle().await;
    assert_eq!(f.deliveries()[0]["state"], "unknown");
    assert_eq!(f.deliveries()[0]["receipt"]["state"], "remoteQueued");
    let unresolved = f.deliveries()[0]["id"].clone();
    // Completed history from other work cannot hide the recoverable source.
    {
        let db = f.state.db.lock().unwrap();
        for i in 0..300 {
            let id = format!("history-{i}");
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','History','complete',1,1,'owner')",rusqlite::params![id,f.room]).unwrap();
            db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?1,?1,?2,'default','direct','completed',1)",rusqlite::params![id,f.room]).unwrap();
        }
    }
    let listed = f.deliveries();
    assert_eq!(listed.len(), 257);
    assert!(
        listed
            .iter()
            .any(|d| d["id"] == unresolved && d["source_sequence"].is_number())
    );

    f.send("A newer question").await;
    f.idle().await;
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
}

#[tokio::test]
async fn unknown_only_recovery_acknowledges_the_backend_without_a_new_prompt() {
    let f = Fixture::new("disconnect", false, false);
    f.send("Original").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("Uncertain correction").await;
    f.idle().await;
    let rows = f.deliveries();
    assert_eq!(rows.len(), 2);
    let rejected = f.home.path().join("reject-ack");
    std::fs::write(&rejected, b"1").unwrap();
    assert_eq!(
        f.request(
            "POST",
            &format!("deliveries/{}/resolve", rows[0]["id"].as_str().unwrap()),
            json!({"disposition":"proceed_without_replay"})
        )
        .await
        .0,
        StatusCode::OK
    );
    f.idle().await;
    assert_eq!(f.deliveries()[0]["state"], "unknown");
    std::fs::remove_file(rejected).unwrap();
    for row in rows {
        assert_eq!(
            f.request(
                "POST",
                &format!("deliveries/{}/resolve", row["id"].as_str().unwrap()),
                json!({"disposition":"proceed_without_replay"})
            )
            .await
            .0,
            StatusCode::OK
        );
        f.idle().await;
    }
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "_session/acknowledge_inputs")
            .count(),
        2
    );
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    assert!(f.deliveries().iter().all(|d| d["state"] == "dismissed"));
    assert_eq!(
        f.state.conversation(&f.room).unwrap().unwrap()["status"],
        "idle"
    );
}

#[tokio::test]
async fn a_revoked_source_credential_cannot_start_queued_work() {
    let f = Fixture::new("generic", false, false);
    f.send("Already running").await;
    f.frames_at_least("session/prompt", 1).await;
    f.send("Queued before access revocation").await;
    f.state
        .db
        .lock()
        .unwrap()
        .execute("DELETE FROM tokens WHERE id='local-control'", [])
        .unwrap();
    f.release();
    f.idle().await;
    assert_eq!(f.deliveries()[0]["state"], "notAccepted");
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    assert!(!f.frames().iter().any(|v| v["method"] == "session/cancel"));
}
