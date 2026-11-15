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
            auth_mode: Default::default(),
            transport: Default::default(),
            agent: Some(Agent {
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
            }),
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
        self.request_in(&self.room, method, path, body).await
    }
    async fn request_in(
        &self,
        room: &str,
        method: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let r = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/api/v1/conversations/{room}/{path}"))
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
        "bad-ack-silent",
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
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

async fn cleanup_started(f: &Fixture) -> u32 {
    for _ in 0..500 {
        if let Some(row) = f.frames().iter().find(|v| v["cleanup"] == "eof") {
            return row["pid"].as_u64().unwrap() as u32;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("The peer did not observe input EOF");
}

fn assert_cleanup_kept_success(f: &Fixture) {
    let room = f.state.conversation(&f.room).unwrap().unwrap();
    let messages = room["messages"].as_array().unwrap();
    let answers: Vec<_> = messages
        .iter()
        .filter(|m| m["role"] == "assistant")
        .collect();
    assert!(!answers.is_empty());
    for answer in answers {
        assert_eq!(answer["status"], "complete");
        assert_eq!(answer["text"], "Completed the work once.");
    }
    assert!(f.deliveries().iter().all(|d| d["state"] == "completed"));
    assert!(f.state.permissions.list(&f.room, "owner").is_empty());
    assert!(!f.home.path().join("late-write").exists());
}

#[tokio::test]
async fn cleanup_releases_capacity_but_keeps_same_room_sequential_and_success_visible() {
    let f = Fixture::new("cleanup-hold", false, false);
    // Leave one global slot, so another room needs this turn's released permit.
    let _other_slots = f
        .state
        .turn_gate
        .clone()
        .acquire_many_owned(3)
        .await
        .unwrap();
    let mut changes = f.state.changes.subscribe();
    f.send("Original").await;
    f.release();
    cleanup_started(&f).await;
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    assert_cleanup_kept_success(&f);
    assert!(f.state.running.lock().unwrap().contains_key(&f.room));
    assert_eq!(f.state.turn_gate.available_permits(), 1);

    f.send("Same room next").await;
    let other = f.state.create("Other room").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        f.request_in(
            &other,
            "POST",
            "messages",
            json!({"text":"Other room now","request_id":uuid::Uuid::new_v4()})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    f.frames_at_least("session/prompt", 2).await;
    let frames = f.frames();
    let prompts: Vec<_> = frames
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[1]["params"]["prompt"][0]["text"], "Other room now");
    std::fs::write(f.home.path().join("cleanup-release"), b"1").unwrap();
    f.idle().await;
    f.frames_at_least("session/prompt", 3).await;
    assert_cleanup_kept_success(&f);
    assert!(f.frames().iter().any(|v| v["cleanup"] == "finished"));
}

#[tokio::test]
async fn cleanup_deadline_and_stop_preserve_accepted_delivery_and_reap_child() {
    for stop in [false, true] {
        let f = Fixture::new("cleanup-hold", false, false);
        f.send("Complete first").await;
        f.release();
        let pid = cleanup_started(&f).await;
        assert_cleanup_kept_success(&f);
        let before = tokio::time::Instant::now();
        if stop {
            assert_eq!(
                f.request("POST", "cancel", json!({})).await.0,
                StatusCode::OK
            );
        }
        f.idle().await;
        assert_cleanup_kept_success(&f);
        assert!(!f.frames().iter().any(|v| v["cleanup"] == "finished"));
        assert!(before.elapsed() < Duration::from_secs(if stop { 2 } else { 5 }));
        #[cfg(target_os = "linux")]
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "Owned child was not reaped"
        );
    }
}

#[tokio::test]
async fn cleanup_drain_failure_preserves_accepted_delivery() {
    let f = Fixture::new("cleanup-error", false, false);
    f.send("Complete first").await;
    f.release();
    cleanup_started(&f).await;
    let before = tokio::time::Instant::now();
    f.idle().await;
    assert!(before.elapsed() < Duration::from_secs(2));
    assert_cleanup_kept_success(&f);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cleanup_early_stdout_close_disposes_live_child_and_descendant_before_reaping() {
    let f = Fixture::new("cleanup-early-eof", false, false);
    f.send("Complete first").await;
    f.release();
    let pid = cleanup_started(&f).await;
    let before = tokio::time::Instant::now();
    f.idle().await;
    assert!(before.elapsed() < Duration::from_secs(2));
    assert_cleanup_kept_success(&f);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    let frames = f.frames();
    let descendant = frames
        .iter()
        .find(|v| v["cleanup"] == "descendant")
        .unwrap()["pid"]
        .as_u64()
        .unwrap();
    // The container's init may not have reaped the orphan yet; a zombie is dead.
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
        assert_eq!(stat.rsplit_once(") ").unwrap().1.chars().next(), Some('Z'));
    }
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
    let mut other = f.state.config.agent.clone().unwrap();
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

#[tokio::test]
async fn received_only_steering_is_unknown_after_terminal_and_is_never_replayed() {
    let f = Fixture::new("received-only", false, false);
    let first = f.send("Read the original source.").await;
    f.frames_at_least("session/prompt", 1).await;
    let second = f
        .send("Only tell me whether it was recorded; do not resend it.")
        .await;
    f.frames_at_least("_session/steering", 1).await;
    f.release();
    f.idle().await;
    let deliveries = f.deliveries();
    assert_eq!(
        deliveries
            .iter()
            .find(|d| d["message_id"] == first)
            .unwrap()["state"],
        "completed"
    );
    assert_eq!(
        deliveries
            .iter()
            .find(|d| d["message_id"] == second)
            .unwrap()["state"],
        "unknown"
    );
    assert_eq!(
        f.frames()
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
}

const SOURCE_META: &str = "com.microsoft.amplifier.source-delivery";
const SOURCE_QUERY: &str = "_amplifier/source_receipts";

fn source_delivery(f: &Fixture, message: &str) -> Value {
    f.deliveries()
        .into_iter()
        .find(|row| row["message_id"] == message)
        .unwrap()
}

fn source_frames(f: &Fixture, method: &str) -> Vec<Value> {
    f.frames()
        .into_iter()
        .filter(|frame| frame["method"] == method)
        .collect()
}

/// Seed only the durable source records normally written by the voice bridge;
/// delivery below still uses the actual HTTP intake and deterministic ACP peer.
/// No realtime provider, media session or model is involved.
fn companion_spoken_source(f: &Fixture, call: &str, text: &str, at: i64) -> (String, String) {
    let message = uuid::Uuid::new_v4().to_string();
    let delegation = uuid::Uuid::new_v4().to_string();
    let endpoint = agent_conversations::auth::digest(
        &serde_json::to_string(&f.state.agent("default").unwrap().unwrap()).unwrap(),
    );
    let db = f.state.db.lock().unwrap();
    db.execute("INSERT OR IGNORE INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'closed',?4,?4,?5)",
        rusqlite::params![call,uuid::Uuid::new_v4().to_string(),f.room,at,endpoint]).unwrap();
    let mut fragments = Vec::new();
    for (index, role, body) in [
        (
            0,
            "assistant",
            "Earlier generated speech, not a new instruction.",
        ),
        (1, "user", text),
    ] {
        let event = json!({"event_id":format!("{delegation}-{index}"),"role":role,"text":body,"start_ms":index*100,"end_ms":index*100+50});
        db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)",
            rusqlite::params![call,format!("{delegation}-{index}"),event.to_string(),at-100+index*100]).unwrap();
        let mut fragment = event;
        fragment["seq"] = json!(db.last_insert_rowid());
        fragments.push(fragment);
    }
    let source = json!({"version":1,"previous_anchor_ms":-1,"offset_ms":150,"fragments":fragments});
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,source,request_text) VALUES(?1,?2,150,'dispatched',?3,?4,?5,?6)",
        rusqlite::params![call,delegation,message,at+1000,source.to_string(),text]).unwrap();
    (message, delegation)
}

