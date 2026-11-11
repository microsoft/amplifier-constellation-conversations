//! Exact human request binding for call-owned huddle tools. A delegation ID is
//! a retained lookup key, not authority supplied by a model. The caller checks
//! the MCP credential, then repeats this resolver inside the shared mutation's
//! transaction so call closure, credential revocation and steering cannot race
//! the operation. Never select a request by recency or matching text.
use crate::{
    error::{ApiError, ApiResult},
    huddles::Origin,
    state::AppState,
};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn resolve_in(
    s: &AppState,
    db: &Connection,
    call: &str,
    owner: &str,
    agent: &str,
    parent: &str,
    delegation: &str,
) -> ApiResult<Origin> {
    // This also validates the stored endpoint against its current configuration
    // and exact private membership without taking AppState's DB lock again.
    let identity =
        crate::companion_history::private_identity(db, call)?.ok_or_else(ApiError::forbidden)?;
    if identity.owner != owner || identity.agent != agent || identity.room != parent {
        return Err(ApiError::forbidden());
    }
    let source: Option<String> = db
        .query_row(
            "SELECT d.message_id FROM companion_delegations d
             JOIN companion_calls c ON c.id=d.call_id
             JOIN messages m ON m.id=d.message_id
             WHERE d.call_id=?1 AND d.id=?2 AND d.state='dispatched'
               AND c.owner=?3 AND c.agent_id=?4 AND c.conversation_id=?5
               AND c.state IN ('connecting','connected')
               AND m.conversation_id=?5 AND m.actor=?3
               AND m.role='user' AND m.status='complete'
               AND EXISTS(SELECT 1 FROM input_deliveries i
                   WHERE i.message_id=m.id AND i.conversation_id=?5 AND i.agent_id=?4
                     AND i.state IN ('submitted','received_remote','incorporated'))",
            params![call, delegation, owner, agent, parent],
            |r| r.get(0),
        )
        .optional()?;
    let source = source.ok_or_else(|| ApiError::bad("An active caller request is required"))?;
    if !crate::delivery::authorized_in(s, db, &source, owner)? {
        return Err(ApiError::forbidden());
    }
    // Validate retained actual speech, including exact fragment IDs/content.
    // Legacy absence remains absence. Its earlier speech time does not replace
    // the shared Origin's saved-message time or raw-wrapper digest namespace.
    crate::companion_awareness::original_human_source_time(db, call, delegation)
        .map_err(|_| ApiError::bad("The original human source cannot be verified"))?;
    crate::huddles::parent_source(db, parent, &source, owner)
}

