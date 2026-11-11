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

struct LaterRequest {
    f: F,
    headers: HeaderMap,
    source: String,
    forwarding: Option<String>,
    peer: String,
    arguments: Value,
}

// Exercise the real call-owned forward route and authenticated peer post route.
// Only the native callback's already-active state is a fixture: no model runs.
async fn later_request(forward: bool) -> LaterRequest {
    let (f, headers) = prepared(false);
    let source = uuid::Uuid::new_v4().to_string();
    let at = now_ms();
    {
        let db = f.s.db.lock().unwrap();
        db.execute(
            "UPDATE messages SET status='complete' WHERE id='peer-target'",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE input_deliveries SET state='completed' WHERE target_turn='peer-target'",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES('later-browser','owner','csrf',?1)", [at+60_000]).unwrap();
        let mut speech = json!({"role":"user","event_id":"later-speech","text":"Actually there are four people. Please check again.","start_ms":20,"end_ms":40});
        db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'later-speech','transcript',?2,?3)",params![f.call,speech.to_string(),at]).unwrap();
        speech["seq"] = json!(db.last_insert_rowid());
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','Actually there are four people. Please check again.','complete',?3,?3,'owner')",params![source,f.parent,at]).unwrap();
        db.execute(
            "INSERT INTO input_authority(message_id,browser_hash) VALUES(?1,'later-browser')",
            [&source],
        )
        .unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('later-human-target',?1,'assistant','','streaming',?2,?2,'Lead','default')",params![f.parent,at]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES('later-human-input',?1,?2,'default','direct','submitted','later-human-target',?3)",params![source,f.parent,at]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,source,request_text) VALUES(?1,'later-delegation',40,'dispatched',?2,?3,?4,'Actually there are four people. Please check again.')",params![f.call,source,at,json!({"version":1,"fragments":[speech]}).to_string()]).unwrap();
    }
    let forwarding = if forward {
        let request = uuid::Uuid::new_v4().to_string();
        let args = json!({"installation_id":"lead-install","huddle_id":f.room,"delegation_id":"later-delegation","request_id":request,"text":"The caller corrected the party size to four. Please check again.","target_agent_ids":["peer"],"kind":"message"});
        assert_eq!(
            permission(&f, "owner", "companion_huddle_post", args.clone())["outcome"]["optionId"],
            "once"
        );
        let mut body = args;
        body.as_object_mut().unwrap().remove("huddle_id");
        let result = huddles::post_message(
            State(f.s.clone()),
            headers.clone(),
            Path((f.call.clone(), f.room.clone())),
            Json(serde_json::from_value(body).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(result.0["source"]["id"], source);
        Some(request)
    } else {
        None
    };
    {
        let db = f.s.db.lock().unwrap();
        db.execute(
            "UPDATE input_deliveries SET state='completed' WHERE id='later-human-input'",
            [],
        )
        .unwrap();
        db.execute("UPDATE companion_delegations SET state='completed' WHERE call_id=?1 AND id='later-delegation'",[&f.call]).unwrap();
        db.execute(
            "UPDATE messages SET status='complete' WHERE id='later-human-target'",
            [],
        )
        .unwrap();
    }
    // Hold the fixture's worker slot while recording the canonical peer ingress.
    let (tx, _rx) = tokio::sync::watch::channel(false);
    f.s.running.lock().unwrap().insert(f.parent.clone(), tx);
    let peer = uuid::Uuid::new_v4().to_string();
    let mut peer_headers = HeaderMap::new();
    peer_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", f.token)).unwrap(),
    );
    let (_, Json(receipt)) = crate::huddles::post(State(f.s.clone()),peer_headers,Path(f.room.clone()),Json(serde_json::from_value(json!({"request_id":peer,"text":"For four people and two cups, bring two more cups. The jug volume is still unknown.","target_agent_ids":["lead"],"kind":"message","source":null})).unwrap())).await.unwrap();
    assert_eq!(receipt["source"], Value::Null);
    assert_eq!(receipt["return"]["state"], "not_requested");
    assert_eq!(receipt["local_delivery"][0]["state"], "received");
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('later-peer-target',?1,'assistant','','streaming',?2,?2,'Lead','default')",params![f.parent,now_ms()]).unwrap();
        db.execute("UPDATE input_deliveries SET state='submitted',target_turn='later-peer-target' WHERE message_id=?1",[&peer]).unwrap();
    }
    let mut arguments = post_args(&f);
    arguments["delegation_id"] = json!("later-delegation");
    LaterRequest {
        f,
        headers,
        source,
        forwarding,
        peer,
        arguments,
    }
}

async fn later_outcome(r: &LaterRequest) -> ApiResult<Json<Value>> {
    let mut body = r.arguments.clone();
    body.as_object_mut().unwrap().remove("huddle_id");
    huddles::post_message(
        State(r.f.s.clone()),
        r.headers.clone(),
        Path((r.f.call.clone(), r.f.room.clone())),
        Json(serde_json::from_value(body).unwrap()),
    )
    .await
}

#[tokio::test]
async fn later_completed_forwarded_request_allows_peer_outcome_with_original_room_origin() {
    let r = later_request(true).await;
    let before_origin: String =
        r.f.s
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT origin FROM huddles WHERE room_id=?1",
                [&r.f.room],
                |row| row.get(0),
            )
            .unwrap();
    let allowed = permission(&r.f, "Peer", "companion_huddle_post", r.arguments.clone());
    let outcome = later_outcome(&r).await;
    assert_eq!(
        allowed["outcome"]["optionId"], "once",
        "The actual later caller request was forwarded to this active peer; HTTP outcome: {outcome:?}"
    );
    let result = outcome.unwrap().0;
    assert_eq!(result["return"]["state"], "stored");
    assert_eq!(result["source"]["id"], r.source);
    assert_eq!(result["origin"]["message_id"], r.f.source);
    assert_eq!(later_outcome(&r).await.unwrap().0, result);
    let db = r.f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT origin FROM huddles WHERE room_id=?1",
            [&r.f.room],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        before_origin
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM companion_events WHERE kind='huddle_return'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM companion_events WHERE kind='card'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM input_authority", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM companion_delegations WHERE state='completed'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT json_extract(payload,'$.source') FROM huddle_posts WHERE message_id=?1",
            [&r.peer],
            |row| row.get::<_, Option<String>>(0)
        )
        .unwrap(),
        None
    );
}