async fn send_companion_wrapper(f: &Fixture, message: &str) {
    let (status, result) = f.request("POST", "messages", json!({
        "request_id":message,
        "text":"Generated delegation wrapper, connection catalog and earlier context; not the human request.",
        "target_agent_ids":["default"]
    })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{result}");
}

#[tokio::test]
async fn companion_source_profile_links_original_speech_and_distinct_correction_to_work() {
    let f = Fixture::new("source-many", false, false);
    f.release();
    let call = uuid::Uuid::new_v4().to_string();
    let (first, first_delegation) =
        companion_spoken_source(&f, &call, "Inspect the blue wall.", 100_000);
    send_companion_wrapper(&f, &first).await;
    f.idle().await;
    let first_before = source_delivery(&f, &first);
    let (second, second_delegation) =
        companion_spoken_source(&f, &call, "Correction: inspect the red wall.", 105_000);
    send_companion_wrapper(&f, &second).await;
    f.idle().await;
    let prompts = source_frames(&f, "session/prompt");
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    for (index, message, delegation, human, at) in [
        (
            0,
            &first,
            &first_delegation,
            "Inspect the blue wall.",
            100_000,
        ),
        (
            1,
            &second,
            &second_delegation,
            "Correction: inspect the red wall.",
            105_000,
        ),
    ] {
        let delivery = source_delivery(&f, message);
        let profile = &delivery["source_delivery"];
        let metadata = &prompts[index]["params"]["_meta"][SOURCE_META];
        assert_eq!(metadata["inputId"], delivery["id"]);
        assert_eq!(
            metadata["sources"],
            json!([{"id":message,"revision":agent_conversations::auth::digest(human),"occurredAt":at}])
        );
        assert_eq!(profile["sources"], metadata["sources"]);
        assert_eq!(profile["origin"]["call_id"], call);
        assert_eq!(profile["origin"]["delegation_id"], delegation.as_str());
        assert_eq!(profile["origin"]["conversation_id"], f.room);
        assert_eq!(profile["origin"]["message_id"], message.as_str());
        assert_eq!(profile["origin"]["actor"], "owner");
        assert_eq!(profile["origin"]["agent_id"], "default");
        assert_ne!(
            profile["origin"]["source_revision"],
            profile["origin"]["evidence_revision"]
        );
        let receipt = &profile["receipt"]["receipts"][0];
        assert_eq!(receipt["sourceId"], message.as_str());
        assert_eq!(receipt["completion"], "not_established");
        assert_eq!(receipt["work"][0]["state"], "proposed");
        assert_eq!(receipt["work"][1]["state"], "accepted");
        assert_eq!(receipt["work"][1]["inputId"], delivery["id"]);
        let (status, submission) = f
            .request(
                "GET",
                &format!("deliveries?request_id={message}"),
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(submission["publication"]["state"], "saved");
        assert_eq!(submission["items"][0]["source_delivery"], *profile);
    }
    assert_eq!(
        source_delivery(&f, &first),
        first_before,
        "A later correction rewrote the original receipt"
    );
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 2);
}

#[tokio::test]
async fn companion_source_profile_inspects_original_session_after_lost_reply_without_replay() {
    let mut f = Fixture::new("source-disconnect", false, false);
    let call = uuid::Uuid::new_v4().to_string();
    let (message, delegation) =
        companion_spoken_source(&f, &call, "Inspect without changing anything.", 100_000);
    send_companion_wrapper(&f, &message).await;
    f.idle().await;
    let before = source_delivery(&f, &message);
    assert_eq!(before["state"], "unknown");
    let prompt = source_frames(&f, "session/prompt").pop().unwrap();
    f.state = AppState::open(f.home.path().into()).unwrap();
    f.app = server::router(f.state.clone());
    let (status, result) = f
        .request(
            "POST",
            &format!("deliveries/{}/inspect", before["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let after = source_delivery(&f, &message);
    assert_eq!(after["state"], "unknown");
    assert_eq!(after["source_delivery"]["origin"]["call_id"], call);
    assert_eq!(
        after["source_delivery"]["origin"]["delegation_id"],
        delegation
    );
    assert_eq!(
        after["source_delivery"]["sources"],
        prompt["params"]["_meta"][SOURCE_META]["sources"]
    );
    assert_eq!(
        after["source_delivery"]["receipt"]["receipts"][0]["state"],
        "unknown"
    );
    assert_eq!(
        after["source_delivery"]["receipt"]["coverage"]["missing"],
        "unknown"
    );
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    let loads = source_frames(&f, "session/load");
    assert_eq!(loads.len(), 1);
    assert_eq!(
        loads[0]["params"]["sessionId"],
        prompt["params"]["sessionId"]
    );
    assert!(source_frames(&f, "_session/acknowledge_inputs").is_empty());
}

#[tokio::test]
async fn companion_source_profile_withholds_work_after_original_binding_drift() {
    for change in [
        "speech",
        "transcript",
        "delegation",
        "actor",
        "recipient",
        "wrapper",
        "audience",
    ] {
        let f = Fixture::new("source-many", false, false);
        f.release();
        let call = uuid::Uuid::new_v4().to_string();
        let (message, delegation) =
            companion_spoken_source(&f, &call, "A private original request.", 100_000);
        send_companion_wrapper(&f, &message).await;
        f.idle().await;
        let before = source_delivery(&f, &message);
        assert_eq!(before["source_delivery"]["status"], "observed");
        {
            let db = f.state.db.lock().unwrap();
            match change {
                "speech" => db.execute("UPDATE companion_delegations SET request_text='Changed instruction' WHERE call_id=?1 AND id=?2", rusqlite::params![call,delegation]),
                "transcript" => db.execute("UPDATE companion_events SET data=json_set(data,'$.text','Changed speech') WHERE call_id=?1 AND kind='transcript' AND json_extract(data,'$.role')='user'", [&call]),
                "delegation" => db.execute("UPDATE companion_delegations SET id=?2 WHERE call_id=?1", rusqlite::params![call,uuid::Uuid::new_v4().to_string()]),
                "actor" => db.execute("UPDATE messages SET actor='other' WHERE id=?1", [&message]),
                "recipient" => db.execute("UPDATE companion_calls SET agent_id='substituted-agent' WHERE id=?1", [&call]),
                "wrapper" => db.execute("UPDATE messages SET text='Changed generated transport evidence' WHERE id=?1", [&message]),
                "audience" => db.execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room]),
                _ => unreachable!(),
            }.unwrap();
        }
        let after = source_delivery(&f, &message);
        assert_eq!(
            after["source_delivery"]["status"], "unavailable",
            "{change}: {after}"
        );
        assert!(after["source_delivery"]["receipt"].is_null());
        assert!(
            !after
                .to_string()
                .contains("00000000-0000-4000-8000-000000000002")
        );
        let (status, _) = f
            .request(
                "POST",
                &format!("deliveries/{}/inspect", before["id"].as_str().unwrap()),
                json!({}),
            )
            .await;
        assert!(
            !status.is_success(),
            "{change} unexpectedly inspected a different origin"
        );
        assert_eq!(source_frames(&f, "session/prompt").len(), 1);
        assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 1);
        assert!(source_frames(&f, "session/load").is_empty());
    }
}

#[tokio::test]
async fn companion_source_profile_keeps_legacy_and_unadvertised_paths_unnegotiated() {
    for mode in ["generic", "source-optional"] {
        let f = Fixture::new(mode, false, false);
        f.release();
        let call = uuid::Uuid::new_v4().to_string();
        let (message, _) = companion_spoken_source(&f, &call, "Original speech.", 100_000);
        if mode == "source-optional" {
            f.state.db.lock().unwrap().execute("UPDATE companion_delegations SET source=NULL,request_text=NULL WHERE call_id=?1", [&call]).unwrap();
        }
        send_companion_wrapper(&f, &message).await;
        f.idle().await;
        let prompts = source_frames(&f, "session/prompt");
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0]["params"]["_meta"][SOURCE_META].is_null());
        assert!(source_delivery(&f, &message)["source_delivery"].is_null());
        assert!(source_frames(&f, SOURCE_QUERY).is_empty());
    }
}