/// An admitted peer reply can continue its existing discussion after the
/// caller's original turn completes. It is not another human delegation and
/// cannot authorize creating rooms or changing their membership. The delivery
/// binding supplies the room and recipient, never a model-provided authority.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_post_in(
    s: &AppState,
    db: &Connection,
    call: &str,
    owner: &str,
    agent: &str,
    parent: &str,
    delegation: &str,
    room: &str,
    installation: &str,
) -> ApiResult<Origin> {
    if let Ok(origin) = resolve_in(s, db, call, owner, agent, parent, delegation) {
        return Ok(origin);
    }
    let identity =
        crate::companion_history::private_identity(db, call)?.ok_or_else(ApiError::forbidden)?;
    if identity.owner != owner || identity.agent != agent || identity.room != parent {
        return Err(ApiError::forbidden());
    }
    let source: Option<String> = db
        .query_row(
            "SELECT d.message_id FROM companion_delegations d
         JOIN companion_calls c ON c.id=d.call_id JOIN messages m ON m.id=d.message_id
         WHERE d.call_id=?1 AND d.id=?2 AND d.state IN ('dispatched','completed')
           AND c.owner=?3 AND c.agent_id=?4 AND c.conversation_id=?5
           AND c.state IN ('connecting','connected')
           AND m.conversation_id=?5 AND m.actor=?3 AND m.role='user' AND m.status='complete'
           AND EXISTS(SELECT 1 FROM input_deliveries i WHERE i.message_id=m.id
             AND i.conversation_id=?5 AND i.agent_id=?4 AND i.state='completed')",
            params![call, delegation, owner, agent, parent],
            |r| r.get(0),
        )
        .optional()?;
    let source = source.ok_or_else(|| {
        ApiError::bad("An active caller request or admitted peer continuation is required")
    })?;
    if !crate::huddle_delivery::active_source(
        s,
        db,
        call,
        parent,
        agent,
        &source,
        room,
        installation,
    )? || !crate::delivery::authorized_in(s, db, &source, owner)?
    {
        return Err(ApiError::forbidden());
    }
    crate::companion_awareness::original_human_source_time(db, call, delegation)
        .map_err(|_| ApiError::bad("The original human source cannot be verified"))?;
    crate::huddles::parent_source(db, parent, &source, owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth,
        config::{Agent, Config},
        state::now_ms,
    };
    use serde_json::json;
    use std::{collections::BTreeMap, sync::Arc};

    struct Fixture {
        _home: tempfile::TempDir,
        s: Arc<AppState>,
        call: String,
        parent: String,
        at: i64,
    }
    struct Request {
        delegation: String,
        message: String,
        raw: String,
        at: i64,
        speech_seq: i64,
    }
    fn fixture() -> Fixture {
        let home = tempfile::tempdir().unwrap();
        Config {
            id: uuid::Uuid::new_v4().to_string(),
            label: "Huddle source fixture".into(),
            owner: "owner".into(),
            allowed_users: vec![],
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
        let parent = s
            .create_room("Private call", "owner", &["default".into()])
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let call = uuid::Uuid::new_v4().to_string();
        let at = now_ms();
        let endpoint =
            auth::digest(&serde_json::to_string(&s.agent("default").unwrap().unwrap()).unwrap());
        {
            let db = s.db.lock().unwrap();
            db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)", params![call,uuid::Uuid::new_v4().to_string(),parent,at,endpoint]).unwrap();
            db.execute("INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES('browser','owner','csrf',?1)", [at+60_000]).unwrap();
        }
        Fixture {
            _home: home,
            s,
            call,
            parent,
            at,
        }
    }
    fn request(f: &Fixture, key: &str, raw: &str, state: &str) -> Request {
        let message = format!("message-{key}");
        let delegation = format!("delegation-{key}");
        let at = f.at + 100;
        let speech = json!({"role":"user","event_id":format!("speech-{key}"),"text":"Please ask the team.","start_ms":1,"end_ms":100});
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)", params![f.call,format!("speech-{key}"),speech.to_string(),f.at-1000]).unwrap();
        let speech_seq = db.last_insert_rowid();
        let mut fragment = speech;
        fragment["seq"] = json!(speech_seq);
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')", params![message,f.parent,raw,at]).unwrap();
        db.execute(
            "INSERT INTO input_authority(message_id,browser_hash) VALUES(?1,'browser')",
            [&message],
        )
        .unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES(?1,?2,?3,'default','direct',?4,'same-active-turn',?5)", params![format!("input-{key}"),message,f.parent,state,at]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,source,request_text) VALUES(?1,?2,100,'dispatched',?3,?4,?5,'Please ask the team.')", params![f.call,delegation,message,at+1000,json!({"version":1,"fragments":[fragment]}).to_string()]).unwrap();
        Request {
            delegation,
            message,
            raw: raw.into(),
            at,
            speech_seq,
        }
    }
    fn resolve(f: &Fixture, r: &Request) -> ApiResult<Origin> {
        let db = f.s.db.lock().unwrap();
        resolve_in(
            &f.s,
            &db,
            &f.call,
            "owner",
            "default",
            &f.parent,
            &r.delegation,
        )
    }
    fn expected(f: &Fixture, r: &Request) -> Origin {
        Origin::Conversation {
            conversation_id: f.parent.clone(),
            request_id: r.message.clone(),
            message_id: r.message.clone(),
            source_revision: auth::digest(&r.raw),
            occurred_at: r.at,
            actor: crate::huddles::OriginActor {
                id: "owner".into(),
                kind: "human".into(),
            },
        }
    }

    #[test]
    fn simultaneous_and_steered_requests_keep_exact_source_not_latest() {
        let f = fixture();
        let first = request(&f, "first", "First saved spoken wrapper", "submitted");
        let followup = request(
            &f,
            "followup",
            "Separate saved correction wrapper",
            "incorporated",
        );
        assert_eq!(resolve(&f, &first).unwrap(), expected(&f, &first));
        assert_eq!(resolve(&f, &followup).unwrap(), expected(&f, &followup));
        assert_ne!(
            resolve(&f, &first).unwrap(),
            resolve(&f, &followup).unwrap()
        );
        let db = f.s.db.lock().unwrap();
        assert!(resolve_in(&f.s, &db, &f.call, "owner", "default", &f.parent, "missing").is_err());
    }

    #[test]
    fn repeated_text_never_merges_two_original_message_identities() {
        let f = fixture();
        let a = request(&f, "a", "Identical repeated text", "submitted");
        let b = request(&f, "b", "Identical repeated text", "received_remote");
        let oa = resolve(&f, &a).unwrap();
        let ob = resolve(&f, &b).unwrap();
        assert_eq!(oa, expected(&f, &a));
        assert_eq!(ob, expected(&f, &b));
        assert_ne!(oa, ob);
    }

    #[test]
    fn canonical_raw_message_revision_and_time_survive_speech_validation() {
        let f = fixture();
        let r = request(
            &f,
            "canonical",
            "[Saved spoken request, with exact quoted fragments]",
            "submitted",
        );
        let db = f.s.db.lock().unwrap();
        let before = db.total_changes();
        let origin = resolve_in(
            &f.s,
            &db,
            &f.call,
            "owner",
            "default",
            &f.parent,
            &r.delegation,
        )
        .unwrap();
        assert_eq!(origin, expected(&f, &r));
        assert_eq!(
            origin,
            crate::huddles::parent_source(&db, &f.parent, &r.message, "owner").unwrap()
        );
        assert_eq!(
            crate::companion_awareness::original_human_source_time(&db, &f.call, &r.delegation)
                .unwrap(),
            Some(f.at - 1000)
        );
        assert_eq!(before, db.total_changes());
    }

    #[test]
    fn retained_speech_change_or_missing_evidence_rejects_binding() {
        for change in ["transcript", "fragment", "missing", "malformed", "empty"] {
            let f = fixture();
            let r = request(&f, "speech", "Retained source wrapper", "submitted");
            assert!(resolve(&f, &r).is_ok());
            {
                let db = f.s.db.lock().unwrap();
                match change {
                    "transcript" => {
                        db.execute("UPDATE companion_events SET data=json_set(data,'$.text','Altered actual speech') WHERE seq=?1", [r.speech_seq]).unwrap();
                    }
                    "fragment" => {
                        db.execute("UPDATE companion_delegations SET source=json_set(source,'$.fragments[0].text','Invented speech') WHERE id=?1", [&r.delegation]).unwrap();
                    }
                    "missing" => {
                        db.execute("DELETE FROM companion_events WHERE seq=?1", [r.speech_seq])
                            .unwrap();
                    }
                    "malformed" => {
                        db.execute(
                            "UPDATE companion_delegations SET source='not-json' WHERE id=?1",
                            [&r.delegation],
                        )
                        .unwrap();
                    }
                    "empty" => {
                        db.execute(
                            "UPDATE companion_delegations SET source=?2 WHERE id=?1",
                            params![
                                r.delegation,
                                json!({"version":1,"fragments":[]}).to_string()
                            ],
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            assert!(resolve(&f, &r).is_err(), "{change}");
        }
    }

    #[test]
    fn legacy_absent_speech_does_not_invent_fragments_or_new_time() {
        let f = fixture();
        let r = request(&f, "legacy", "Original legacy source", "incorporated");
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_delegations SET source=NULL WHERE id=?1",
                [&r.delegation],
            )
            .unwrap();
        assert_eq!(resolve(&f, &r).unwrap(), expected(&f, &r));
    }

    #[test]
    fn delivery_must_belong_to_exact_agent_parent_and_admitted_state() {
        for change in [
            "agent",
            "parent",
            "received",
            "unknown",
            "completed",
            "cancelled",
            "ack_pending",
            "remoteQueued",
        ] {
            let f = fixture();
            let r = request(&f, "delivery", "Saved source", "submitted");
            {
                let db = f.s.db.lock().unwrap();
                match change {
                    "agent" => {
                        db.execute("UPDATE input_deliveries SET agent_id='other-agent' WHERE message_id=?1", [&r.message]).unwrap();
                    }
                    "parent" => {
                        db.execute(
                            "UPDATE input_deliveries SET conversation_id=?1 WHERE message_id=?2",
                            params![other_room(&db), r.message],
                        )
                        .unwrap();
                    }
                    state => {
                        db.execute(
                            "UPDATE input_deliveries SET state=?2 WHERE message_id=?1",
                            params![r.message, state],
                        )
                        .unwrap();
                    }
                }
            }
            assert!(resolve(&f, &r).is_err(), "{change}");
        }
    }
    fn other_room(db: &Connection) -> String {
        let room = uuid::Uuid::new_v4().to_string();
        db.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note,owner,shared) VALUES(?1,'Other',1,1,'idle','','owner',0)", [&room]).unwrap();
        room
    }

    #[test]
    fn call_owner_agent_parent_and_delegation_cannot_be_substituted() {
        let f = fixture();
        let r = request(&f, "identity", "Saved source", "submitted");
        let db = f.s.db.lock().unwrap();
        for (call, owner, agent, parent) in [
            ("other-call", "owner", "default", f.parent.as_str()),
            (f.call.as_str(), "other-owner", "default", f.parent.as_str()),
            (f.call.as_str(), "owner", "other-agent", f.parent.as_str()),
            (f.call.as_str(), "owner", "default", "other-parent"),
        ] {
            assert!(resolve_in(&f.s, &db, call, owner, agent, parent, &r.delegation).is_err());
        }
    }

    #[test]
    fn completed_human_request_without_active_bound_peer_cannot_post_or_ensure_again() {
        let f = fixture();
        let r = request(&f, "completed", "Original request", "completed");
        let db = f.s.db.lock().unwrap();
        db.execute(
            "UPDATE companion_delegations SET state='completed' WHERE id=?1",
            [&r.delegation],
        )
        .unwrap();
        assert!(
            resolve_in(
                &f.s,
                &db,
                &f.call,
                "owner",
                "default",
                &f.parent,
                &r.delegation
            )
            .is_err()
        );
        assert!(
            resolve_post_in(
                &f.s,
                &db,
                &f.call,
                "owner",
                "default",
                &f.parent,
                &r.delegation,
                "unrelated-room",
                "unrelated-installation"
            )
            .is_err()
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM companion_delegations WHERE id=?1",
                [&r.delegation],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "completed"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM huddle_peer_deliveries", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn post_resolver_keeps_current_human_source_and_rejects_changed_speech() {
        let f = fixture();
        let r = request(&f, "current", "Exact original request", "submitted");
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            resolve_post_in(
                &f.s,
                &db,
                &f.call,
                "owner",
                "default",
                &f.parent,
                &r.delegation,
                "room",
                "installation"
            )
            .unwrap(),
            expected(&f, &r)
        );
        db.execute("UPDATE companion_delegations SET source=json_set(source,'$.fragments[0].text','Changed human text') WHERE id=?1", [&r.delegation]).unwrap();
        assert!(
            resolve_post_in(
                &f.s,
                &db,
                &f.call,
                "owner",
                "default",
                &f.parent,
                &r.delegation,
                "room",
                "installation"
            )
            .is_err()
        );
    }

    #[test]
    fn inactive_or_unknown_delegation_and_incomplete_message_cannot_authorize() {
        for change in [
            "closed",
            "closing",
            "interrupted",
            "starting",
            "waiting_transcript",
            "unknown",
            "complete",
            "message-incomplete",
            "actor",
        ] {
            let f = fixture();
            let r = request(&f, "state", "Saved source", "submitted");
            {
                let db = f.s.db.lock().unwrap();
                match change {
                    "closed" | "closing" | "interrupted" | "starting" => {
                        db.execute(
                            "UPDATE companion_calls SET state=?2 WHERE id=?1",
                            params![f.call, change],
                        )
                        .unwrap();
                    }
                    "message-incomplete" => {
                        db.execute(
                            "UPDATE messages SET status='streaming' WHERE id=?1",
                            [&r.message],
                        )
                        .unwrap();
                    }
                    "actor" => {
                        db.execute(
                            "UPDATE messages SET actor='other' WHERE id=?1",
                            [&r.message],
                        )
                        .unwrap();
                    }
                    state => {
                        db.execute(
                            "UPDATE companion_delegations SET state=?2 WHERE id=?1",
                            params![r.delegation, state],
                        )
                        .unwrap();
                    }
                }
            }
            assert!(resolve(&f, &r).is_err(), "{change}");
        }
    }

    #[test]
    fn private_membership_and_current_endpoint_changes_revoke_source() {
        for change in ["shared", "member", "room-agent", "endpoint", "owner"] {
            let f = fixture();
            let r = request(&f, "private", "Saved source", "submitted");
            {
                let db = f.s.db.lock().unwrap();
                match change {
                    "shared" => {
                        db.execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.parent])
                            .unwrap();
                    }
                    "member" => {
                        db.execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'other','member',1)", [&f.parent]).unwrap();
                    }
                    "room-agent" => {
                        db.execute(
                            "DELETE FROM room_agents WHERE conversation_id=?1",
                            [&f.parent],
                        )
                        .unwrap();
                    }
                    "endpoint" => {
                        db.execute("UPDATE agent_endpoints SET config=json_set(config,'$.label','Changed endpoint') WHERE id='default'", []).unwrap();
                    }
                    "owner" => {
                        db.execute(
                            "UPDATE conversations SET owner='other' WHERE id=?1",
                            [&f.parent],
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
            }
            assert!(resolve(&f, &r).is_err(), "{change}");
        }
    }

    #[test]
    fn original_browser_or_admin_token_must_still_authorize_source() {
        let f = fixture();
        let r = request(&f, "authority", "Saved source", "submitted");
        assert!(resolve(&f, &r).is_ok());
        f.s.db
            .lock()
            .unwrap()
            .execute("DELETE FROM browser_sessions", [])
            .unwrap();
        assert!(resolve(&f, &r).is_err());
        {
            let db = f.s.db.lock().unwrap();
            db.execute("UPDATE input_authority SET browser_hash=NULL,token_hash=(SELECT hash FROM tokens WHERE id='local-control') WHERE message_id=?1", [&r.message]).unwrap();
        }
        assert!(resolve(&f, &r).is_ok());
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE tokens SET expires_at=0 WHERE id='local-control'",
                [],
            )
            .unwrap();
        assert!(resolve(&f, &r).is_err());
    }

    #[test]
    fn same_transaction_recheck_rejects_closure_or_revocation_after_preflight() {
        for change in ["call", "credential", "delivery", "speech"] {
            let f = fixture();
            let r = request(&f, "recheck", "Saved source", "submitted");
            let earlier = resolve(&f, &r).unwrap();
            let mut db = f.s.db.lock().unwrap();
            let tx = db.transaction().unwrap();
            match change {
                "call" => {
                    tx.execute(
                        "UPDATE companion_calls SET state='closing' WHERE id=?1",
                        [&f.call],
                    )
                    .unwrap();
                }
                "credential" => {
                    tx.execute("DELETE FROM browser_sessions", []).unwrap();
                }
                "delivery" => {
                    tx.execute(
                        "UPDATE input_deliveries SET state='cancelled' WHERE message_id=?1",
                        [&r.message],
                    )
                    .unwrap();
                }
                "speech" => {
                    tx.execute("UPDATE companion_events SET data=json_set(data,'$.text','Altered') WHERE seq=?1", [r.speech_seq]).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                resolve_in(
                    &f.s,
                    &tx,
                    &f.call,
                    "owner",
                    "default",
                    &f.parent,
                    &r.delegation
                )
                .is_err(),
                "{change}"
            );
            tx.rollback().unwrap();
            assert_eq!(
                resolve_in(
                    &f.s,
                    &db,
                    &f.call,
                    "owner",
                    "default",
                    &f.parent,
                    &r.delegation
                )
                .unwrap(),
                earlier
            );
        }
    }
}
