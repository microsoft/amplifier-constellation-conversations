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
fn user_floor(f: &Fixture) {
    let db = f.s.db.lock().unwrap();
    db.execute("UPDATE companion_surfaces SET user_speaking=1,agent_speaking=0,updated_at=?2,playback=?3 WHERE call_id=?1",params![f.call,now_ms(),json!({"state":"suppressed","floor":"user","interruption_id":"31c8d924-43bc-4cc4-a130-ef6a2a25602c","output_active":false}).to_string()]).unwrap();
    db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)",params![f.call,uuid::Uuid::new_v4().to_string(),json!({"role":"user","text":"I am still explaining."}).to_string(),now_ms()]).unwrap();
}
fn open_floor(f: &Fixture) {
    let db = f.s.db.lock().unwrap();
    db.execute("UPDATE companion_surfaces SET user_speaking=0,agent_speaking=0,updated_at=?2,playback=?3 WHERE call_id=?1",params![f.call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
    db.execute(
        "UPDATE companion_events SET at=?2 WHERE call_id=?1 AND kind='transcript'",
        params![f.call, now_ms() - 2000],
    )
    .unwrap();
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
    let facts: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        facts,
        json!({"kind":"call_request_status","request_id":"shelf",
        "request_revision":auth::digest(&serde_json::to_string(&("shelf",Some("How wide is the shelf?"))).unwrap()),
        "answer":"pending","external_execution":"unknown","caller_request":"How wide is the shelf?"})
    );
    assert!(text.len() <= 480);
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
fn typed_pending_fact_preserves_whole_label_and_revision_within_existing_budget() {
    for label in [
        None,
        Some("".into()),
        Some("A quoted \"request\"\nwith 界".into()),
        Some("界".repeat(200)),
    ] {
        let f = fixture();
        request(&f, "shelf", "submitted");
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_delegations SET request_text=?1 WHERE id='shelf'",
                params![label],
            )
            .unwrap();
        pending_context(&f.s, &f.call, "shelf").unwrap();
        let rows = batches(&f);
        let text = rows[0].2[0]["content"].as_str().unwrap();
        let facts: Value = serde_json::from_str(text).unwrap();
        assert!(text.len() <= 480);
        assert_eq!(facts["request_id"], "shelf");
        assert_eq!(facts["answer"], "pending");
        assert_eq!(facts["external_execution"], "unknown");
        assert_eq!(
            facts["request_revision"],
            auth::digest(&serde_json::to_string(&("shelf", &label)).unwrap())
        );
        if label
            .as_ref()
            .is_some_and(|s| !s.is_empty() && s.len() < 100)
        {
            assert_eq!(facts["caller_request"], label.as_deref().unwrap());
        } else {
            assert!(facts.get("caller_request").is_none());
        }
        // An omitted label remains bound in the stable revision, not shortened
        // into a different request or silently reused after a source change.
        if label.as_ref().is_some_and(|s| s.len() > 480) {
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE companion_delegations SET request_text=?1 WHERE id='shelf'",
                    ["別".repeat(200)],
                )
                .unwrap();
            assert!(
                crate::companion_delivery::take_ready(&f.s, &f.call)
                    .unwrap()
                    .is_none()
            );
            pending_context(&f.s, &f.call, "shelf").unwrap();
            assert_eq!(batches(&f).len(), 1);
            assert_eq!(batches(&f)[0].1, "superseded");
            assert_eq!(batches(&f)[0].2, rows[0].2);
        }
    }
}

fn legacy_pending_receipt(f: &Fixture) -> String {
    let text = "Request shelf is saved in this call. No matching completed answer has been recorded yet. This receipt does not establish external execution or progress. Caller request: \"How wide is the shelf?\"";
    queue_append(
        &f.s,
        &f.call,
        "thinking",
        Some("shelf"),
        text,
        &pending_context_key("shelf"),
    )
    .unwrap();
    text.into()
}

