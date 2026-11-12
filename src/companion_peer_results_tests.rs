//! Local database regressions: peer continuation text is not voice output.
//! No process, transport, model or external service is started.
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
        label: "Quiet peer fixture".into(),
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
        auth_mode: Default::default(),
        transport: Default::default(),
        agent: Some(Agent {
            label: "Coach".into(),
            command: "/bin/true".into(),
            args: vec![],
            cwd: home.path().to_string_lossy().into(),
            session_cwd: None,
            env: BTreeMap::new(),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        }),
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

fn answer(f: &Fixture, id: &str, text: &str) {
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,'Coach','default')",params![id,f.room,text,now_ms()]).unwrap();
}

fn delivery(f: &Fixture, input: &str, target: &str, peer: bool) {
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,?3,'A retained source','complete',?4,?4,?5)",params![input,f.room,if peer {"system"} else {"user"},now_ms(),if peer {"Peer"} else {"owner"}]).unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES(?1,?1,?2,'default','direct','completed',?3,?4)",params![input,f.room,target,now_ms()]).unwrap();
    if peer {
        db.execute(
            "INSERT INTO huddle_peer_deliveries(input_id,message_id,binding) VALUES(?1,?2,?3)",
            params![
                input,
                input,
                json!({"call_id":f.call,"parent_id":f.room}).to_string()
            ],
        )
        .unwrap();
    }
}

fn events(f: &Fixture) -> Vec<Value> {
    let db = f.s.db.lock().unwrap();
    let mut q = db
        .prepare("SELECT data FROM companion_events WHERE call_id=?1 ORDER BY seq")
        .unwrap();
    q.query_map([&f.call], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}

#[test]
fn quiet_peer_completion_is_neither_card_nor_automatic_voice_and_is_not_rewritten() {
    let f = fixture();
    answer(&f, "peer-answer", "A routine private coordination detail.");
    delivery(&f, "peer-input", "peer-answer", true);
    for _ in 0..2 {
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        // Defense in depth also applies to direct publication callers.
        publish_result(
            &f.s,
            &f.call,
            &f.room,
            "peer-answer",
            "A routine private coordination detail.",
        )
        .unwrap();
    }
    assert!(events(&f).is_empty());
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT text FROM messages WHERE id='peer-answer'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "A routine private coordination detail."
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM input_deliveries WHERE message_id='peer-input'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "completed"
    );
}

#[test]
fn quiet_turns_do_not_consume_the_ordinary_result_batch_or_hide_a_human_answer() {
    let f = fixture();
    for index in 0..20 {
        let target = format!("quiet-answer-{index}");
        answer(&f, &target, "Routine coordination");
        delivery(&f, &format!("quiet-input-{index}"), &target, true);
    }
    answer(&f, "ordinary-answer", "The answer you requested.");
    delivery(&f, "human-input", "ordinary-answer", false);
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let records = events(&f);
    assert_eq!(
        records
            .iter()
            .filter(|row| row["message_id"] == "ordinary-answer"
                && row["text"] == "The answer you requested.")
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("Routine coordination")
    );
    let db = f.s.db.lock().unwrap();
    let batches: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(batches, 1);
}

#[test]
fn a_real_human_input_in_the_same_target_retains_ordinary_result_presentation() {
    let f = fixture();
    answer(&f, "combined-answer", "Your clarification is answered.");
    delivery(&f, "peer-input", "combined-answer", true);
    delivery(&f, "human-followup", "combined-answer", false);
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        events(&f)
            .iter()
            .any(|row| row["message_id"] == "combined-answer"
                && row["request_id"] == "human-followup")
    );
}

