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

#[tokio::test]
async fn complete_multiline_private_reply_has_ordered_exact_body_parts_and_full_ack() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut update = cumulative_private_outcome(&f, &request, &queue);
    let body = format!(
        "{}\n{}\nThe measurement is 137 cm; this is an observation, not independent proof of completion.",
        "Exact Unicode café — source paragraph. ".repeat(6),
        "A second intact source paragraph remains available. ".repeat(5)
    );
    assert!(body.len() > 480);
    update["source_replies"][0]["text"] = json!(body);
    cumulative_response(&f, &update);
    let first = cumulative_parts(&f).await;
    let spoken: Vec<_> = first
        .iter()
        .filter(|part| part["channel"] == "commentary")
        .collect();
    assert!(
        spoken.len() > 1,
        "A bounded multiline reply must not be discarded at 480 bytes"
    );
    assert_eq!(
        spoken
            .iter()
            .map(|part| part["content"].as_str().unwrap())
            .collect::<String>(),
        body
    );
    assert!(
        spoken
            .iter()
            .all(|part| part["content"].as_str().unwrap().len() <= 480)
    );
    accept_cumulative_parts(&f, &first);
    assert_eq!(states(&f), vec!["context_accepted"]);
    update["revision"] = json!("after-complete-multipart");
    update["responses"] = json!([{"id":"next","audience":"public","at":timestamp(now_ms()),"text":"A genuinely new source fact."}]);
    cumulative_response(&f, &update);
    let next = cumulative_parts(&f).await;
    assert!(next.iter().all(|part| part["channel"] != "commentary"));
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(update)
    );
}

fn multipart_body() -> String {
    format!(
        "{}\n{}\nThe measurement is 137 cm; this is a recorded observation, not independent proof of completion.",
        "Exact Unicode café — source paragraph. ".repeat(6),
        "A second intact source paragraph remains available. ".repeat(5)
    )
}

fn spoken_body(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| p["channel"] == "commentary")
        .map(|p| p["content"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn multipart_reply_requires_all_ordered_parts_and_keeps_frozen_unknown_batches() {
    for mode in [
        "all",
        "missing-last-ack",
        "missing-metadata-ack",
        "unknown",
        "reordered",
        "missing-command",
        "duplicate-command",
        "wrong-metadata",
        "split-batch",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = cumulative_private_outcome(&f, &request, &queue);
        update["source_replies"][0]["text"] = json!(multipart_body());
        cumulative_response(&f, &update);
        let first = cumulative_parts(&f).await;
        let metadata = first
            .iter()
            .position(|p| {
                p["content"]
                    .as_str()
                    .unwrap()
                    .contains("\"kind\":\"private_source_reply\"")
            })
            .unwrap();
        let last = first
            .iter()
            .rposition(|p| p["channel"] == "commentary")
            .unwrap();
        assert_eq!(spoken_body(&first), multipart_body());
        for (index, command) in first.iter().enumerate() {
            if (mode == "missing-last-ack" && index == last)
                || (mode == "missing-metadata-ack" && index == metadata)
            {
                continue;
            }
            if mode == "unknown" && index == last {
                provider_event(&f.s,&f.id,&json!({"type":"error","error":{"type":"server_error","client_event_id":command["event_id"]}})).unwrap();
            } else {
                accept_cumulative_parts(&f, std::slice::from_ref(command));
            }
        }
        let batch: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT id FROM companion_voice_batches WHERE call_id=?1",
                    [&f.id],
                    |r| r.get(0),
                )
                .unwrap();
        let mut retained = first.clone();
        match mode {
            "reordered" => retained.swap(metadata + 1, last),
            "missing-command" => {
                retained.remove(last);
            }
            "duplicate-command" => {
                retained[last]["content"] = retained[metadata + 1]["content"].clone()
            }
            "wrong-metadata" => {
                retained[metadata]["content"] = json!("Different private source qualification.")
            }
            "split-batch" => {
                let tail = retained.split_off(last);
                f.s.db.lock().unwrap().execute("INSERT INTO companion_voice_batches SELECT 'split-proof',call_id,semantic_key,?2,state,created_at+1,updated_at FROM companion_voice_batches WHERE id=?1",params![batch,serde_json::to_string(&tail).unwrap()]).unwrap();
            }
            _ => {}
        }
        let frozen = serde_json::to_string(&retained).unwrap();
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_voice_batches SET updates=?2 WHERE id=?1",
                params![batch, frozen],
            )
            .unwrap();
        assert!(
            cumulative_parts(&f).await.is_empty(),
            "same frozen event must not retry: {mode}"
        );
        update["revision"] = json!(format!("next-{mode}"));
        update["responses"] = json!([{"id":"new","audience":"public","at":timestamp(now_ms()),"text":"An independent new fact is available."}]);
        cumulative_response(&f, &update);
        let next = cumulative_parts(&f).await;
        assert_eq!(
            spoken_body(&next),
            if mode == "all" {
                String::new()
            } else {
                multipart_body()
            },
            "{mode}"
        );
        assert!(
            next.iter()
                .all(|p| first.iter().all(|old| old["event_id"] != p["event_id"]))
        );
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT updates FROM companion_voice_batches WHERE id=?1",
                    [&batch],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            frozen
        );
        assert_eq!(
            action_observation(&f.s, &f.id, &request).unwrap(),
            Some(update)
        );
        if mode == "unknown" {
            assert!(states(&f).contains(&"unknown".to_owned()));
        }
    }
}

