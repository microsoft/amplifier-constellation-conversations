//! Receiving a result and making it available to the speaking model are
//! different events. Persist complete updates while the caller has the floor.
//! Audio activity is a presentation hint, never a completed turn or authority.
use crate::{
    auth,
    state::{AppState, now_ms},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS companion_voice_batches(id TEXT PRIMARY KEY,call_id TEXT NOT NULL REFERENCES companion_calls(id) ON DELETE CASCADE,semantic_key TEXT NOT NULL,updates TEXT NOT NULL,state TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS companion_voice_pending ON companion_voice_batches(call_id,state,created_at);")?;
    // A process interruption does not establish what the voice service heard.
    db.execute(
        "UPDATE companion_voice_batches SET state='unknown' WHERE state='dispatching'",
        [],
    )?;
    Ok(())
}

pub(crate) fn enqueue_batch(
    s: &AppState,
    call: &str,
    semantic_key: &str,
    updates: Vec<Value>,
) -> Result<bool> {
    ensure!(
        !semantic_key.is_empty()
            && semantic_key.len() <= 512
            && !updates.is_empty()
            && updates.len() <= 32,
        "Invalid voice update batch"
    );
    for update in &updates {
        ensure!(
            update["op"] == "append"
                && matches!(update["channel"].as_str(), Some("thinking" | "commentary"))
                && update["content"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty() && text.len() <= 480)
                && update["event_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 256)
                && (update["delegation_id"].is_null()
                    || update["delegation_id"]
                        .as_str()
                        .is_some_and(|id| !id.is_empty() && id.len() <= 160)),
            "Invalid complete voice update"
        );
    }
    let serialized = serde_json::to_string(&updates)?;
    let batch = auth::digest(&json!([call, semantic_key, serialized]).to_string());
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_calls WHERE id=?1 AND state IN ('connecting','connected'))", [call], |r|r.get(0))?;
    if !active {
        return Ok(false);
    }
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_voice_batches WHERE id=?1)",
        [&batch],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(true);
    }
    let bytes: i64 = tx.query_row("SELECT COALESCE(SUM(length(updates)),0) FROM companion_voice_batches WHERE call_id=?1 AND state='pending' AND semantic_key!=?2", params![call,semantic_key], |r|r.get(0))?;
    if bytes + serialized.len() as i64 > 512 * 1024 {
        // Keep the result in its canonical card/receipt. Queue pressure must not
        // disconnect a conversation or imply the caller heard the result.
        tx.execute("INSERT OR IGNORE INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'context_delivery',?3,?4)",params![call,format!("batch:{batch}:retained"),json!({"batch_id":batch,"state":"not_queued","reason":"pending_context_full","result_retained":true,"automatic_replay_allowed":false}).to_string(),now_ms()])?;
        tx.commit()?;
        return Ok(false);
    }
    // Only a newer revision of the SAME fact replaces an unsent update. Results
    // for different questions always retain their independent identities.
    tx.execute("UPDATE companion_voice_batches SET state='superseded',updated_at=?3 WHERE call_id=?1 AND semantic_key=?2 AND state='pending'", params![call,semantic_key,now_ms()])?;
    tx.execute("INSERT INTO companion_voice_batches(id,call_id,semantic_key,updates,state,created_at,updated_at) VALUES(?1,?2,?3,?4,'pending',?5,?5)", params![batch,call,semantic_key,serialized,now_ms()])?;
    for update in &updates {
        tx.execute("INSERT OR IGNORE INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'context_delivery',?3,?4)", params![call,format!("delivery:{}:deferred",update["event_id"].as_str().unwrap()),json!({"event_id":update["event_id"],"delegation_id":update["delegation_id"],"channel":update["channel"],"content":update["content"],"batch_id":batch,"state":"deferred"}).to_string(),now_ms()])?;
    }
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(true)
}

/// Withdraw only unsent context for this exact semantic key. A newer read can
/// intentionally select no facts; that is not delivery, source deletion or a
/// reason to withdraw unrelated answers. Already dispatched/unknown batches stay.
pub(crate) fn supersede_pending(s: &AppState, call: &str, semantic_key: &str) -> Result<()> {
    ensure!(
        !semantic_key.is_empty() && semantic_key.len() <= 512,
        "Invalid voice update key"
    );
    let db = s.db.lock().unwrap();
    db.execute("UPDATE companion_voice_batches SET state='superseded',updated_at=?3 WHERE call_id=?1 AND semantic_key=?2 AND state='pending'", params![call,semantic_key,now_ms()])?;
    drop(db);
    s.changed();
    Ok(())
}