#[test]
fn an_explicit_huddle_outcome_still_returns_after_a_quiet_peer_turn() {
    let f = fixture();
    answer(&f, "quiet-answer", "Working through a routine peer detail.");
    delivery(&f, "quiet-input", "quiet-answer", true);
    let huddle = f.s.create("Review").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = uuid::Uuid::new_v4().to_string();
    let post = uuid::Uuid::new_v4().to_string();
    let source_text = "Review this together and tell me the outcome.";
    let text = "The requested review is complete; one question remains.";
    let at = now_ms();
    let origin = json!({"kind":"conversation","conversation_id":f.room,"request_id":source,"message_id":source,"source_revision":auth::digest(source_text),"occurred_at":at,"actor":{"id":"owner","kind":"human"}});
    let payload = json!({"request_id":post,"text":text,"target_agent_ids":[],"kind":"outcome","source":null,"outcome":{"kind":"result","evidence":[]}});
    let receipt = json!({"version":1,"status":"saved","message_id":post,"request_id":post,"room_id":huddle,"kind":"outcome","agent_id":"default","origin":origin,"revision":auth::digest(&payload.to_string()),"text_sha256":auth::digest(text),"inspect_url":format!("https://fixture.test/?conversation={huddle}")});
    {
        let mut db = f.s.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')",params![source,f.room,source_text,at]).unwrap();
        tx.execute("UPDATE conversations SET shared=1 WHERE id=?1", [&huddle])
            .unwrap();
        tx.execute("INSERT INTO huddles(room_id,request_id,purpose,origin,fingerprint) VALUES(?1,?2,'Review',?3,'fixture')",params![huddle,uuid::Uuid::new_v4().to_string(),origin.to_string()]).unwrap();
        tx.execute("INSERT INTO huddle_participants(room_id,agent_id,label,connection_id,token_hash) VALUES(?1,'default','Coach','fixture','private-test-hash')",[&huddle]).unwrap();
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,'Coach','default')",params![post,huddle,text,at]).unwrap();
        let seq = tx.last_insert_rowid();
        tx.execute("INSERT INTO huddle_posts(message_id,room_id,agent_id,payload,receipt) VALUES(?1,?2,'default',?3,?4)",params![post,huddle,payload.to_string(),receipt.to_string()]).unwrap();
        let row = json!({"huddle_id":huddle,"inspect_url":receipt["inspect_url"],"origin":origin,"message":{"id":post,"seq":seq,"agent_id":"default","actor":"Coach","text":text,"created_at":at},"outcome":payload["outcome"],"revision":receipt["revision"]});
        assert_eq!(
            crate::companion_huddles::record_return(&f.s, &tx, &row).unwrap()["state"],
            "stored"
        );
        tx.commit().unwrap();
    }
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let records = events(&f);
    assert!(
        records
            .iter()
            .any(|row| row["text"] == text && row["huddle_id"] == huddle)
    );
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("Working through a routine peer detail")
    );
    let db = f.s.db.lock().unwrap();
    let raw: String = db
        .query_row(
            "SELECT updates FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw.contains(text));
    assert!(!raw.contains("Working through a routine peer detail"));
}

#[test]
fn external_action_result_keeps_compound_facts_local_across_incorporated_refinement() {
    let f = fixture();
    let text = "I am back, though the movement was only queued. The independent answer is 137 cm.";
    answer(&f, "action-answer", text);
    delivery(&f, "action-input", "action-answer", false);
    delivery(&f, "refinement", "action-answer", false);
    {
        let db = f.s.db.lock().unwrap();
        db.execute(
            "UPDATE input_deliveries SET state='incorporated' WHERE message_id='refinement'",
            [],
        )
        .unwrap();
        for (id, message, at) in [
            ("original", "action-input", 1),
            ("followup", "refinement", 2),
        ] {
            db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,0,'dispatched',?3,?4)", params![f.call,id,message,at]).unwrap();
        }
        db.execute("INSERT INTO companion_actions VALUES(?1,'action-request','original','minecraft','Walk and return.','{}','queued',?2,?3)", params![f.call,json!({"state":"queued","receipt":{"queued":true}}).to_string(),now_ms()]).unwrap();
    }
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none(),
        "A queued handoff's native narration must not reach Live through either channel"
    );
    assert_eq!(
        events(&f)
            .iter()
            .filter(|event| event["message_id"] == "action-answer" && event["text"] == text)
            .count(),
        1
    );
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
}

#[test]
fn external_action_result_state_and_target_scope_preserve_ordinary_answers() {
    for state in [
        "submitting",
        "queued",
        "accepted",
        "unknown",
        "failed",
        "completed",
        "none",
        "other-target",
        "other-call",
    ] {
        let f = fixture();
        answer(&f, "answer", "The result remains exactly as authored.");
        delivery(&f, "input", "answer", false);
        {
            let db = f.s.db.lock().unwrap();
            db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'original',0,'dispatched','input',1)", [&f.call]).unwrap();
        }
        if state != "none" {
            let action_delegation = if state == "other-target" {
                answer(&f, "other-answer", "Other result.");
                delivery(&f, "other-input", "other-answer", false);
                f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'other',0,'completed','other-input',2)", [&f.call]).unwrap();
                "other"
            } else {
                "original"
            };
            let action_call = if state == "other-call" {
                let other = uuid::Uuid::new_v4().to_string();
                f.s.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at) VALUES(?1,'owner',?1,'fixture','default',?2,'connected',1,1)", params![other,f.room]).unwrap();
                other
            } else {
                f.call.clone()
            };
            let stored_state = if state.starts_with("other-") {
                "queued"
            } else {
                state
            };
            f.s.db.lock().unwrap().execute("INSERT INTO companion_actions VALUES(?1,'request',?2,'minecraft','Walk.','{}',?3,?4,1)",params![action_call,action_delegation,stored_state,json!({"state":stored_state}).to_string()]).unwrap();
        }
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        let batch = crate::companion_delivery::take_ready(&f.s, &f.call).unwrap();
        if matches!(state, "submitting" | "queued" | "accepted") {
            assert!(batch.is_none(), "{state}: pending action prose stays local");
        } else {
            let batch = batch.unwrap();
            assert_eq!(
                batch["commands"][0]["channel"],
                if state == "unknown" {
                    "thinking"
                } else {
                    "commentary"
                },
                "{state}"
            );
            assert_eq!(
                batch["commands"][0]["content"],
                "The result remains exactly as authored."
            );
        }
        assert!(
            events(&f)
                .iter()
                .any(|event| event["message_id"] == "answer"
                    && event["text"] == "The result remains exactly as authored.")
        );
    }
}