fn start_source_inspection(
    f: &Fixture,
    input_id: &str,
    token: &str,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = f.app.clone();
    let path = format!(
        "/api/v1/conversations/{}/deliveries/{input_id}/inspect",
        f.room
    );
    let token = token.to_owned();
    tokio::spawn(async move {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    })
}

#[tokio::test]
async fn source_profile_binds_original_message_and_preserves_generic_acp() {
    for mode in ["source-zero", "generic"] {
        let f = Fixture::new(mode, false, false);
        f.release();
        let first = f.send("A distinct saved source.").await;
        f.idle().await;
        let second = f.send("A separate source, not the earlier history.").await;
        f.idle().await;
        let prompts = source_frames(&f, "session/prompt");
        assert_eq!(prompts.len(), 2);
        assert_eq!(
            prompts[0]["params"]["sessionId"],
            prompts[1]["params"]["sessionId"]
        );
        for (index, message) in [&first, &second].into_iter().enumerate() {
            let delivery = source_delivery(&f, message);
            assert_eq!(delivery["state"], "completed");
            let metadata = &prompts[index]["params"]["_meta"][SOURCE_META];
            if mode == "generic" {
                assert!(metadata.is_null());
                assert!(delivery["source_delivery"].is_null());
                continue;
            }
            let created_at: i64 = f
                .state
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT created_at FROM messages WHERE id=?1",
                    [message],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(metadata["version"], 1);
            assert_eq!(metadata["inputId"], delivery["id"]);
            assert_eq!(metadata["sources"].as_array().unwrap().len(), 1);
            assert_eq!(metadata["sources"][0]["id"], message.as_str());
            assert_eq!(metadata["sources"][0]["occurredAt"], created_at);
            let revision = metadata["sources"][0]["revision"].as_str().unwrap();
            assert_eq!(revision.len(), 64);
            assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
            let saved = &delivery["source_delivery"];
            assert_eq!(saved["version"], 1);
            assert_eq!(saved["input_id"], delivery["id"]);
            assert_eq!(saved["sources"], metadata["sources"]);
            assert_eq!(saved["status"], "observed");
            assert!(saved["observed_at"].as_i64().unwrap() >= created_at);
            let receipt = &saved["receipt"]["receipts"][0];
            assert_eq!(receipt["sourceId"], message.as_str());
            assert_eq!(receipt["revision"], metadata["sources"][0]["revision"]);
            assert_eq!(receipt["occurredAt"], created_at);
            assert_eq!(receipt["completion"], "not_established");
            assert_eq!(receipt["work"], json!([]));
        }
        assert_eq!(
            source_frames(&f, SOURCE_QUERY).len(),
            if mode == "generic" { 0 } else { 2 }
        );
        assert_eq!(source_frames(&f, "session/new").len(), 1);
    }
}

