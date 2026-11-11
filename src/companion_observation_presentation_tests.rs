//! Actual read/observer retention boundaries, with disposable helpers only.
use super::*;

#[tokio::test]
async fn repeated_native_status_reads_retain_qualified_facts_without_live_batches() {
    let f = fixture();
    std::fs::write(
        f.home.path().join("companion-actions.json"),
        r#"{"enabled":true,"agents":{}}"#,
    )
    .unwrap();
    let grant = crate::companion_tools::create_mcp_config(&f.s, &f.id).unwrap();
    let mut h = HeaderMap::new();
    h.insert(
        "authorization",
        format!("Bearer {}", grant["token"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    for index in 0..3 {
        let mut presence = source_event();
        presence["id"] = json!("current-position");
        presence["revision"] = json!(format!("position-{index}"));
        presence["kind"] = json!("presence_observation");
        presence["actor"] = json!("Jarvis");
        presence["provenance"] = json!({"kind":"minecraft_public_presence"});
        presence["text"] = json!(
            json!({"position":{"x":index,"y":64,"z":7},"ready":true,"turn_active":index==1})
                .to_string()
        );
        fresh_result(&f, vec![presence]);
        let query =
            serde_json::from_value(json!({"connection_id":"minecraft","view":"current"})).unwrap();
        let axum::Json(response) = crate::companion_tools::connection_status(
            axum::extract::State(f.s.clone()),
            axum::extract::Path(f.id.clone()),
            h.clone(),
            axum::extract::Query(query),
        )
        .await
        .unwrap();
        assert_eq!(response["reports"].as_array().unwrap().len(), 1);
        assert_eq!(response["read_only"], true);
        assert_eq!(response["authority"], false);
        assert!(response.to_string().contains(&format!("position-{index}")));
    }
    let db = f.s.db.lock().unwrap();
    let retained:i64=db.query_row("SELECT COUNT(*) FROM companion_awareness WHERE call_id=?1 AND json_extract(data,'$.kind')='requested_status_qualifier'",[&f.id],|r|r.get(0)).unwrap();
    assert_eq!(retained, 0);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
            [&f.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0,
        "A read tool is not an automatic Live notification stream"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_awareness_appends WHERE call_id=?1",
            [&f.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    let saved = current_observations(&f.s, &f.id, Some("minecraft")).unwrap();
    assert!(!saved.to_string().contains("requested_status_qualifier"));
    assert!(calls(&f, "decision").is_empty());
}

fn pure_input(f: &Fixture, request: &str, queue: &str, state: &str) -> Value {
    let mut update = returned_outcome(f, request, queue, false);
    update["delivery_state"] = json!(state);
    update["state"] = json!(if state == "admitted" {
        "admitted"
    } else {
        "unknown"
    });
    update["conversation"]["blocked_reason"] = Value::Null;
    update["task_authorizations"] = json!([]);
    update["responses"] = json!([]);
    update
}

#[tokio::test]
async fn pure_queued_and_admitted_bookkeeping_keeps_full_receipt_card_and_no_live_append() {
    for state in ["queued", "admitted"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let update = pure_input(&f, &request, &queue, state);
        cumulative_response(&f, &update);
        let parts = cumulative_parts(&f).await;
        assert!(
            parts.is_empty(),
            "{state}: transport admission alone is not a result notification"
        );
        assert_eq!(
            action_observation(&f.s, &f.id, &request).unwrap(),
            Some(update.clone())
        );
        assert_eq!(cards(&f).len(), 1);
        assert!(
            cards(&f)[0]["text"]
                .as_str()
                .unwrap()
                .contains("Recorded turn scope: conversation only")
        );
        assert_eq!(states(&f), vec!["observed"]);
        assert!(cumulative_parts(&f).await.is_empty());
        assert!(calls(&f, "decision").is_empty());
    }
}

#[tokio::test]
async fn meaningful_input_outcomes_preserve_evidence_without_standalone_bookkeeping() {
    for mode in [
        "blocker",
        "unknown",
        "interrupted",
        "terminal-no-reply",
        "public-reply",
        "private-reply",
        "authority",
        "resumption",
        "omission",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = pure_input(&f, &request, &queue, "admitted");
        let expected = match mode {
            "blocker" => {
                update["conversation"]["blocked_reason"] = json!("unresolved_activity_effects");
                "Recorded turn blocker"
            }
            "unknown" => {
                update["delivery_state"] = json!("unknown");
                update["state"] = json!("unknown");
                "outcome remains unknown"
            }
            "interrupted" => {
                update["delivery_state"] = json!("interrupted");
                update["state"] = json!("interrupted");
                "Handling was interrupted"
            }
            "terminal-no-reply" => {
                update["delivery_state"] = json!("terminal");
                update["state"] = json!("settled");
                "No public reply is included"
            }
            "public-reply" => {
                update["responses"] = json!([{"id":"public-result","audience":"public","at":timestamp(now_ms()),"text":"The requested walk was declined."}]);
                "The requested walk was declined"
            }
            "private-reply" => {
                update = cumulative_private_outcome(&f, &request, &queue);
                "Private exact-source reply"
            }
            "authority" => {
                update["task_authorizations"] =
                    json!([{"operation":"amend","task_id":"original-task","authority_revision":2}]);
                "amendment to the existing assignment"
            }
            "resumption" => {
                update["work_resumptions"] = json!([{"task_id":"original-task","recorded_status":"running","scheduled":true,"execution_started":false}]);
                update["work_resumptions_omitted"] = json!(0);
                "work was scheduled"
            }
            _ => {
                update["responses_omitted"] = json!(1);
                "reply record is incomplete"
            }
        };
        cumulative_response(&f, &update);
        let parts = cumulative_parts(&f).await;
        let text = cumulative_text(&parts);
        assert!(text.contains(expected), "{mode}: {text}");
        assert!(!text.contains("Input admitted"), "{mode}");
        assert!(!text.contains("Recorded turn scope"), "{mode}");
        if mode == "private-reply" {
            assert!(
                parts.iter().any(|p| p["channel"] == "commentary"
                    && p["content"] == "Private exact-source reply.")
            );
        }
        if mode == "resumption" {
            assert!(text.contains("not a current activity check"));
        }
        assert_eq!(
            action_observation(&f.s, &f.id, &request).unwrap(),
            Some(update)
        );
        assert_eq!(cards(&f).len(), 1);
        assert!(calls(&f, "decision").is_empty());
    }
}

#[tokio::test]
async fn retained_bookkeeping_and_historical_read_qualifiers_cannot_become_ambient_speech() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let update = pure_input(&f, &request, &queue, "admitted");
    cumulative_response(&f, &update);
    assert!(cumulative_parts(&f).await.is_empty());
    let (configuration, policy) = config(&f.s).unwrap().unwrap();
    let source = &configuration.agents["default"][0];
    let mut old = source_event();
    old["id"] = json!("retained-legacy-status");
    old["kind"] = json!("requested_status_qualifier");
    old["provenance"] = json!({"kind":"companion_requested_connection_status","read_only":true});
    record_source_event(&f.s, &f.id, source, &old, &policy, false).unwrap();
    let mut response = fresh_result(&f, vec![old]);
    response["actions"] = json!([update]);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        response.to_string(),
    )
    .unwrap();
    for _ in 0..2 {
        assert!(cumulative_parts(&f).await.is_empty());
    }
    assert!(states(&f).iter().all(|state| state == "observed"));
    assert!(calls(&f, "decision").is_empty());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_awareness_appends WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