#[test]
fn external_action_long_compound_answer_stays_whole_in_history_after_domain_terminal() {
    let f = fixture();
    let text = "The independent measurement is 137 cm. ".repeat(20);
    answer(&f, "answer", &text);
    delivery(&f, "input", "answer", false);
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'original',0,'dispatched','input',1)", [&f.call]).unwrap();
        db.execute("INSERT INTO companion_actions VALUES(?1,'request','original','minecraft','Walk.','{}','queued',?2,1)",params![f.call,json!({"state":"queued","observation":{"state":"settled","conversation":{"terminal_state":"interrupted"},"source_replies":[{"text":"The movement was declined."}]}}).to_string()]).unwrap();
    }
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    assert!(
        events(&f)
            .iter()
            .any(|event| event["message_id"] == "answer" && event["text"] == text)
    );
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT text FROM messages WHERE id='answer'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        text
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM companion_actions WHERE request_id='request'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "queued"
    );
}

#[test]
fn queued_handoff_does_not_republish_the_observed_false_native_execution_claim() {
    let f = fixture();
    let text = "The walk instruction was queued to the Home Server body, but it hasn't executed yet — the receipt still reads queued, with no confirmed acceptance, movement, or completion. A queued status proves neither that the walk happened nor that it didn't.\n\nThe body needs a turn to pick up the instruction and move. I won't replay the request (that risks a double-execution). If you'd like, give it a moment and I can check the receipt again for a confirmed two-blocks-west-and-back result.";
    answer(&f, "native-final", text);
    delivery(&f, "input", "native-final", false);
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'original',0,'dispatched','input',1)", [&f.call]).unwrap();
        db.execute("INSERT INTO companion_actions VALUES(?1,'request','original','minecraft','Walk.','{}','queued',?2,1)",params![f.call,json!({"state":"queued","receipt":{"queued":true},"message":"This does not confirm execution or fulfillment."}).to_string()]).unwrap();
    }
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none(),
        "Thinking is not silent: do not auto-publish receipt-only model narration"
    );
    assert!(
        events(&f)
            .iter()
            .any(|event| event["message_id"] == "native-final" && event["text"] == text)
    );
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT text FROM messages WHERE id='native-final'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        text
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM companion_actions WHERE request_id='request'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "queued"
    );
}

fn seed_pending_action(f: &Fixture, target: &str, text: &str, state: &str) {
    answer(f, target, text);
    delivery(f, "action-input", target, false);
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'original',0,'dispatched','action-input',1)",[&f.call]).unwrap();
    db.execute("INSERT INTO companion_actions VALUES(?1,'request','original','minecraft','Walk.','{}',?2,?3,1)",params![f.call,state,json!({"state":state}).to_string()]).unwrap();
}

#[test]
fn pending_action_does_not_hide_an_independent_target_answer() {
    let f = fixture();
    seed_pending_action(
        &f,
        "action-answer",
        "Only queued; unknown physical outcome.",
        "queued",
    );
    answer(
        &f,
        "independent-answer",
        "The independent measurement is 137 cm.",
    );
    delivery(&f, "independent-input", "independent-answer", false);
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let batch = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    assert_eq!(batch["commands"][0]["channel"], "commentary");
    assert_eq!(
        batch["commands"][0]["content"],
        "The independent measurement is 137 cm."
    );
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .is_none()
    );
    assert!(events(&f).iter().any(|e| e["message_id"] == "action-answer"
        && e["text"] == "Only queued; unknown physical outcome."));
}