#[tokio::test]
async fn source_profile_keeps_zero_one_many_and_retained_work_separate_from_completion() {
    for (mode, count) in [
        ("source-zero", 0),
        ("source-one", 1),
        ("source-many", 3),
        ("source-work-only", 1),
    ] {
        let f = Fixture::new(mode, false, false);
        f.release();
        let message = f
            .send("Record only the work that was actually created.")
            .await;
        f.idle().await;
        let delivery = source_delivery(&f, &message);
        assert_eq!(delivery["state"], "completed");
        let profile = &delivery["source_delivery"];
        assert_eq!(profile["status"], "observed", "{mode}: {profile}");
        let receipt = &profile["receipt"]["receipts"][0];
        assert_eq!(receipt["completion"], "not_established");
        assert_eq!(receipt["incorporated"], false);
        assert_eq!(receipt["workCoverage"], "retained_session_records");
        assert_eq!(profile["receipt"]["coverage"]["missing"], "unknown");
        let work = receipt["work"].as_array().unwrap();
        assert_eq!(work.len(), count);
        for (index, row) in work.iter().enumerate() {
            assert_eq!(
                row["id"],
                format!("00000000-0000-4000-8000-{:012}", index + 1)
            );
            assert_eq!(row["ownerAgentId"], "11111111-1111-4111-8111-111111111111");
            assert_eq!(row["revision"], index + 1);
            assert_eq!(
                row["state"],
                if index == 0 { "proposed" } else { "accepted" }
            );
        }
        if count == 3 {
            assert_eq!(work[2]["inputId"], "22222222-2222-4222-8222-222222222222");
        }
        if mode == "source-work-only" {
            assert!(receipt["inputId"].is_null());
            assert_eq!(receipt["state"], "unknown");
            assert!(receipt["response"].is_null());
            assert_eq!(work[0]["inputId"], delivery["id"]);
        }
        assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    }
}