/// Recheck at the final transport boundary. Neither a quiet microphone nor a
/// transcript gap claims that the person finished their thought.
pub(crate) fn floor_open_in(db: &Connection, call: &str) -> Result<bool> {
    type Surface = (Option<bool>, Option<bool>, i64, Option<String>);
    let surface: Option<Surface> = db.query_row("SELECT user_speaking,agent_speaking,updated_at,playback FROM companion_surfaces WHERE call_id=?1", [call], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((Some(false), Some(false), at, playback)) = surface else {
        return Ok(false);
    };
    if now_ms() - at > 5000 {
        return Ok(false);
    }
    if let Some(raw) = playback {
        let p: Value = serde_json::from_str(&raw)?;
        if p["state"] != "ready" || p["floor"] != "open" {
            return Ok(false);
        }
    }
    let last_user: i64 = db.query_row("SELECT COALESCE(MAX(at),0) FROM companion_events WHERE call_id=?1 AND kind='transcript' AND json_extract(data,'$.role')='user'", [call], |r|r.get(0))?;
    Ok(now_ms() - last_user >= 1200)
}

pub(crate) fn take_ready(s: &AppState, call: &str) -> Result<Option<Value>> {
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    if !floor_open_in(&tx, call)? {
        return Ok(None);
    }
    let row: Option<(String,String)>=tx.query_row("SELECT id,updates FROM companion_voice_batches WHERE call_id=?1 AND state='pending' ORDER BY created_at,rowid LIMIT 1",[call],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((id, raw)) = row else {
        return Ok(None);
    };
    let commands: Vec<Value> = serde_json::from_str(&raw)?;
    let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_calls WHERE id=?1 AND state IN ('connecting','connected'))", [call], |r|r.get(0))?;
    if !active
        || crate::companion_history::private_identity(&tx, call)?.is_none()
        || !crate::companion_awareness::presentation_allowed(s, &tx, call, &commands)?
        || !crate::companion_huddles::presentation_allowed(s, &tx, call, &commands)?
        || !crate::companion_surface::board_presentation_allowed(&tx, call, &commands)?
    {
        tx.execute(
            "UPDATE companion_voice_batches SET state='withdrawn',updated_at=?2 WHERE id=?1",
            params![id, now_ms()],
        )?;
        tx.commit()?;
        return Ok(None);
    }
    // Recheck the exact pending source at dispatch: a terminal result can arrive
    // while the caller holds the floor. Never append stale waiting context then.
    for command in &commands {
        if command["event_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("request-pending-"))
            && !crate::companion::pending_context_matches(
                &tx,
                call,
                command["delegation_id"].as_str().unwrap_or(""),
                command["content"].as_str().unwrap_or(""),
            )?
        {
            tx.execute(
                "UPDATE companion_voice_batches SET state='superseded',updated_at=?2 WHERE id=?1",
                params![id, now_ms()],
            )?;
            tx.commit()?;
            return Ok(None);
        }
    }
    tx.execute("UPDATE companion_voice_batches SET state='dispatching',updates=?3,updated_at=?2 WHERE id=?1 AND state='pending'",params![id,now_ms(),serde_json::to_string(&commands)?])?;
    for command in &commands {
        tx.execute("INSERT OR IGNORE INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'context_delivery',?3,?4)",params![call,format!("delivery:{}:queued",command["event_id"].as_str().unwrap()),json!({"event_id":command["event_id"],"delegation_id":command["delegation_id"],"channel":command["channel"],"content":command["content"],"batch_id":id,"state":"queued"}).to_string(),now_ms()])?;
    }
    tx.commit()?;
    Ok(Some(
        json!({"op":"delivery_batch","batch_id":id,"commands":commands}),
    ))
}

pub(crate) fn sent(s: &AppState, call: &str, batch: &str) -> Result<()> {
    s.db.lock().unwrap().execute("UPDATE companion_voice_batches SET state='sent',updated_at=?3 WHERE id=?1 AND call_id=?2 AND state='dispatching'",params![batch,call,now_ms()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Agent, Config};
    use std::{collections::BTreeMap, sync::Arc};
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
            label: "Delivery fixture".into(),
            owner: "owner".into(),
            allowed_users: vec![],
            embed_origins: vec![],
            bind: "127.0.0.1".into(),
            port: 0,
            public_url: "https://example.test:18445".into(),
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
        s.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)",params![call,uuid::Uuid::new_v4().to_string(),room,now_ms(),endpoint]).unwrap();
        s.db.lock().unwrap().execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at,playback) VALUES(?1,'browser','session',0,0,0,0,?2,?3)",params![call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
        Fixture {
            _home: home,
            s,
            call,
            room,
        }
    }
    fn update(id: &str, text: &str) -> Value {
        json!({"op":"append","channel":"thinking","event_id":id,"delegation_id":null,"content":text})
    }
    fn state(f: &Fixture, id: &str) -> String {
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM companion_voice_batches WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn holds_complete_results_through_speech_unknown_playback_and_recent_transcript() {
        let f = fixture();
        enqueue_batch(
            &f.s,
            &f.call,
            "result:a",
            vec![update("a", "The door is blue; the roof is not finished.")],
        )
        .unwrap();
        for (speaking, playback) in [
            (true, json!({"state":"ready","floor":"user"})),
            (false, json!({"state":"suppressed","floor":"user"})),
            (false, json!({"state":"unknown","floor":"unknown"})),
        ] {
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE companion_surfaces SET user_speaking=?2,playback=?3 WHERE call_id=?1",
                    params![f.call, speaking, playback.to_string()],
                )
                .unwrap();
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        }
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_surfaces SET user_speaking=0,playback=?2 WHERE call_id=?1",
                params![f.call, json!({"state":"ready","floor":"open"}).to_string()],
            )
            .unwrap();
        f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'speech','transcript',?2,?3)",params![f.call,json!({"role":"user","text":"And another thing"}).to_string(),now_ms()]).unwrap();
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_events SET at=?2 WHERE call_id=?1",
                params![f.call, now_ms() - 2000],
            )
            .unwrap();
        let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert_eq!(
            batch["commands"][0]["content"],
            "The door is blue; the roof is not finished."
        );
        assert_eq!(
            state(&f, batch["batch_id"].as_str().unwrap()),
            "dispatching"
        );
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }

    #[test]
    fn newer_delegation_does_not_prove_an_awaited_answer_was_replaced() {
        {
            let f = fixture();
            let old_at = now_ms() - 6000;
            f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'earlier',100,'completed','original-input',?2)",params![f.call,old_at]).unwrap();
            let mut answer = update(
                "door-answer",
                "The door was blue; no repaint was performed.",
            );
            answer["channel"] = json!("commentary");
            answer["delegation_id"] = json!("earlier");
            enqueue_batch(&f.s, &f.call, "result:door", vec![answer.clone()]).unwrap();
            enqueue_batch(
                &f.s,
                &f.call,
                "result:roof",
                vec![update("roof-answer", "The roof remains unfinished.")],
            )
            .unwrap();
            {
                let db = f.s.db.lock().unwrap();
                db.execute(
                    "UPDATE companion_voice_batches SET created_at=?2 WHERE call_id=?1",
                    params![f.call, old_at],
                )
                .unwrap();
                db.execute(
                    "UPDATE companion_surfaces SET user_speaking=1,playback=?2 WHERE call_id=?1",
                    params![f.call, json!({"state":"ready","floor":"user"}).to_string()],
                )
                .unwrap();
                db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'correction',200,'dispatched','correction-input',?2)",params![f.call,old_at+1000]).unwrap();
            }
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            f.s.db.lock().unwrap().execute("UPDATE companion_surfaces SET user_speaking=0,updated_at=?2,playback=?3 WHERE call_id=?1",params![f.call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
            let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
            let commands = batch["commands"].as_array().unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0], answer);
            assert!(
                commands
                    .iter()
                    .all(|c| c["content"].as_str().unwrap().len() <= 480)
            );
            let next = take_ready(&f.s, &f.call).unwrap().unwrap();
            assert_eq!(next["commands"][0]["event_id"], "roof-answer");
            assert_eq!(
                next["commands"][0]["content"],
                "The roof remains unfinished."
            );
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        }
    }

    #[test]
    fn local_conversation_does_not_demote_the_pending_answer_or_bypass_the_floor() {
        for speech in [
            "Thanks, take your time.",
            "While that runs, what did I say my name was?",
        ] {
            let f = fixture();
            let old_at = now_ms() - 6000;
            f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'pending',100,'completed','original-input',?2)",params![f.call,old_at]).unwrap();
            let mut answer = update("pending-answer", "The checked shelf contains three cups.");
            answer["channel"] = json!("commentary");
            answer["delegation_id"] = json!("pending");
            enqueue_batch(&f.s, &f.call, "result:pending", vec![answer.clone()]).unwrap();
            {
                let db = f.s.db.lock().unwrap();
                db.execute(
                    "UPDATE companion_voice_batches SET created_at=?2 WHERE call_id=?1",
                    params![f.call, old_at],
                )
                .unwrap();
                db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'local-conversation','transcript',?2,?3)",params![f.call,json!({"role":"user","text":speech}).to_string(),now_ms()]).unwrap();
                db.execute(
                    "UPDATE companion_surfaces SET user_speaking=1,playback=?2 WHERE call_id=?1",
                    params![f.call, json!({"state":"ready","floor":"user"}).to_string()],
                )
                .unwrap();
            }
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            f.s.db.lock().unwrap().execute("UPDATE companion_surfaces SET user_speaking=0,updated_at=?2,playback=?3 WHERE call_id=?1",params![f.call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
            // A quiet microphone does not erase the recent speech guard.
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            f.s.db.lock().unwrap().execute("UPDATE companion_events SET at=?2 WHERE call_id=?1 AND event_key='local-conversation'",params![f.call,now_ms()-2000]).unwrap();
            let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
            assert_eq!(batch["commands"], json!([answer.clone()]));
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            sent(&f.s, &f.call, batch["batch_id"].as_str().unwrap()).unwrap();
            // Neither a later poll nor re-enqueuing the same answer replays it.
            enqueue_batch(&f.s, &f.call, "result:pending", vec![answer]).unwrap();
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            let db = f.s.db.lock().unwrap();
            let presentations: i64 = db.query_row("SELECT count(*) FROM companion_events WHERE call_id=?1 AND json_extract(data,'$.state')='presentation_changed'",[&f.call],|r|r.get(0)).unwrap();
            assert_eq!(presentations, 0);
        }
    }

    #[test]
    fn current_answer_and_assistant_activity_do_not_become_an_old_human_request() {
        let f = fixture();
        f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'current',100,'completed','input',?2)",params![f.call,now_ms()-2000]).unwrap();
        let mut answer = update(
            "current-answer",
            "The current observation shows the door is closed.",
        );
        answer["channel"] = json!("commentary");
        answer["delegation_id"] = json!("current");
        enqueue_batch(&f.s, &f.call, "result:current", vec![answer.clone()]).unwrap();
        f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'assistant','transcript',?2,?3)",params![f.call,json!({"role":"assistant","text":"Checking."}).to_string(),now_ms()+1]).unwrap();
        let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert_eq!(batch["commands"], json!([answer]));
    }
    #[test]
    fn revisions_replace_only_unsent_same_fact_and_independent_answers_survive() {
        let f = fixture();
        enqueue_batch(
            &f.s,
            &f.call,
            "status:door",
            vec![update("r1", "Door open.")],
        )
        .unwrap();
        enqueue_batch(
            &f.s,
            &f.call,
            "answer:roof",
            vec![update("roof", "Roof unfinished.")],
        )
        .unwrap();
        enqueue_batch(
            &f.s,
            &f.call,
            "status:door",
            vec![update("r2", "Door closed.")],
        )
        .unwrap();
        let first = take_ready(&f.s, &f.call).unwrap().unwrap();
        let second = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert_eq!(first["commands"][0]["event_id"], "roof");
        assert_eq!(second["commands"][0]["event_id"], "r2");
        sent(&f.s, &f.call, second["batch_id"].as_str().unwrap()).unwrap();
        enqueue_batch(
            &f.s,
            &f.call,
            "status:door",
            vec![update("r2", "Door closed.")],
        )
        .unwrap();
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }
    #[test]
    fn dispatch_interruption_is_unknown_not_automatic_redelivery() {
        let f = fixture();
        enqueue_batch(
            &f.s,
            &f.call,
            "answer",
            vec![update("one", "One complete answer.")],
        )
        .unwrap();
        let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
        migrate(&f.s.db.lock().unwrap()).unwrap();
        assert_eq!(state(&f, batch["batch_id"].as_str().unwrap()), "unknown");
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }
    #[test]
    fn queue_pressure_retains_result_without_closing_call() {
        let f = fixture();
        let oversized = "x".repeat(512 * 1024 + 1);
        f.s.db.lock().unwrap().execute("INSERT INTO companion_voice_batches(id,call_id,semantic_key,updates,state,created_at,updated_at) VALUES('full',?1,'old',?2,'pending',?3,?3)",params![f.call,oversized,now_ms()]).unwrap();
        assert!(
            !enqueue_batch(
                &f.s,
                &f.call,
                "new",
                vec![update("new", "A saved complete answer.")]
            )
            .unwrap()
        );
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM companion_calls WHERE id=?1",
                    [&f.call],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "connected"
        );
        let receipt: String = f
            .s
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT data FROM companion_events WHERE call_id=?1 AND kind='context_delivery'",
                [&f.call],
                |r| r.get(0),
            )
            .unwrap();
        let receipt: Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(receipt["state"], "not_queued");
        assert_eq!(receipt["result_retained"], true);
    }

    #[test]
    fn closed_call_or_changed_private_room_withdraws_pending_result() {
        for close in [true, false] {
            let f = fixture();
            enqueue_batch(
                &f.s,
                &f.call,
                "answer",
                vec![update("one", "Private answer.")],
            )
            .unwrap();
            if close {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET state='closed' WHERE id=?1",
                        [&f.call],
                    )
                    .unwrap();
            } else {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room])
                    .unwrap();
            }
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            assert_eq!(
                f.s.db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM companion_voice_batches WHERE call_id=?1",
                        [&f.call],
                        |r| r.get::<_, String>(0)
                    )
                    .unwrap(),
                "withdrawn"
            );
        }
    }
}