#[test]
fn pending_action_keeps_real_image_publication_without_its_native_action_claim() {
    let f = fixture();
    let text = "The requested movement is already complete. Here is the saved image.";
    seed_pending_action(&f, "action-answer", text, "queued");
    f.s.db.lock().unwrap().execute("INSERT INTO attachments(id,conversation_id,name,mime_type,data,bytes,created_by,created_at,message_id) VALUES('actual-image',?1,'view.png','image/png',X'89504E47',4,'Coach',?2,'action-answer')",params![f.room,now_ms()]).unwrap();
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let batch = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert!(commands.iter().any(|c| c["channel"] == "commentary"
        && c["content"] == "The image is saved in our shared space."));
    assert!(
        !serde_json::to_string(commands)
            .unwrap()
            .contains("movement is already complete")
    );
    let card = events(&f)
        .into_iter()
        .find(|e| e["message_id"] == "action-answer" && e["text"] == text)
        .unwrap();
    assert_eq!(card["attachments"][0]["id"], "actual-image");
}

#[test]
fn historical_pending_sent_or_unknown_native_batches_are_not_rewritten_or_replayed() {
    for phase in ["pending", "sent", "unknown"] {
        let f = fixture();
        let text = "The unchanged historical native answer.";
        seed_pending_action(&f, "answer", text, "queued");
        let old =
            json!({"message_id":"answer","conversation_id":f.room,"text":text,"attachments":[]});
        let commands = vec![
            json!({"op":"append","channel":"thinking","event_id":"historical-event","delegation_id":"original","content":text}),
        ];
        {
            let db = f.s.db.lock().unwrap();
            insert_event(&db, &f.call, "result:answer", "card", &old).unwrap();
        }
        crate::companion_delivery::enqueue_batch(&f.s, &f.call, "result:answer", commands).unwrap();
        {
            let db = f.s.db.lock().unwrap();
            db.execute(
                "UPDATE companion_voice_batches SET state=?2 WHERE call_id=?1",
                params![f.call, phase],
            )
            .unwrap();
        }
        let before: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT updates FROM companion_voice_batches WHERE call_id=?1",
                    [&f.call],
                    |r| r.get(0),
                )
                .unwrap();
        for _ in 0..2 {
            observe_results(&f.s, &f.call, &f.room, true).unwrap();
        }
        let db = f.s.db.lock().unwrap();
        let after:(String,String,i64)=db.query_row("SELECT updates,state,(SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1) FROM companion_voice_batches WHERE call_id=?1",[&f.call],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(after, (before, phase.to_owned(), 1));
    }
}

#[test]
fn unknown_submission_without_observer_and_mixed_receipts_keep_the_existing_answer() {
    for sibling in [None, Some("queued"), Some("accepted"), Some("submitting")] {
        let f = fixture();
        let text = "The submission receipt is unknown. Whether any effect occurred is unknown; do not retry it.";
        seed_pending_action(&f, "answer", text, "unknown");
        if let Some(state) = sibling {
            f.s.db.lock().unwrap().execute("INSERT INTO companion_actions VALUES(?1,'sibling','original','other-connection','Separate work.','{}',?2,?3,2)",params![f.call,state,json!({"state":state}).to_string()]).unwrap();
        }
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM companion_observations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        let batch = crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .unwrap();
        assert_eq!(batch["commands"][0]["channel"], "thinking");
        assert_eq!(batch["commands"][0]["content"], text);
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.call)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM companion_actions WHERE request_id='request'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "unknown"
        );
    }
}

#[test]
fn definite_failure_or_completion_with_pending_sibling_keeps_existing_answer() {
    for state in ["failed", "completed"] {
        let f = fixture();
        let text = if state == "failed" {
            "The helper explicitly rejected this request. The other action has only a queued receipt."
        } else {
            "The helper recorded its own completed operation; that does not establish the other queued action's outcome."
        };
        seed_pending_action(&f, "answer", text, state);
        f.s.db.lock().unwrap().execute("INSERT INTO companion_actions VALUES(?1,'sibling','original','other-connection','Separate work.','{}','queued',?2,2)",params![f.call,json!({"state":"queued"}).to_string()]).unwrap();
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        let batch = crate::companion_delivery::take_ready(&f.s, &f.call)
            .unwrap()
            .unwrap();
        assert_eq!(batch["commands"][0]["channel"], "thinking");
        assert_eq!(batch["commands"][0]["content"], text);
    }
}

#[test]
fn unknown_submission_keeps_long_answer_on_original_thinking_channel() {
    let f = fixture();
    let text = "The submission receipt is unknown; whether the action occurred is unconfirmed. "
        .repeat(12);
    seed_pending_action(&f, "answer", &text, "unknown");
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let batch = crate::companion_delivery::take_ready(&f.s, &f.call)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert!(commands.len() > 2);
    assert!(commands.iter().all(|c| c["channel"] == "thinking"));
    assert_eq!(
        commands
            .iter()
            .skip(1)
            .map(|c| c["content"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        complete_fragments(&text, 360, 16).0
    );
    assert!(
        events(&f)
            .iter()
            .any(|e| e["message_id"] == "answer" && e["text"] == text)
    );
}