#[tokio::test]
async fn source_profile_rejects_wrong_revision_or_time_without_losing_completed_answer() {
    for mode in [
        "source-wrong-revision",
        "source-wrong-time",
        "source-query-error",
    ] {
        let f = Fixture::new(mode, false, false);
        f.release();
        let message = f
            .send("A completed answer is not conditional on receipt availability.")
            .await;
        f.idle().await;
        let delivery = source_delivery(&f, &message);
        assert_eq!(delivery["state"], "completed", "{mode}: {delivery}");
        let profile = &delivery["source_delivery"];
        assert_eq!(profile["status"], "unavailable", "{mode}: {profile}");
        assert!(
            profile["receipt"].is_null(),
            "Mismatched source facts leaked: {profile}"
        );
        let answer: (String, String) = f
            .state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT text,status FROM messages WHERE id=?1",
                [delivery["target_turn"].as_str().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(answer.0, "Completed the work once.");
        assert_eq!(answer.1, "complete");
        assert_eq!(source_frames(&f, "session/prompt").len(), 1);
        assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 1);
    }
}

#[tokio::test]
async fn source_profile_inspects_lost_ordinary_reply_after_restart_without_replay() {
    let mut f = Fixture::new("source-disconnect", false, false);
    let message = f
        .send("An uncertain original must never be submitted again.")
        .await;
    f.idle().await;
    let before = source_delivery(&f, &message);
    assert_eq!(before["state"], "unknown");
    let prompt = source_frames(&f, "session/prompt").pop().unwrap();
    let metadata = prompt["params"]["_meta"][SOURCE_META].clone();
    let original_session = prompt["params"]["sessionId"].clone();
    f.state = AppState::open(f.home.path().into()).unwrap();
    f.app = server::router(f.state.clone());
    let (status, result) = f
        .request(
            "POST",
            &format!("deliveries/{}/inspect", before["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["inspected"], true);
    assert_eq!(result["input_id"], before["id"]);
    let after = source_delivery(&f, &message);
    assert_eq!(
        after["state"], "unknown",
        "Observation must not dismiss uncertainty"
    );
    assert_eq!(result["source_delivery"], after["source_delivery"]);
    assert_eq!(after["source_delivery"]["sources"], metadata["sources"]);
    assert_eq!(after["source_delivery"]["status"], "observed");
    assert_eq!(
        after["source_delivery"]["receipt"]["receipts"][0]["state"],
        "unknown"
    );
    assert_eq!(
        after["source_delivery"]["receipt"]["receipts"][0]["work"][0]["state"],
        "proposed"
    );
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    assert!(source_frames(&f, "_session/acknowledge_inputs").is_empty());
    let loads = source_frames(&f, "session/load");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0]["params"]["sessionId"], original_session);
    let queries = source_frames(&f, SOURCE_QUERY);
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0]["params"]["sessionId"], original_session);
    assert_eq!(queries[0]["params"]["sources"], metadata["sources"]);
}