#[tokio::test]
async fn later_peer_outcome_requires_exact_prior_forward_and_both_live_human_authorities() {
    for change in [
        "unforwarded",
        "wrong_room",
        "wrong_peer",
        "wrong_revision",
        "wrong_time",
        "wrong_parent",
        "not_prior",
        "original_revoked",
        "later_revoked",
        "later_not_completed",
        "later_source_changed",
    ] {
        let r = later_request(change != "unforwarded").await;
        let other_room = if change == "wrong_room" {
            Some(
                r.f.s
                    .create_room("Unrelated coordination", "owner", &[])
                    .unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        } else {
            None
        };
        {
            let db = r.f.s.db.lock().unwrap();
            match change {
                "unforwarded" => {}
                "wrong_room" => {
                    db.execute("INSERT INTO huddles(room_id,request_id,purpose,origin,fingerprint) SELECT ?1,?2,purpose,origin,'other-room' FROM huddles WHERE room_id=?3",params![other_room,uuid::Uuid::new_v4().to_string(),r.f.room]).unwrap();
                    db.execute(
                        "UPDATE huddle_posts SET room_id=?2 WHERE message_id=?1",
                        params![r.forwarding, other_room],
                    )
                    .unwrap();
                    db.execute(
                        "UPDATE messages SET conversation_id=?2 WHERE id=?1",
                        params![r.forwarding, other_room],
                    )
                    .unwrap();
                }
                "wrong_peer" => {
                    db.execute("UPDATE huddle_posts SET payload=json_set(payload,'$.target_agent_ids',json('[\"some-other-peer\"]')) WHERE message_id=?1",[r.forwarding.as_ref().unwrap()]).unwrap();
                }
                "wrong_revision" => {
                    db.execute("UPDATE huddle_posts SET payload=json_set(payload,'$.source.revision','different') WHERE message_id=?1",[r.forwarding.as_ref().unwrap()]).unwrap();
                }
                "wrong_time" => {
                    db.execute("UPDATE huddle_posts SET payload=json_set(payload,'$.source.occurred_at',1) WHERE message_id=?1",[r.forwarding.as_ref().unwrap()]).unwrap();
                }
                "wrong_parent" => {
                    db.execute("UPDATE huddle_posts SET payload=json_set(payload,'$.source.conversation_id','other-parent') WHERE message_id=?1",[r.forwarding.as_ref().unwrap()]).unwrap();
                }
                "not_prior" => {
                    db.execute(
                        "UPDATE messages SET seq=(SELECT max(seq)+1 FROM messages) WHERE id=?1",
                        [r.forwarding.as_ref().unwrap()],
                    )
                    .unwrap();
                }
                "original_revoked" => {
                    db.execute(
                        "UPDATE browser_sessions SET expires_at=1 WHERE hash='browser'",
                        [],
                    )
                    .unwrap();
                }
                "later_revoked" => {
                    db.execute(
                        "UPDATE browser_sessions SET expires_at=1 WHERE hash='later-browser'",
                        [],
                    )
                    .unwrap();
                }
                "later_not_completed" => {
                    db.execute(
                        "UPDATE input_deliveries SET state='unknown' WHERE id='later-human-input'",
                        [],
                    )
                    .unwrap();
                }
                "later_source_changed" => {
                    db.execute(
                        "UPDATE messages SET text='Changed saved request' WHERE id=?1",
                        [&r.source],
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
        }
        let before: i64 =
            r.f.s
                .db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM huddle_posts", [], |row| row.get(0))
                .unwrap();
        assert_eq!(
            permission(&r.f, "Peer", "companion_huddle_post", r.arguments.clone())["outcome"]["optionId"],
            "deny",
            "{change}"
        );
        assert!(later_outcome(&r).await.is_err(), "{change}");
        let db = r.f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM huddle_posts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            before,
            "{change}"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM companion_events WHERE kind IN ('card','huddle_return')",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0,
            "{change}"
        );
    }
}

#[tokio::test]
async fn later_peer_outcome_rechecks_each_original_or_later_token_authority() {
    for original in [true, false] {
        let r = later_request(true).await;
        let source = if original { &r.f.source } else { &r.source };
        {
            let db = r.f.s.db.lock().unwrap();
            db.execute("INSERT INTO tokens(hash,id,scopes,expires_at) VALUES('source-token','source-admin','[\"admin\"]',?1)",[now_ms()+60_000]).unwrap();
            db.execute("UPDATE input_authority SET browser_hash=NULL,token_hash='source-token' WHERE message_id=?1",[source]).unwrap();
        }
        assert_eq!(
            permission(&r.f, "Peer", "companion_huddle_post", r.arguments.clone())["outcome"]["optionId"],
            "once"
        );
        r.f.s
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM tokens WHERE hash='source-token'", [])
            .unwrap();
        assert_eq!(
            permission(&r.f, "Peer", "companion_huddle_post", r.arguments.clone())["outcome"]["optionId"],
            "deny"
        );
        assert!(later_outcome(&r).await.is_err());
    }
}
