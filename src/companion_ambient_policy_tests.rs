//! Ambient relevance does not itself authorize an unsolicited Live append.
use super::*;

#[tokio::test]
async fn actual_negative_proactive_presence_decisions_retain_readable_evidence_without_live_context()
 {
    for mode in [
        "predicate-home-first",
        "predicate-home-second",
        "quiet",
        "legacy-later",
        "weak-notify",
        "low-confidence",
    ] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        let mut event = source_event();
        event["id"] = json!("minecraft:presence:Jarvis");
        event["kind"] = json!("presence_observation");
        event["actor"] = json!("Jarvis");
        event["provenance"] = json!({"kind":"minecraft_public_presence"});
        event["text"] = json!(
            r#"{"turn_active":false,"position":{"x":-312.29,"y":73,"z":68.5},"meaning":"Observed activity is not completion."}"#
        );
        observations(&f, vec![event.clone()]);
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        assert!(
            rx.try_recv().is_err(),
            "{mode}: negative proactive decision must not become unsolicited thinking speech"
        );
        assert_eq!(states(&f), vec!["retained"], "{mode}");
        let read = current_observations(&f.s, &f.id, Some("minecraft")).unwrap();
        assert_eq!(read["observations"][0]["source"], event);
        assert_eq!(read["observations"][0]["presentation_state"], "retained");
        assert_eq!(cards(&f).len(), 1);
        add_human_context(
            &f,
            "later-question",
            "What is the last known source activity?",
        );
        run(&f).await.unwrap();
        assert_eq!(
            calls(&f, "decision").len(),
            1,
            "no automatic reconsideration/retry for {mode}"
        );
        assert!(rx.try_recv().is_err());
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let decision: Value = serde_json::from_str(
            &db.query_row(
                "SELECT decision FROM companion_awareness WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        if mode == "predicate-home-first" {
            assert_eq!(
                decision["assessment"],
                json!({"kind":"relevance_and_proactive_v1","relevant_probability":0.92,"proactive_probability":0.04})
            );
        }
        if mode == "predicate-home-second" {
            assert_eq!(decision["assessment"]["proactive_probability"], 0.47);
        }
    }
}

fn retained_ambient(f: &Fixture, decision: Value, state: &str) -> (String, String, Value) {
    let (configuration, fingerprint) = config(&f.s).unwrap().unwrap();
    let source = &configuration.agents["default"][0];
    let event = source_event();
    record_event(&f.s, &f.id, source, &event, &fingerprint).unwrap();
    let key = event_key(&source.id, &event);
    let mut decision = decision;
    decision["context_revision"] =
        json!(human_context_revision(&f.s.db.lock().unwrap(), &f.id).unwrap());
    let raw = decision.to_string();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_awareness SET decision=?3,state=?4 WHERE call_id=?1 AND event_key=?2",
            params![f.id, key, raw, state],
        )
        .unwrap();
    (key, raw, event)
}
fn legacy(choice: &str, confidence: f64) -> Value {
    json!({"version":1,"status":"decided","model":"gpt-6-luna","decision":choice,"provider_choice":choice,"confidence":confidence,"selected_probability":confidence,"reason":"provider_decision"})
}

#[tokio::test]
async fn resumed_ready_quiet_later_or_weak_notify_retain_without_new_decision() {
    for (choice, confidence) in [("quiet", 0.95), ("later", 0.95), ("notify", 0.7)] {
        let f = fixture();
        let (_, raw, event) = retained_ambient(&f, legacy(choice, confidence), "ready");
        observations(&f, vec![]);
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        assert!(rx.try_recv().is_err(), "{choice}/{confidence}");
        assert_eq!(states(&f), vec!["retained"]);
        assert!(calls(&f, "decision").is_empty());
        assert_eq!(
            current_observations(&f.s, &f.id, Some("minecraft")).unwrap()["observations"][0]["source"],
            event
        );
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT decision FROM companion_awareness WHERE call_id=?1",
                    [&f.id],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            raw
        );
    }
}