#[tokio::test]
async fn source_profile_refuses_active_inspection_without_an_extra_connection() {
    let f = Fixture::new("source-zero", false, false);
    let message = f
        .send("Stay active until explicitly released by the fixture.")
        .await;
    f.frames_at_least("session/prompt", 1).await;
    let delivery = source_delivery(&f, &message);
    let (status, _) = f
        .request(
            "POST",
            &format!("deliveries/{}/inspect", delivery["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(source_frames(&f, "initialize").len(), 1);
    assert!(source_frames(&f, SOURCE_QUERY).is_empty());
    f.release();
    f.idle().await;
    assert_eq!(source_delivery(&f, &message)["state"], "completed");
}

#[tokio::test]
async fn source_profile_inspection_requires_auth_and_never_replaces_a_failed_original_load() {
    let f = Fixture::new("source-one", false, false);
    f.release();
    let message = f.send("Keep the original completed session.").await;
    f.idle().await;
    let delivery = source_delivery(&f, &message);
    let path = format!("deliveries/{}/inspect", delivery["id"].as_str().unwrap());
    let denied = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/conversations/{}/{path}", f.room))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!denied.status().is_success());
    let browser = "source-inspection-browser";
    f.state.db.lock().unwrap().execute(
        "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'owner','required-csrf',?2)",
        rusqlite::params![agent_conversations::auth::digest(browser), agent_conversations::state::now_ms() + 60_000],
    ).unwrap();
    let missing_csrf = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/conversations/{}/{path}", f.room))
                .header(
                    "cookie",
                    format!(
                        "{}={browser}",
                        agent_conversations::auth::cookie_name(&f.state)
                    ),
                )
                .header("host", "example.test")
                .header("origin", "https://example.test")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);
    assert_eq!(source_frames(&f, "initialize").len(), 1);
    assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 1);
    let original_session = source_frames(&f, "session/prompt")[0]["params"]["sessionId"].clone();
    std::fs::write(f.home.path().join("reject-load"), b"1").unwrap();
    let (status, _) = f.request("POST", &path, json!({})).await;
    assert!(!status.is_success());
    assert_eq!(source_delivery(&f, &message)["state"], "completed");
    let loads = source_frames(&f, "session/load");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0]["params"]["sessionId"], original_session);
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 1);
    let retained: String = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT session_id FROM room_agents WHERE conversation_id=?1 AND agent_id='default'",
            [&f.room],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, original_session.as_str().unwrap());
}

