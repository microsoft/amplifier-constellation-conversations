//! Source-bound lifecycle and independently awaited result regressions.
use super::*;
use crate::config::{Agent, Config};

struct Fixture {
    _home: tempfile::TempDir,
    s: Arc<AppState>,
    call: String,
    room: String,
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Pending receipt fixture".into(),
        owner: "owner".into(),
        allowed_users: vec![],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://fixture.test".into(),
        pam_service: "fixture".into(),
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
    }
    .save(home.path())
    .unwrap();
    let s = AppState::open(home.path().into()).unwrap();
    let room = s.create("Private call").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call = uuid::Uuid::new_v4().to_string();
    let endpoint =
        auth::digest(&serde_json::to_string(&s.agent("default").unwrap().unwrap()).unwrap());
    {
        let db = s.db.lock().unwrap();
        db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)",params![call,uuid::Uuid::new_v4().to_string(),room,now_ms(),endpoint]).unwrap();
        db.execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at,playback) VALUES(?1,'browser','session',0,0,0,0,?2,?3)",params![call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
    }
    Fixture {
        _home: home,
        s,
        call,
        room,
    }
}

fn request(f: &Fixture, did: &str, state: &str) {
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','How wide is the shelf?','sent',?3,?3,'owner')",params![did,f.room,now_ms()]).unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,request_text,created_at) VALUES(?1,?2,100,'dispatched',?2,'How wide is the shelf?',?3)",params![f.call,did,now_ms()]).unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?1,?1,?2,'default','direct',?3,?4)",params![did,f.room,state,now_ms()]).unwrap();
}
fn completed(f: &Fixture, did: &str, answer: &str) -> String {
    let target = format!("answer-{did}");
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,'Coach','default')",params![target,f.room,answer,now_ms()]).unwrap();
    db.execute(
        "UPDATE input_deliveries SET state='completed',target_turn=?2 WHERE id=?1",
        params![did, target],
    )
    .unwrap();
    target
}
fn batches(f: &Fixture) -> Vec<(String, String, Value)> {
    let db = f.s.db.lock().unwrap();
    let mut q=db.prepare("SELECT semantic_key,state,updates FROM companion_voice_batches WHERE call_id=?1 ORDER BY rowid").unwrap();
    q.query_map([&f.call], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
    })
    .unwrap()
    .map(|r| {
        let (k, s, u) = r.unwrap();
        (k, s, serde_json::from_str(&u).unwrap())
    })
    .collect()
}
#[test]
fn one_quiet_pending_receipt_survives_polling_without_more_work() {
    let f = fixture();
    request(&f, "shelf", "received");
    for _ in 0..100 {
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
    }
    let rows = batches(&f);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2[0]["channel"], "thinking");
    assert_eq!(rows[0].2[0]["delegation_id"], "shelf");
    let text = rows[0].2[0]["content"].as_str().unwrap();
    assert!(text.contains("Request shelf is saved"));
    assert!(text.contains("How wide is the shelf?"));
    assert!(text.contains("does not establish external execution or progress"));
    // Provider-facing identity is stable, and no additional task is admitted.
    let batch = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    assert_eq!(
        batch["commands"][0]["event_id"],
        pending_context_key("shelf")
    );
    crate::companion_delivery::sent(&f.s, &f.call, batch["batch_id"].as_str().unwrap()).unwrap();
    for _ in 0..100 {
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
    }
    assert_eq!(batches(&f).len(), 1);
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM input_deliveries", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn terminal_arriving_under_user_floor_retires_unsent_pending_but_preserves_answer() {
    let f = fixture();
    request(&f, "shelf", "received");
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_surfaces SET user_speaking=1 WHERE call_id=?1",
            [&f.call],
        )
        .unwrap();
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    let target = completed(&f, "shelf", "The shelf is 137 cm wide.");
    // Dispatch guard handles a terminal before the regular result observer runs.
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_surfaces SET user_speaking=0,updated_at=?2 WHERE call_id=?1",
            params![f.call, now_ms()],
        )
        .unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    assert_eq!(batches(&f)[0].1, "superseded");
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let answer = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    assert_eq!(answer["commands"][0]["channel"], "commentary");
    assert_eq!(
        answer["commands"][0]["content"],
        "The shelf is 137 cm wide."
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT target_turn FROM input_deliveries WHERE id='shelf'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        target
    );
}
#[test]
fn unrelated_requests_finishing_in_reverse_order_both_remain_speakable() {
    let f = fixture();
    request(&f, "shelf", "submitted");
    request(&f, "weather", "submitted");
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET created_at=created_at+1000 WHERE id='weather'",
            [],
        )
        .unwrap();
    completed(&f, "weather", "The forecast says rain.");
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    completed(&f, "shelf", "The shelf is 137 cm wide.");
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let mut spoken = Vec::new();
    for _ in 0..5 {
        if let Some(batch) = crate::companion_delivery::take_ready(&f.s, &f.call).unwrap() {
            for c in batch["commands"].as_array().unwrap() {
                if c["channel"] == "commentary" {
                    spoken.push(c["content"].as_str().unwrap().to_owned());
                }
            }
        }
    }
    assert_eq!(
        spoken,
        vec!["The forecast says rain.", "The shelf is 137 cm wide."]
    );
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_events WHERE kind='card'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_delegations WHERE state='completed'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}
#[test]
fn invalid_or_unadmitted_sources_never_gain_a_pending_fact() {
    for case in [
        "held",
        "unknown",
        "cancelled",
        "other_actor",
        "other_agent",
        "other_room",
        "old_source",
        "changed_private_room",
        "wrong_target",
    ] {
        let f = fixture();
        request(&f, "shelf", "received");
        let other_room = f.s.create("Other private call").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        {
            let db = f.s.db.lock().unwrap();
            match case {
                "held" => {
                    db.execute("UPDATE companion_delegations SET state='held'", [])
                        .unwrap();
                }
                "unknown" | "cancelled" => {
                    db.execute("UPDATE input_deliveries SET state=?1", [case])
                        .unwrap();
                }
                "other_actor" => {
                    db.execute("UPDATE messages SET actor='other'", []).unwrap();
                }
                "other_agent" => {
                    db.execute("UPDATE input_deliveries SET agent_id='other'", [])
                        .unwrap();
                }
                "other_room" => {
                    db.execute(
                        "UPDATE companion_calls SET conversation_id=?1",
                        [&other_room],
                    )
                    .unwrap();
                }
                "old_source" => {
                    db.execute("UPDATE messages SET created_at=0", []).unwrap();
                }
                "changed_private_room" => {
                    db.execute(
                        "INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'other','member',0)",
                        [&f.room],
                    )
                    .unwrap();
                }
                "wrong_target" => {
                    db.execute("UPDATE input_deliveries SET target_turn='missing'", [])
                        .unwrap();
                }
                _ => unreachable!(),
            }
        }
        pending_context(&f.s, &f.call, "shelf").unwrap();
        assert!(batches(&f).is_empty(), "{case}");
    }
}
#[test]
fn unknown_delivery_or_process_interruption_never_replays_pending_context() {
    for interrupted in [false, true] {
        let f = fixture();
        request(&f, "shelf", "received");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        let _ = crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .unwrap();
        if interrupted {
            crate::companion_delivery::migrate(&f.s.db.lock().unwrap()).unwrap();
        } else {
            f.s.db
                .lock()
                .unwrap()
                .execute("UPDATE input_deliveries SET state='unknown'", [])
                .unwrap();
        }
        pending_context(&f.s, &f.call, "shelf").unwrap();
        assert_eq!(batches(&f).len(), 1);
        assert_eq!(
            batches(&f)[0].1,
            if interrupted {
                "unknown"
            } else {
                "dispatching"
            }
        );
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn changed_source_cannot_relabel_a_queued_pending_fact_or_reuse_its_event_id() {
    for label in [false, true] {
        let f = fixture();
        request(&f, "shelf", "received");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        let original = batches(&f)[0].2.clone();
        if label {
            f.s.db.lock().unwrap().execute("UPDATE companion_delegations SET request_text='Another question' WHERE id='shelf'",[]).unwrap();
        } else {
            request(&f, "weather", "received");
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE companion_delegations SET message_id='weather' WHERE id='shelf'",
                    [],
                )
                .unwrap();
        }
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none()
        );
        pending_context(&f.s, &f.call, "shelf").unwrap();
        assert_eq!(batches(&f).len(), 1);
        assert_eq!(batches(&f)[0].1, "superseded");
        assert_eq!(batches(&f)[0].2, original);
    }
}