#[tokio::test]
async fn legacy_unsent_quiet_batch_is_withdrawn_without_rewriting_or_replaying_history() {
    for phase in ["pending", "dispatching", "sent", "unknown"] {
        let f = fixture();
        let (key, raw, event) = retained_ambient(&f, legacy("quiet", 0.95), "context_queued");
        let (configuration, _) = config(&f.s).unwrap().unwrap();
        let source = &configuration.agents["default"][0];
        let event_id = format!("awareness-{key}:legacy:0");
        let commands = vec![
            json!({"op":"append","channel":"thinking","event_id":event_id,"delegation_id":null,"content":context_parts(source,&event,"")[0]}),
        ];
        let frozen = serde_json::to_string(&commands).unwrap();
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO companion_awareness_appends VALUES(?1,?2,?3,'thinking',?4)",
                params![
                    f.id,
                    key,
                    event_id,
                    if phase == "unknown" {
                        "unknown"
                    } else {
                        "queued"
                    }
                ],
            )
            .unwrap();
        crate::companion_delivery::enqueue_batch(
            &f.s,
            &f.id,
            "awareness:minecraft:source-event-1",
            commands,
        )
        .unwrap();
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_voice_batches SET state=?2 WHERE call_id=?1",
                params![f.id, phase],
            )
            .unwrap();
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.id)
                .unwrap()
                .is_none(),
            "{phase}"
        );
        let db = f.s.db.lock().unwrap();
        let (updates, state): (String, String) = db
            .query_row(
                "SELECT updates,state FROM companion_voice_batches WHERE call_id=?1",
                [&f.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(updates, frozen);
        assert_eq!(
            state,
            if phase == "pending" {
                "withdrawn"
            } else {
                phase
            }
        );
        assert_eq!(
            db.query_row(
                "SELECT decision FROM companion_awareness WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            raw
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM companion_awareness_appends WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            if phase == "pending" {
                "withdrawn"
            } else if phase == "unknown" {
                "unknown"
            } else {
                "queued"
            }
        );
        drop(db);
        assert!(calls(&f, "decision").is_empty());
    }
}

#[tokio::test]
async fn eligible_proactive_notice_and_exact_requested_replies_keep_their_existing_paths() {
    for mode in ["notify", "predicate-notify"] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        let command = rx.try_recv().unwrap();
        assert_eq!(command["channel"], "commentary");
        assert!(
            command["content"]
                .as_str()
                .unwrap()
                .contains("independent verification remains pending")
        );
        assert_eq!(cards(&f).len(), 1);
        assert_eq!(calls(&f, "decision").len(), 1);
        run(&f).await.unwrap();
        assert!(rx.try_recv().is_err());
        assert_eq!(calls(&f, "decision").len(), 1);
    }
    let f = fixture();
    std::fs::write(f.home.path().join("decision-mode"), "quiet").unwrap();
    let (request, queue) = retained_action(&f);
    let update = cumulative_private_outcome(&f, &request, &queue);
    cumulative_response(&f, &update);
    let parts = cumulative_parts(&f).await;
    assert!(
        parts
            .iter()
            .any(|p| p["channel"] == "commentary" && p["content"] == "Private exact-source reply.")
    );
    assert!(calls(&f, "decision").is_empty());
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(update)
    );
}

#[tokio::test]
async fn an_arbitrary_reason_cannot_turn_ambient_data_into_a_requested_outcome() {
    let f = fixture();
    let (key, _, event) = retained_ambient(
        &f,
        json!({"status":"not_requested","reason":"bound_requested_outcome"}),
        "context_queued",
    );
    let commands = vec![
        json!({"op":"append","channel":"thinking","event_id":format!("awareness-{key}:legacy:0"),"delegation_id":null,"content":"Source must not bypass meaningfulness by a label."}),
    ];
    crate::companion_delivery::enqueue_batch(
        &f.s,
        &f.id,
        "awareness:minecraft:source-event-1",
        commands,
    )
    .unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        current_observations(&f.s, &f.id, Some("minecraft")).unwrap()["observations"][0]["source"],
        event
    );
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn quiet_decision_settling_after_human_context_change_is_retained_once() {
    for mode in ["quiet", "legacy-later", "predicate-home-second"] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        std::fs::write(f.home.path().join("decision-block"), "").unwrap();
        let mut rx = quiet_browser(&f);
        let s = f.s.clone();
        let id = f.id.clone();
        let headers = f.headers.clone();
        let work = tokio::spawn(async move { tick(&s, &id, &headers).await });
        entered(&f, "decision").await;
        add_human_context(&f, "while-deciding", "Keep that in mind while we talk.");
        std::fs::remove_file(f.home.path().join("decision-block")).unwrap();
        work.await.unwrap().unwrap();
        assert_eq!(states(&f), vec!["retained"], "{mode}");
        run(&f).await.unwrap();
        assert_eq!(calls(&f, "decision").len(), 1, "{mode}");
        assert!(rx.try_recv().is_err());
        assert_eq!(
            current_observations(&f.s, &f.id, None).unwrap()["observations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let decision: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT decision FROM companion_awareness WHERE call_id=?1",
                    [&f.id],
                    |r| r.get(0),
                )
                .unwrap();
        let decision: Value = serde_json::from_str(&decision).unwrap();
        assert_eq!(decision["status"], "decided");
        assert_ne!(
            decision["context_revision"].as_str().unwrap(),
            human_context_revision(&f.s.db.lock().unwrap(), &f.id).unwrap()
        );
    }
}