#[tokio::test]
async fn source_profile_hides_cached_work_and_refuses_changed_audience_endpoint_or_provider() {
    for change in ["audience", "endpoint", "provider"] {
        let mut f = Fixture::new("source-one", false, false);
        f.release();
        let message = f.send("Owner-private work reference.").await;
        f.idle().await;
        let before = source_delivery(&f, &message);
        assert_eq!(
            before["source_delivery"]["receipt"]["receipts"][0]["work"][0]["id"],
            "00000000-0000-4000-8000-000000000001"
        );
        if change == "audience" {
            let (status, body) = f
                .request(
                    "POST",
                    "members",
                    json!({"username":"other","share_history":true}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        } else if change == "endpoint" {
            f.state.db.lock().unwrap().execute(
                "UPDATE room_agents SET endpoint='a-different-endpoint' WHERE conversation_id=?1",
                [&f.room],
            ).unwrap();
        } else {
            let mut config = Config::load(f.home.path()).unwrap();
            config.id = uuid::Uuid::new_v4().to_string();
            config.save(f.home.path()).unwrap();
            f.state = AppState::open(f.home.path().into()).unwrap();
            f.app = server::router(f.state.clone());
        }
        let after = source_delivery(&f, &message);
        assert!(
            !after
                .to_string()
                .contains("00000000-0000-4000-8000-000000000001"),
            "Cached work escaped its original {change}: {after}"
        );
        assert!(after["source_delivery"]["receipt"].is_null());
        let (status, _) = f
            .request(
                "POST",
                &format!("deliveries/{}/inspect", before["id"].as_str().unwrap()),
                json!({}),
            )
            .await;
        assert!(
            !status.is_success(),
            "Changed {change} must not load a replacement session"
        );
        assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 1);
        assert_eq!(source_frames(&f, "session/prompt").len(), 1);
        assert_eq!(source_frames(&f, "session/new").len(), 1);
    }
}

#[tokio::test]
async fn source_profile_inspection_accepts_observe_only_room_member_without_chat_authority() {
    let mut f = Fixture::new("source-one", false, false);
    let observer = "source-observe-only-token";
    {
        let db = f.state.db.lock().unwrap();
        db.execute(
            "INSERT INTO tokens(hash,id,scopes,expires_at) VALUES(?1,'source-observer','[\"observe\"]',?2)",
            rusqlite::params![agent_conversations::auth::digest(observer), agent_conversations::state::now_ms() + 60_000],
        ).unwrap();
        db.execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room])
            .unwrap();
        db.execute(
            "INSERT INTO room_members VALUES(?1,'token:source-observer','member',1)",
            [&f.room],
        )
        .unwrap();
    }
    f.release();
    let message = f
        .send("Observe already recorded work without starting anything.")
        .await;
    f.idle().await;
    let before = source_delivery(&f, &message);
    f.token = observer.into();
    let (status, result) = f
        .request(
            "POST",
            &format!("deliveries/{}/inspect", before["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["inspected"], true);
    assert_eq!(result["source_delivery"]["status"], "observed");
    assert_eq!(source_delivery(&f, &message)["state"], "completed");
    let (chat_status, _) = f
        .request(
            "POST",
            "messages",
            json!({"text":"Must not be admitted","request_id":uuid::Uuid::new_v4().to_string()}),
        )
        .await;
    assert_eq!(chat_status, StatusCode::FORBIDDEN);
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    assert_eq!(source_frames(&f, "session/load").len(), 1);
    assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 2);
    assert!(source_frames(&f, "_session/acknowledge_inputs").is_empty());
}

#[tokio::test]
async fn source_profile_loss_cannot_downgrade_a_negotiated_steering_fallback() {
    let f = Fixture::new("source-idle-loss", false, false);
    f.send("Original ongoing turn.").await;
    f.frames_at_least("session/prompt", 1).await;
    let message = f
        .send("Keep this input's negotiated provenance through fallback.")
        .await;
    f.frames_at_least("_session/steering", 1).await;
    f.release();
    f.idle().await;
    assert_eq!(source_frames(&f, "initialize").len(), 2);
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    assert_eq!(source_frames(&f, "_session/steering").len(), 1);
    let delivery = source_delivery(&f, &message);
    assert_ne!(delivery["state"], "completed");
    assert_eq!(delivery["source_delivery"]["input_id"], delivery["id"]);
    let steering = source_frames(&f, "_session/steering");
    assert_eq!(
        delivery["source_delivery"]["sources"],
        steering[0]["params"]["_meta"][SOURCE_META]["sources"]
    );
}

#[tokio::test]
async fn source_profile_preserves_metadata_through_explicit_steering_fallback() {
    let f = Fixture::new("source-idle", false, false);
    f.send("Original ongoing turn.").await;
    f.frames_at_least("session/prompt", 1).await;
    let message = f
        .send("This steering input needs the next ordinary prompt.")
        .await;
    f.frames_at_least("session/prompt", 2).await;
    f.release();
    f.idle().await;
    let steering = source_frames(&f, "_session/steering");
    let prompts = source_frames(&f, "session/prompt");
    assert_eq!(steering.len(), 1);
    assert_eq!(prompts.len(), 2);
    let metadata = &steering[0]["params"]["_meta"][SOURCE_META];
    assert_eq!(metadata, &prompts[1]["params"]["_meta"][SOURCE_META]);
    assert_eq!(
        metadata["inputId"],
        steering[0]["params"]["_meta"]["steering"]["inputId"]
    );
    assert_eq!(metadata["sources"][0]["id"], message);
    assert_eq!(
        steering[0]["params"]["sessionId"],
        prompts[1]["params"]["sessionId"]
    );
    assert_eq!(source_delivery(&f, &message)["state"], "completed");
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert!(source_frames(&f, "session/cancel").is_empty());
}

#[tokio::test]
async fn source_inspection_cancel_cancels_queued_input_and_reaps_before_idle() {
    let f = Fixture::new("source-query-hold", false, false);
    f.release();
    let original = f.send("The completed original remains completed.").await;
    f.idle().await;
    let delivery = source_delivery(&f, &original);
    assert_eq!(delivery["state"], "completed");
    assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 1);
    std::fs::write(f.home.path().join("hold-source-query"), b"1").unwrap();
    let inspection = start_source_inspection(&f, delivery["id"].as_str().unwrap(), &f.token);
    f.frames_at_least(SOURCE_QUERY, 2).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Some(frame) = f.frames().iter().find(|v| v["inspection"] == "query-held") {
            break frame["pid"].as_u64().unwrap() as u32;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Inspection did not enter its deterministic query gate"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(pid > 0);
    #[cfg(target_os = "linux")]
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    let queued = f
        .send("This newer input is queued, then cancelled with the inspection.")
        .await;
    assert_eq!(source_delivery(&f, &queued)["state"], "received");
    assert_eq!(
        f.request("POST", "cancel", json!({})).await.0,
        StatusCode::OK
    );
    let (status, result) = tokio::time::timeout(Duration::from_secs(5), inspection)
        .await
        .expect("Cancelled inspection did not return")
        .expect("Inspection request panicked");
    assert!(
        !status.is_success(),
        "Cancelled inspection was reported successful: {result}"
    );
    f.idle().await;
    // Inspect the OS at the first observed idle state, rather than polling for
    // disposal afterward and concealing an early room-release boundary.
    #[cfg(target_os = "linux")]
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "Inspection child {pid} remained after room idle"
    );
    assert_eq!(source_delivery(&f, &original)["state"], "completed");
    assert_eq!(source_delivery(&f, &queued)["state"], "cancelled");
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/load").len(), 1);
    assert_eq!(source_frames(&f, SOURCE_QUERY).len(), 2);
    assert!(source_frames(&f, "_session/acknowledge_inputs").is_empty());
}