#[test]
fn retained_legacy_pending_fact_is_never_reformatted_replaced_or_replayed() {
    for state in ["pending", "sent", "unknown", "dispatching"] {
        let f = fixture();
        request(&f, "shelf", "submitted");
        let text = legacy_pending_receipt(&f);
        f.s.db
            .lock()
            .unwrap()
            .execute("UPDATE companion_voice_batches SET state=?1", [state])
            .unwrap();
        let retained = batches(&f)[0].2.clone();
        // Existing dispatches lose acknowledgment on process recovery; neither
        // that uncertainty nor a format upgrade permits a replacement append.
        if state == "dispatching" {
            crate::companion_delivery::migrate(&f.s.db.lock().unwrap()).unwrap();
        }
        for _ in 0..10 {
            pending_context(&f.s, &f.call, "shelf").unwrap();
        }
        assert_eq!(batches(&f).len(), 1);
        assert_eq!(batches(&f)[0].2, retained);
        user_floor(&f);
        let ready = crate::companion_delivery::take_ready(&f.s, &f.call).unwrap();
        if state == "pending" {
            let ready = ready.unwrap();
            assert_eq!(ready["commands"][0]["content"], text);
            assert_eq!(ready["commands"][0]["channel"], "thinking");
            crate::companion_delivery::sent(&f.s, &f.call, ready["batch_id"].as_str().unwrap())
                .unwrap();
        } else {
            assert!(ready.is_none());
        }
        for _ in 0..10 {
            pending_context(&f.s, &f.call, "shelf").unwrap();
        }
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none()
        );
        assert_eq!(batches(&f).len(), 1);
        assert_eq!(batches(&f)[0].2, retained);
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
}

#[test]
fn legacy_receipt_cannot_bypass_current_source_or_terminal_validation() {
    for change in ["label", "actor", "terminal"] {
        let f = fixture();
        request(&f, "shelf", "submitted");
        legacy_pending_receipt(&f);
        let old = batches(&f)[0].2.clone();
        match change {
            "label" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_delegations SET request_text='Different question'",
                        [],
                    )
                    .unwrap();
            }
            "actor" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE messages SET actor='other'", [])
                    .unwrap();
            }
            "terminal" => {
                completed(&f, "shelf", "The shelf is 137 cm wide.");
            }
            _ => unreachable!(),
        }
        user_floor(&f);
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none()
        );
        pending_context(&f.s, &f.call, "shelf").unwrap();
        assert_eq!(batches(&f).len(), 1);
        assert_eq!(batches(&f)[0].1, "superseded");
        assert_eq!(batches(&f)[0].2, old);
    }
}