#[tokio::test]
async fn multipart_same_snapshot_is_silent_but_changed_qualifications_and_corrections_remain() {
    for mode in [
        "revision-only",
        "blocker",
        "unknown",
        "authority",
        "coverage",
        "body",
        "time",
        "association",
        "policy",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = cumulative_private_outcome(&f, &request, &queue);
        update["source_replies"][0]["text"] = json!(multipart_body());
        cumulative_response(&f, &update);
        let first = cumulative_parts(&f).await;
        accept_cumulative_parts(&f, &first);
        assert_eq!(states(&f), vec!["context_accepted"]);
        let original = update.clone();
        update["revision"] = json!(format!("same-body-{mode}"));
        update["observed_at"] = json!(timestamp(now_ms() + 1));
        match mode {
            "blocker" => update["conversation"]["blocked_reason"] = json!("new_recorded_blocker"),
            "unknown" => {
                update["delivery_state"] = json!("unknown");
                update["state"] = json!("unknown");
            }
            "authority" => {
                update["task_authorizations"] =
                    json!([{"operation":"amend","task_id":"original-task","authority_revision":2}])
            }
            "coverage" => update["domain_source_replies_omitted"] = json!(true),
            "body" => {
                update["source_replies"][0]["text"] =
                    json!(multipart_body().replace("137 cm", "138 cm"))
            }
            "time" => update["source_replies"][0]["at"] = json!(timestamp(now_ms() + 1)),
            "association" => update["domain_response_association"] = json!("shared_turn"),
            "policy" => rewrite_policy(&f, |p| p["guidance"] = json!("Updated source guidance.")),
            _ => {}
        }
        cumulative_response(&f, &update);
        let next = cumulative_parts(&f).await;
        if mode == "revision-only" {
            assert!(
                next.is_empty(),
                "same whole accepted reply and qualifiers must not create a new batch"
            );
            assert_eq!(
                f.s.db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
                        [&f.id],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            assert!(states(&f).contains(&"context_accepted".to_owned()));
            assert!(states(&f).contains(&"observed".to_owned()));
        } else {
            assert!(!next.is_empty(), "{mode}");
            let content = cumulative_text(&next);
            let marker = match mode {
                "blocker" => "new_recorded_blocker",
                "unknown" => "outcome remains unknown",
                "authority" => "amendment",
                "coverage" => "incomplete",
                _ => "private_source_reply",
            };
            assert!(content.contains(marker), "{mode}: {content}");
            if matches!(mode, "body" | "time" | "association" | "policy") {
                assert!(!spoken_body(&next).is_empty(), "{mode}");
            } else {
                assert!(
                    spoken_body(&next).is_empty(),
                    "unchanged accepted body: {mode}"
                );
            }
            accept_cumulative_parts(&f, &next);
        }
        assert_eq!(
            action_observation(&f.s, &f.id, &request).unwrap(),
            Some(update)
        );
        if mode == "body" {
            let mut reverted = original;
            reverted["revision"] = json!("corrected-back-to-original");
            cumulative_response(&f, &reverted);
            assert_eq!(spoken_body(&cumulative_parts(&f).await), multipart_body());
        }
    }
}

#[tokio::test]
async fn whole_private_reply_overflow_never_selects_partial_paragraphs() {
    for mode in ["unit", "batch", "ambiguous", "metadata"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = cumulative_private_outcome(&f, &request, &queue);
        let body = match mode {
            "unit" => format!(
                "{}\nDO NOT PRESENT ONLY THIS LAST QUALIFICATION",
                "é".repeat(241)
            ),
            "batch" => format!("{}\n", "é".repeat(188)).repeat(31),
            _ => multipart_body(),
        };
        update["source_replies"][0]["text"] = json!(body);
        if mode == "ambiguous" {
            let mut duplicate = update["source_replies"][0].clone();
            duplicate["id"] = json!("separate-private-id");
            update["source_replies"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
        }
        if mode == "metadata" {
            update["source_replies"][0]["id"] = json!("x".repeat(160));
        }
        assert!(update.to_string().len() <= 16000, "{mode}");
        cumulative_response(&f, &update);
        let parts = cumulative_parts(&f).await;
        assert!(!parts.is_empty(), "{mode}");
        assert!(parts.len() <= 32);
        assert!(parts.iter().all(|p| p["channel"] == "thinking"), "{mode}");
        let text = cumulative_text(&parts);
        assert!(
            text.contains("Some returned private replies are not included"),
            "{mode}: {text}"
        );
        assert!(!text.contains("DO NOT PRESENT ONLY"), "{mode}");
        assert!(!text.contains("No public reply is included"), "{mode}");
        assert_eq!(
            action_observation(&f.s, &f.id, &request).unwrap(),
            Some(update.clone())
        );
        accept_cumulative_parts(&f, &parts);
        update["revision"] = json!("after-unavailable-notice");
        cumulative_response(&f, &update);
        assert!(
            !cumulative_parts(&f).await.is_empty(),
            "an omission notice is not whole-reply acceptance: {mode}"
        );
    }
}

#[test]
fn private_reply_split_preserves_newlines_unicode_and_repeated_chunks() {
    let unit = format!("{}\r\n", "é".repeat(237));
    let body = format!("{unit}{unit}{unit}Final qualification.\n");
    let parts = whole_reply_parts(&body).unwrap();
    assert_eq!(parts.concat(), body);
    assert!(parts.iter().all(|p| p.len() <= 480));
    assert_eq!(parts[0], parts[1]);
    let reply = PrivateReply {
        identity: "id".into(),
        fact: "fact".into(),
        id: "reply".into(),
        body: body.clone(),
        parts: parts.clone(),
        facts: "qualified".into(),
    };
    let mut commands = vec![(
        "batch".into(),
        json!({"op":"append","channel":"thinking","content":"qualified","event_id":"facts"}),
    )];
    commands.extend(parts.iter().enumerate().map(|(i,part)|("batch".into(),json!({"op":"append","channel":"commentary","content":part,"event_id":format!("part-{i}")}))));
    assert!(private_reply_accepted(&reply, &commands));
    commands[2] = commands[1].clone();
    assert!(
        !private_reply_accepted(&reply, &commands),
        "one accepted event cannot prove two identical body parts"
    );
    assert!(whole_reply_parts(&format!("{}\nEnd.", "é".repeat(241))).is_none());
}

#[tokio::test]
async fn qualifier_reversion_cannot_reuse_an_older_whole_snapshot_acceptance() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut a = cumulative_private_outcome(&f, &request, &queue);
    a["source_replies"][0]["text"] = json!(multipart_body());
    a["conversation"]["blocked_reason"] = Value::Null;
    a["revision"] = json!("qualifier-a");
    cumulative_response(&f, &a);
    accept_cumulative_parts(&f, &cumulative_parts(&f).await);
    let mut b = a.clone();
    b["revision"] = json!("qualifier-b");
    b["conversation"]["blocked_reason"] = json!("intervening_recorded_blocker");
    cumulative_response(&f, &b);
    let changed = cumulative_parts(&f).await;
    assert!(
        spoken_body(&changed).is_empty(),
        "same reply already accepted"
    );
    assert!(cumulative_text(&changed).contains("intervening_recorded_blocker"));
    accept_cumulative_parts(&f, &changed);
    let mut c = a.clone();
    c["revision"] = json!("qualifier-c-back-to-a");
    cumulative_response(&f, &c);
    let restored = cumulative_parts(&f).await;
    assert!(
        !restored.is_empty(),
        "A differing newer substantive observation must prevent reuse of older A's whole-snapshot acceptance"
    );
    assert!(
        spoken_body(&restored).is_empty(),
        "the exact original reply remains accepted"
    );
    assert!(!cumulative_text(&restored).contains("intervening_recorded_blocker"));
    assert_eq!(action_observation(&f.s, &f.id, &request).unwrap(), Some(c));
}