#[tokio::test]
async fn source_inspection_rechecks_revoked_observe_token_after_capacity_wait() {
    let f = Fixture::new("source-one", false, false);
    let observer = "capacity-wait-observe-token";
    {
        let db = f.state.db.lock().unwrap();
        db.execute(
            "INSERT INTO tokens(hash,id,scopes,expires_at) VALUES(?1,'waiting-observer','[\"observe\"]',?2)",
            rusqlite::params![agent_conversations::auth::digest(observer), agent_conversations::state::now_ms() + 60_000],
        ).unwrap();
        db.execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room])
            .unwrap();
        db.execute(
            "INSERT INTO room_members VALUES(?1,'token:waiting-observer','member',1)",
            [&f.room],
        )
        .unwrap();
    }
    f.release();
    let original = f
        .send("A saved source whose inspection must remain authorized.")
        .await;
    f.idle().await;
    let delivery = source_delivery(&f, &original);
    assert_eq!(delivery["state"], "completed");
    let baseline =
        ["initialize", "session/load", SOURCE_QUERY].map(|method| source_frames(&f, method).len());
    let permits = f
        .state
        .turn_gate
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    assert_eq!(f.state.turn_gate.available_permits(), 0);
    let inspection = start_source_inspection(&f, delivery["id"].as_str().unwrap(), observer);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if f.state.running.lock().unwrap().contains_key(&f.room) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Inspection never claimed the room before waiting for capacity"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        ["initialize", "session/load", SOURCE_QUERY].map(|method| source_frames(&f, method).len()),
        baseline
    );
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM tokens WHERE id='waiting-observer'", [])
            .unwrap(),
        1
    );
    drop(permits);
    let (status, result) = tokio::time::timeout(Duration::from_secs(5), inspection)
        .await
        .expect("Revoked inspection did not settle after capacity became available")
        .expect("Inspection request panicked");
    assert!(
        !status.is_success(),
        "Revoked credential retained inspection authority: {result}"
    );
    f.idle().await;
    assert_eq!(
        ["initialize", "session/load", SOURCE_QUERY].map(|method| source_frames(&f, method).len()),
        baseline,
        "Revoked inspection performed remote setup or query before refusing"
    );
    assert_eq!(source_frames(&f, "session/new").len(), 1);
    assert_eq!(source_frames(&f, "session/prompt").len(), 1);
    assert_eq!(source_delivery(&f, &original)["state"], "completed");
}