#[test]
fn typed_pending_fields_cannot_be_relabelled_or_promoted_to_execution() {
    for (key, value) in [
        ("request_id", json!("other")),
        ("request_revision", json!("new")),
        ("answer", json!("complete")),
        ("external_execution", json!(true)),
        ("caller_request", json!("Different question")),
        ("kind", json!("progress")),
    ] {
        let f = fixture();
        request(&f, "shelf", "submitted");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        let mut updates = batches(&f)[0].2.clone();
        let mut content: Value =
            serde_json::from_str(updates[0]["content"].as_str().unwrap()).unwrap();
        content[key] = value;
        updates[0]["content"] = json!(content.to_string());
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_voice_batches SET updates=?1",
                [updates.to_string()],
            )
            .unwrap();
        user_floor(&f);
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none(),
            "{key}"
        );
        assert_eq!(batches(&f)[0].1, "superseded");
    }
}
#[test]
fn terminal_arriving_under_user_floor_retires_unsent_pending_but_preserves_answer() {
    let f = fixture();
    request(&f, "shelf", "received");
    user_floor(&f);
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let target = completed(&f, "shelf", "The shelf is 137 cm wide.");
    // Terminal wins before quiet receipt claim, even without an open speech
    // floor or a regular result-observer poll.
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    assert_eq!(batches(&f)[0].1, "superseded");
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    open_floor(&f);
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
        user_floor(&f);
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
        user_floor(&f);
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

#[test]
fn quiet_pending_crosses_continuing_input_without_dispatching_speech_or_more_work() {
    let f = fixture();
    request(&f, "shelf", "received");
    request(&f, "weather", "submitted");
    for (key, channel) in [
        ("older-thinking", "thinking"),
        ("older-answer", "commentary"),
    ] {
        queue_append(
            &f.s,
            &f.call,
            channel,
            None,
            "An independent earlier result.",
            key,
        )
        .unwrap();
    }
    pending_context(&f.s, &f.call, "shelf").unwrap();
    pending_context(&f.s, &f.call, "weather").unwrap();
    // Every poll still has actual fresh user input and suppressed output. Only
    // exact source receipts advance; quiet FIFO and the held speech FIFO remain.
    for expected in [Some("shelf"), Some("weather"), None, None] {
        user_floor(&f);
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        let ready = crate::companion_delivery::take_ready(&f.s, &f.call).unwrap();
        match expected {
            Some(did) => {
                let ready = ready.unwrap();
                assert_eq!(ready["commands"].as_array().unwrap().len(), 1);
                assert_eq!(ready["commands"][0]["delegation_id"], did);
                assert_eq!(ready["commands"][0]["channel"], "thinking");
                assert_eq!(ready["commands"][0]["event_id"], pending_context_key(did));
                crate::companion_delivery::sent(&f.s, &f.call, ready["batch_id"].as_str().unwrap())
                    .unwrap();
            }
            None => assert!(ready.is_none()),
        }
    }
    assert_eq!(batches(&f).len(), 4);
    assert_eq!(batches(&f)[0].1, "pending");
    assert_eq!(batches(&f)[1].1, "pending");
    open_floor(&f);
    for expected in ["older-thinking", "older-answer"] {
        let ready = crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .unwrap();
        assert_eq!(ready["commands"][0]["event_id"], expected);
    }
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM input_deliveries", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_events WHERE kind='card'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn quiet_pending_can_cross_agent_speech_or_only_a_recent_transcript() {
    for agent_speaking in [false, true] {
        let f = fixture();
        request(&f, "shelf", "received");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        user_floor(&f);
        f.s.db.lock().unwrap().execute("UPDATE companion_surfaces SET user_speaking=0,agent_speaking=?2,playback=?3 WHERE call_id=?1",params![f.call,agent_speaking,json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
        assert!(
            !crate::companion_delivery::floor_open_in(&f.s.db.lock().unwrap(), &f.call).unwrap()
        );
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn pending_receipt_floor_exception_requires_exact_singleton_identity_and_content() {
    for case in [
        "semantic_key",
        "event_id",
        "channel",
        "extra_field",
        "null_delegation",
        "other_delegation",
        "mixed_batch",
        "content",
    ] {
        let f = fixture();
        request(&f, "shelf", "received");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        user_floor(&f);
        let mut updates = batches(&f)[0].2.clone();
        let mut key = pending_context_key("shelf");
        match case {
            "semantic_key" => key.push_str("-other"),
            "event_id" => updates[0]["event_id"] = json!("request-pending-forged"),
            "channel" => updates[0]["channel"] = json!("commentary"),
            "extra_field" => updates[0]["quiet"] = json!(true),
            "null_delegation" => updates[0]["delegation_id"] = Value::Null,
            "other_delegation" => updates[0]["delegation_id"] = json!("other"),
            "mixed_batch" => {
                let mut extra = updates[0].clone();
                extra["channel"] = json!("commentary");
                updates.as_array_mut().unwrap().push(extra);
            }
            "content" => updates[0]["content"] = json!("The lookup is executing."),
            _ => unreachable!(),
        }
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_voice_batches SET semantic_key=?1,updates=?2",
                params![key, updates.to_string()],
            )
            .unwrap();
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none(),
            "{case}"
        );
        assert!(
            !batches(&f)
                .iter()
                .any(|(_, state, _)| state == "dispatching"),
            "{case}"
        );
    }
}

#[test]
fn quiet_pending_does_not_cross_stale_missing_or_unknown_surface() {
    for case in [
        "stale",
        "future",
        "no_surface",
        "no_playback",
        "unknown_playback",
        "unknown_floor",
        "unknown_user",
        "unknown_agent",
    ] {
        let f = fixture();
        request(&f, "shelf", "received");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        user_floor(&f);
        let db = f.s.db.lock().unwrap();
        let sql = match case {
            "stale" => "UPDATE companion_surfaces SET updated_at=0",
            "future" => "UPDATE companion_surfaces SET updated_at=updated_at+100000",
            "no_surface" => "DELETE FROM companion_surfaces",
            "no_playback" => "UPDATE companion_surfaces SET playback=NULL",
            "unknown_playback" => {
                "UPDATE companion_surfaces SET playback='{\"state\":\"unknown\",\"floor\":\"user\"}'"
            }
            "unknown_floor" => {
                "UPDATE companion_surfaces SET playback='{\"state\":\"ready\",\"floor\":\"unknown\"}'"
            }
            "unknown_user" => "UPDATE companion_surfaces SET user_speaking=NULL",
            "unknown_agent" => "UPDATE companion_surfaces SET agent_speaking=NULL",
            _ => unreachable!(),
        };
        db.execute(sql, []).unwrap();
        drop(db);
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none(),
            "{case}"
        );
        assert_eq!(batches(&f)[0].1, "pending", "{case}");
    }
}

#[test]
fn queued_quiet_receipt_rechecks_source_and_private_identity_during_user_speech() {
    for case in [
        "unknown",
        "cancelled",
        "actor",
        "source_time",
        "target",
        "agent",
        "endpoint",
        "private_room",
        "closed_call",
    ] {
        let f = fixture();
        request(&f, "shelf", "received");
        pending_context(&f.s, &f.call, "shelf").unwrap();
        user_floor(&f);
        let db = f.s.db.lock().unwrap();
        match case {
            "unknown" | "cancelled" => {
                db.execute("UPDATE input_deliveries SET state=?1", [case])
                    .unwrap();
            }
            "actor" => {
                db.execute("UPDATE messages SET actor='other'", []).unwrap();
            }
            "source_time" => {
                db.execute("UPDATE messages SET created_at=0", []).unwrap();
            }
            "target" => {
                db.execute("UPDATE input_deliveries SET target_turn='missing'", [])
                    .unwrap();
            }
            "agent" => {
                db.execute("UPDATE input_deliveries SET agent_id='other'", [])
                    .unwrap();
            }
            "endpoint" => {
                db.execute("UPDATE companion_calls SET endpoint_hash='changed'", [])
                    .unwrap();
            }
            "private_room" => {
                db.execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'other','member',0)",[&f.room]).unwrap();
            }
            "closed_call" => {
                db.execute("UPDATE companion_calls SET state='closed'", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(db);
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none(),
            "{case}"
        );
        assert!(
            !batches(&f)
                .iter()
                .any(|(_, state, _)| state == "dispatching"),
            "{case}"
        );
    }
}

#[test]
fn quiet_receipt_claimed_before_terminal_keeps_final_answer_behind_speech_floor() {
    let f = fixture();
    request(&f, "shelf", "received");
    user_floor(&f);
    pending_context(&f.s, &f.call, "shelf").unwrap();
    let pending = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    assert_eq!(pending["commands"][0]["channel"], "thinking");
    completed(&f, "shelf", "The shelf is 137 cm wide.");
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert_eq!(batches(&f)[0].1, "dispatching");
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    crate::companion_delivery::sent(&f.s, &f.call, pending["batch_id"].as_str().unwrap()).unwrap();
    open_floor(&f);
    let answer = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    assert_eq!(answer["commands"][0]["channel"], "commentary");
    assert_eq!(
        answer["commands"][0]["content"],
        "The shelf is 137 cm wide."
    );
    for _ in 0..10 {
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
    }
    assert_eq!(batches(&f).len(), 2);
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
}
