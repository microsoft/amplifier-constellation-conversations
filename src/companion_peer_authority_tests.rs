use super::*;
use crate::huddle_delivery::tests::{F, fixture, queued};
use axum::http::{HeaderValue, header};

fn prepared(same_label: bool) -> (F, HeaderMap) {
    let f = fixture();
    std::fs::write(
        f.s.home.join("companion-actions.json"),
        r#"{"enabled":true}"#,
    )
    .unwrap();
    {
        let db = f.s.db.lock().unwrap();
        db.execute(
            "INSERT INTO companion_tool_grants(call_id,token,port) VALUES(?1,'call-token',12345)",
            [&f.call],
        )
        .unwrap();
        if same_label {
            db.execute("UPDATE huddle_installations SET definition=json_set(definition,'$.label','owner') WHERE id='peer-install'", []).unwrap();
            db.execute(
                "UPDATE huddle_participants SET label='owner' WHERE agent_id='peer'",
                [],
            )
            .unwrap();
        }
        let mut speech = json!({"role":"user","event_id":"original-speech","text":"Ask the peer to review.","start_ms":1,"end_ms":10});
        db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'original-speech','transcript',?2,?3)", params![f.call,speech.to_string(),now_ms()]).unwrap();
        speech["seq"] = json!(db.last_insert_rowid());
        db.execute("UPDATE companion_delegations SET source=?2 WHERE call_id=?1 AND id='original-delegation'",params![f.call,json!({"version":1,"fragments":[speech]}).to_string()]).unwrap();
    }
    let (_, input) = queued(&f);
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('peer-target',?1,'assistant','','streaming',1,1,'Lead','default')", [&f.parent]).unwrap();
        db.execute(
            "UPDATE input_deliveries SET state='submitted',target_turn='peer-target' WHERE id=?1",
            [input],
        )
        .unwrap();
    }
    let mut h = HeaderMap::new();
    h.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer call-token"),
    );
    (f, h)
}
fn permission(f: &F, actor: &str, tool: &str, arguments: Value) -> Value {
    permission_once(&f.s,&f.parent,actor,"default",true,&json!({
        "toolCall":{"toolCallId":"permission-test","title":format!("companion / {tool}"),"kind":"other","status":"pending","rawInput":{"server":"companion","tool":tool,"arguments":arguments}},
        "options":[{"optionId":"once","name":"Allow once","kind":"allow_once"},{"optionId":"deny","name":"Deny once","kind":"reject_once"}]
    })).unwrap().unwrap()
}
fn post_args(f: &F) -> Value {
    json!({"installation_id":"lead-install","huddle_id":f.room,"delegation_id":"original-delegation","request_id":uuid::Uuid::new_v4().to_string(),"text":"The peer reviewed the cups; water capacity is still unknown.","target_agent_ids":[],"kind":"outcome","outcome":{"kind":"result","evidence":[]}})
}
#[tokio::test]
async fn admitted_peer_posts_outcome_to_original_call_without_reopening_human_delegation() {
    let (f, h) = prepared(false);
    let mut args = post_args(&f);
    assert_eq!(
        permission(&f, "Peer", "companion_huddle_post", args.clone())["outcome"]["optionId"],
        "once"
    );
    args.as_object_mut().unwrap().remove("huddle_id");
    let result = huddles::post_message(
        State(f.s.clone()),
        h,
        Path((f.call.clone(), f.room.clone())),
        Json(serde_json::from_value(args).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(result.0["return"]["state"], "stored");
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM companion_events WHERE call_id=?1 AND kind='huddle_return'",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM companion_events WHERE call_id=?1 AND kind='card'",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let state: String = db
        .query_row(
            "SELECT state FROM companion_delegations WHERE call_id=?1 AND id='original-delegation'",
            [&f.call],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "completed");
    assert_eq!(
        db.query_row("SELECT count(*) FROM input_authority", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn peer_post_is_bound_to_exact_room_installation_source_and_live_membership() {
    let (f, _) = prepared(false);
    let original = post_args(&f);
    for (field, value) in [
        ("huddle_id", uuid::Uuid::new_v4().to_string()),
        ("installation_id", "peer-install".into()),
        ("delegation_id", "different-delegation".into()),
    ] {
        let mut args = original.clone();
        args[field] = json!(value);
        assert_eq!(
            permission(&f, "Peer", "companion_huddle_post", args)["outcome"]["optionId"],
            "deny"
        );
    }
    assert_eq!(
        permission(
            &f,
            "Peer",
            "companion_huddle_ensure",
            json!({"installation_id":"lead-install","request_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"original-delegation","title":"New room","purpose":"new work","peer_installation_ids":[]})
        )["outcome"]["optionId"],
        "deny"
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE huddle_auto_members SET revoked=1 WHERE installation_id='peer-install'",
            [],
        )
        .unwrap();
    assert_eq!(
        permission(&f, "Peer", "companion_huddle_post", original)["outcome"]["optionId"],
        "deny"
    );
}
#[tokio::test]
async fn same_label_peer_and_revoked_peer_cannot_borrow_call_effects_in_native_or_http_tools() {
    for same_label in [false, true] {
        let (f, h) = prepared(same_label);
        let actor = if same_label { "owner" } else { "Peer" };
        assert_eq!(
            permission(&f, actor, "companion_connections", json!({}))["outcome"]["optionId"],
            "once"
        );
        assert!(
            connections(State(f.s.clone()), Path(f.call.clone()), h.clone())
                .await
                .is_ok()
        );
        for revoked in [false, true] {
            if revoked {
                f.s.db.lock().unwrap().execute("UPDATE huddle_installations SET token_hash='rotated' WHERE id='peer-install'",[]).unwrap();
            }
            let show_args = json!({"title":"Unexpected card","text":"Not authorized by a peer"});
            assert_eq!(
                permission(&f, actor, "companion_show", show_args.clone())["outcome"]["optionId"],
                "deny"
            );
            assert!(
                show(
                    State(f.s.clone()),
                    Path(f.call.clone()),
                    h.clone(),
                    Json(serde_json::from_value(show_args).unwrap())
                )
                .await
                .is_err()
            );
            assert!(action(State(f.s.clone()),Path(f.call.clone()),h.clone(),Json(serde_json::from_value(json!({"connection_id":"game","instruction":"Do work","request_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"original-delegation"})).unwrap())).await.is_err());
            assert!(crate::companion_surface::hangup(State(f.s.clone()),Path(f.call.clone()),h.clone(),Json(serde_json::from_value(json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"original-delegation"})).unwrap())).await.is_err());
        }
        let db = f.s.db.lock().unwrap();
        for table in ["companion_actions", "companion_surface_commands"] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM companion_events WHERE kind='card'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
#[test]
fn only_incorporated_human_steering_lifts_quiet_peer_presentation_restriction() {
    let (f, h) = prepared(false);
    let args = json!({"title":"Answer for caller","text":"A human correction was incorporated."});
    for state in [
        "received",
        "submitted",
        "remoteQueued",
        "unknown",
        "notAccepted",
        "cancelled",
        "incorporated",
    ] {
        f.s.db.lock().unwrap().execute("UPDATE input_deliveries SET target_turn='peer-target',state=?1 WHERE id='original-input'",[state]).unwrap();
        let allowed = state == "incorporated";
        assert_eq!(
            authorize_effect(&f.s, &f.call, &h).is_ok(),
            allowed,
            "{state}"
        );
        assert_eq!(
            permission(&f, "Peer", "companion_show", args.clone())["outcome"]["optionId"],
            if allowed { "once" } else { "deny" },
            "{state}"
        );
    }
}

#[test]
fn incorporated_human_action_uses_exact_human_authority_not_initial_peer_label() {
    for same_label in [false, true] {
        let (f, _) = prepared(same_label);
        std::fs::write(f.s.home.join("companion-actions.json"), json!({"enabled":true,"agents":{"default":[{"id":"fixture","label":"Test","description":"Never invoked by permission check","command":["/bin/false"]}]}}).to_string()).unwrap();
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_delegations SET state='dispatched' WHERE call_id=?1",
                [&f.call],
            )
            .unwrap();
        let actor = if same_label { "owner" } else { "Peer" };
        let args = json!({"connection_id":"fixture","instruction":"Do the human's requested action.","request_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"original-delegation"});
        for state in ["submitted", "unknown", "incorporated"] {
            f.s.db.lock().unwrap().execute("UPDATE input_deliveries SET target_turn='peer-target',state=?1 WHERE id='original-input'",[state]).unwrap();
            assert_eq!(
                permission(&f, actor, "companion_request_action", args.clone())["outcome"]["optionId"],
                if state == "incorporated" {
                    "once"
                } else {
                    "deny"
                },
                "{same_label}/{state}"
            );
        }
        let mut wrong = args.clone();
        wrong["delegation_id"] = json!("unrelated-human-request");
        assert_eq!(
            permission(&f, actor, "companion_request_action", wrong)["outcome"]["optionId"],
            "deny"
        );
        f.s.db
            .lock()
            .unwrap()
            .execute("UPDATE browser_sessions SET expires_at=1", [])
            .unwrap();
        assert_eq!(
            permission(&f, actor, "companion_request_action", args)["outcome"]["optionId"],
            "deny"
        );
    }
}
