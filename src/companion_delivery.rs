//! Receiving a result and making it available to the speaking model are
//! different events. Persist speech updates while the caller has the floor.
//! Only the exact call-local pending receipt can supply quiet awareness then.
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

/// Quiet receipt context does not request speech. It may cross known ongoing
/// speech or a recent transcript, but missing/stale playback evidence still
/// defers it. Suppression is the normal browser state while the caller speaks.
fn quiet_context_ready_in(db: &Connection, call: &str) -> Result<bool> {
    type Surface = (Option<bool>, Option<bool>, i64, Option<String>);
    let surface: Option<Surface> = db.query_row("SELECT user_speaking,agent_speaking,updated_at,playback FROM companion_surfaces WHERE call_id=?1", [call], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((Some(_), Some(_), at, Some(playback))) = surface else {
        return Ok(false);
    };
    if !(0..=5000).contains(&(now_ms() - at)) {
        return Ok(false);
    }
    let p: Value = serde_json::from_str(&playback)?;
    Ok(matches!(p["state"].as_str(), Some("ready" | "suppressed"))
        && matches!(p["floor"].as_str(), Some("open" | "user")))
}

/// For a single, directly delegated text question the duplex model sometimes
/// generates the literal answer before its selected agent returns. The card
/// and native result remain authoritative. Confirm their source quietly when
/// the entire generated phrase already matches, without inferring playback.
fn generated_answer_confirmation(
    s: &AppState,
    db: &Connection,
    call: &str,
    command: &Value,
) -> Result<Option<String>> {
    const PREFIX: &str = "The selected agent returned this answer for this request: ";
    let Some(target) = command["event_id"]
        .as_str()
        .and_then(|id| id.strip_prefix("plain-result:"))
    else {
        return Ok(None);
    };
    let Some(did) = command["delegation_id"].as_str() else {
        return Ok(None);
    };
    let text = command["content"].as_str().unwrap_or("");
    if command["channel"] != "commentary" || text.is_empty() || PREFIX.len() + text.len() > 480 {
        return Ok(None);
    }
    // Fail open to normal commentary for ambiguous or repeated requests,
    // other inputs, actions, permission decisions, and missing source bindings.
    let source: Option<(String,String,i64,i64,i64)> = db.query_row(
        "SELECT c.owner,c.conversation_id,u.seq,json_extract(u.data,'$.end_ms'),card.seq
         FROM companion_calls c
         JOIN companion_delegations x ON x.call_id=c.id AND x.id=?2 AND x.state='completed'
         JOIN messages input ON input.id=x.message_id AND input.conversation_id=c.conversation_id AND input.role='user' AND input.actor=c.owner
         JOIN input_deliveries d ON d.message_id=input.id AND d.conversation_id=c.conversation_id AND d.agent_id=c.agent_id
             AND d.participation='direct' AND d.state='completed' AND d.target_turn=?3
         JOIN messages m ON m.id=d.target_turn AND m.conversation_id=c.conversation_id AND m.agent_id=c.agent_id
             AND m.role='assistant' AND m.status='complete' AND m.text=?4
         JOIN companion_events card ON card.call_id=c.id AND card.kind='card' AND card.event_key='result:'||m.id
             AND json_extract(card.data,'$.request_id')=input.id AND json_extract(card.data,'$.delegation_id')=x.id
             AND json_extract(card.data,'$.message_id')=m.id AND json_extract(card.data,'$.text')=m.text AND json_extract(card.data,'$.late')=0 AND json_extract(card.data,'$.truncated')=0
             AND json_extract(card.data,'$.conversation_id')=c.conversation_id AND json_array_length(card.data,'$.attachments')=0
         JOIN companion_events u ON u.seq=(SELECT MAX(seq) FROM companion_events WHERE call_id=c.id AND kind='transcript' AND json_extract(data,'$.role')='user')
         WHERE c.id=?1 AND c.state IN ('connecting','connected')
             AND json_extract(x.source,'$.version')=1 AND EXISTS(SELECT 1 FROM json_each(x.source,'$.fragments') f
                 WHERE json_extract(f.value,'$.seq')=u.seq AND json_extract(f.value,'$.event_id')=u.event_key
                   AND json_extract(f.value,'$.role')='user' AND json_extract(f.value,'$.text')=json_extract(u.data,'$.text'))
             AND NOT EXISTS(SELECT 1 FROM companion_delegations WHERE call_id=c.id AND id!=x.id)
             AND NOT EXISTS(SELECT 1 FROM input_deliveries WHERE conversation_id=c.conversation_id AND id!=d.id)
             AND NOT EXISTS(SELECT 1 FROM messages WHERE conversation_id=c.conversation_id AND role='user' AND status!='voice_fragment' AND seq>input.seq)
             AND NOT EXISTS(SELECT 1 FROM companion_actions WHERE call_id=c.id)
             AND NOT EXISTS(SELECT 1 FROM companion_events WHERE call_id=c.id AND kind IN ('permission','playback'))",
        params![call,did,target,text], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))
    ).optional()?;
    let Some((owner, room, anchor, mut last_end, card_seq)) = source else {
        return Ok(None);
    };
    if !s.permissions.list(&room, &owner).is_empty() {
        return Ok(None);
    }
    let mut statement = db.prepare("SELECT seq,json_extract(data,'$.text'),json_extract(data,'$.start_ms'),json_extract(data,'$.end_ms'),json_extract(data,'$.role')='assistant' AND json_extract(data,'$.fragment')=1 AND json_extract(data,'$.event_id')=event_key FROM companion_events WHERE call_id=?1 AND kind='transcript' AND seq>?2 ORDER BY seq LIMIT 33")?;
    let mut rows = statement.query(params![call, anchor])?;
    let mut generated = String::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        let (seq, word, start, end, exact) = (
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<bool>>(4)?,
        );
        if count > 32
            || exact != Some(true)
            || seq >= card_seq
            || start < last_end
            || start.saturating_sub(last_end) > 3000
            || end < start
            || generated.len() + word.len() > 480
        {
            return Ok(None);
        }
        generated.push_str(&word);
        last_end = end;
    }
    Ok((generated.trim() == text.trim()).then(|| format!("{PREFIX}{text}")))
}

pub(crate) fn take_ready(s: &AppState, call: &str) -> Result<Option<Value>> {
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let speech_ready = floor_open_in(&tx, call)?;
    if !speech_ready && !quiet_context_ready_in(&tx, call)? {
        return Ok(None);
    }
    // When speech is held, select the oldest eligible quiet receipt without
    // consuming or reordering the held speech batches. The bounded durable
    // queue and the existing serialized bridge writer still own delivery.
    let rows = {
        let mut q = tx.prepare("SELECT id,semantic_key,updates FROM companion_voice_batches WHERE call_id=?1 AND state='pending' ORDER BY created_at,rowid")?;
        q.query_map([call], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut selected = None;
    for (id, semantic_key, raw) in rows {
        let commands: Vec<Value> = serde_json::from_str(&raw)?;
        if let Some(did) = semantic_key.strip_prefix("wait-cue:")
            && !crate::companion::wait_cue_current(&tx, call, did)?
        {
            tx.execute("UPDATE companion_voice_batches SET state='superseded',updated_at=?2 WHERE id=?1 AND state='pending'", params![id, now_ms()])?;
            continue;
        }
        // Recheck permission presence and revision under the same DB transaction
        // that selects delivery. A resolved unsent notice is never speech-ready.
        if !commands.iter().try_fold(true, |valid, command| {
            Ok::<_, anyhow::Error>(
                valid && crate::companion::permission_update_matches(s, &tx, call, command)?,
            )
        })? {
            tx.execute("UPDATE companion_voice_batches SET state='superseded',updated_at=?2 WHERE id=?1 AND state='pending'", params![id, now_ms()])?;
            continue;
        }
        if speech_ready
            || crate::companion::is_pending_context_batch(&semantic_key, &commands)
            || crate::companion::is_permission_revision_batch(
                s,
                &tx,
                call,
                &semantic_key,
                &commands,
            )?
        {
            selected = Some((id, commands));
            break;
        }
    }
    let Some((id, mut commands)) = selected else {
        tx.commit()?;
        return Ok(None);
    };
    let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_calls WHERE id=?1 AND state IN ('connecting','connected'))", [call], |r|r.get(0))?;
    if !active
        || crate::companion_history::private_identity(&tx, call)?.is_none()
        || !crate::companion_awareness::presentation_allowed(s, &tx, call, &commands)?
        || !crate::companion_huddles::presentation_allowed(s, &tx, call, &commands)?
        || !crate::companion_surface::board_presentation_allowed(&tx, call, &commands)?
        || !crate::companion_surface::capability_presentation_allowed(&tx, call, &commands)?
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
    // Unavailable evidence leaves normal delivery intact. Only the final,
    // admitted command changes; the original card and pending receipt persist.
    if let [command] = commands.as_mut_slice()
        && let Ok(Some(confirmation)) = generated_answer_confirmation(s, &tx, call, command)
    {
        command["channel"] = json!("thinking");
        command["content"] = json!(confirmation);
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
        s.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)",params![call,uuid::Uuid::new_v4().to_string(),room,now_ms(),endpoint]).unwrap();
        s.db.lock().unwrap().execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at,playback) VALUES(?1,'browser','session',0,0,0,0,?2,?3)",params![call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
        Fixture {
            _home: home,
            s,
            call,
            room,
        }
    }
    #[test]
    fn slow_request_cue_expires_on_more_speech_or_resolution_while_the_floor_is_held() {
        for change in [
            "correction",
            "spoken_ack",
            "completed",
            "expired",
            "blocked",
        ] {
            let f = fixture();
            let now = now_ms();
            {
                let db = f.s.db.lock().unwrap();
                db.execute("INSERT INTO messages(id,conversation_id,role,actor,text,status,created_at,updated_at) VALUES('m',?1,'user','owner','Make a list','complete',?2,?2)",params![f.room,now]).unwrap();
                db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES('d','m',?1,'default','direct','received',?2)",params![f.room,now]).unwrap();
                db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'speech','transcript',?2,?3)",params![f.call,json!({"role":"user"}).to_string(),now-8000]).unwrap();
                let seq = db.last_insert_rowid();
                db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at,source) VALUES(?1,'cue',100,'dispatched','m',?2,?3)",params![f.call,now-7000,json!({"fragments":[{"seq":seq,"role":"user"}]}).to_string()]).unwrap();
                assert!(crate::companion::wait_cue_current(&db, &f.call, "cue").unwrap());
                db.execute(
                    "UPDATE companion_surfaces SET user_speaking=1 WHERE call_id=?1",
                    [&f.call],
                )
                .unwrap();
            }
            let mut cue = update("cue-event", "Fixture waiting cue");
            cue["channel"] = json!("commentary");
            enqueue_batch(&f.s, &f.call, "wait-cue:cue", vec![cue]).unwrap();
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
            {
                let db = f.s.db.lock().unwrap();
                match change {
                    "correction" | "spoken_ack" => {
                        let role = if change == "correction" {
                            "user"
                        } else {
                            "assistant"
                        };
                        db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'more','transcript',?2,?3)",params![f.call,json!({"role":role}).to_string(),now-3000]).unwrap();
                    }
                    "completed" => {
                        db.execute(
                            "UPDATE input_deliveries SET state='completed' WHERE id='d'",
                            [],
                        )
                        .unwrap();
                    }
                    "expired" => {
                        db.execute(
                            "UPDATE companion_delegations SET created_at=?1 WHERE id='cue'",
                            [now - 21000],
                        )
                        .unwrap();
                    }
                    "blocked" => {
                        db.execute(
                            "UPDATE input_deliveries SET state='unknown' WHERE id='d'",
                            [],
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
                db.execute(
                    "UPDATE companion_surfaces SET user_speaking=0 WHERE call_id=?1",
                    [&f.call],
                )
                .unwrap();
            }
            assert!(take_ready(&f.s, &f.call).unwrap().is_none(), "{change}");
            let saved:String=f.s.db.lock().unwrap().query_row("SELECT state FROM companion_voice_batches WHERE call_id=?1 AND semantic_key='wait-cue:cue'",[&f.call],|r|r.get(0)).unwrap();
            assert_eq!(saved, "superseded", "{change}");
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

    fn permission(f: &Fixture) -> crate::permissions::Request {
        f.s.permissions
            .request(
                &f.room,
                "owner",
                "default",
                &json!({
                    "options":[{"optionId":"yes","name":"Allow once","kind":"allow_once"},
                               {"optionId":"no","name":"Reject","kind":"reject_once"}]
                }),
                std::time::Duration::from_secs(60),
            )
            .unwrap()
    }
    fn permission_decide(f: &Fixture, id: &str, choice: Option<&str>) {
        let kind =
            f.s.permissions
                .resolve(&f.room, "owner", id, choice)
                .unwrap();
        f.s.audit("permission.decided",Some(&f.room),json!({"actor":"owner","request_id":id,"decision":kind.unwrap_or_else(||"cancelled".into())})).unwrap();
    }
    fn notice(f: &Fixture) {
        crate::companion::permission_notice(&f.s, &f.call, &f.room).unwrap();
    }
    #[test]
    fn permission_resolved_before_dispatch_never_sends_stale_waiting_commentary() {
        let f = fixture();
        let request = permission(&f);
        notice(&f);
        permission_decide(&f, &request.id, Some("yes"));
        // Deliberately do not reconcile: this exercises the final dispatch check.
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        notice(&f);
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        assert_eq!(f.s.db.lock().unwrap().query_row("SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='context_delivery' AND json_extract(data,'$.state')='queued'",[&f.call],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
    #[test]
    fn permission_still_pending_is_sent_once_without_changing_authority() {
        let f = fixture();
        let request = permission(&f);
        notice(&f);
        notice(&f);
        let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert_eq!(
            batch["commands"][0]["event_id"],
            format!("permission_{}", request.id)
        );
        assert_eq!(batch["commands"][0]["channel"], "commentary");
        assert_eq!(f.s.permissions.list(&f.room, "owner").len(), 1);
        sent(&f.s, &f.call, batch["batch_id"].as_str().unwrap()).unwrap();
        notice(&f);
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }
    #[test]
    fn permission_notice_delivered_after_decision_remains_a_request_event() {
        for (choice, decision) in [
            (Some("yes"), "allow_once"),
            (Some("no"), "reject_once"),
            (None, "cancelled"),
        ] {
            let f = fixture();
            let request = permission(&f);
            notice(&f);
            let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
            let batch_id = batch["batch_id"].as_str().unwrap();
            sent(&f.s, &f.call, batch_id).unwrap();
            // Like the recorded call, the choice is still pending at send but
            // resolves while the already sent notice can still reach the model.
            permission_decide(&f, &request.id, choice);
            assert!(f.s.permissions.list(&f.room, "owner").is_empty());
            let delivered = &batch["commands"][0];
            assert_eq!(delivered["channel"], "commentary");
            assert_eq!(delivered["event_id"], format!("permission_{}", request.id));
            assert_eq!(delivered["content"], "An action requested your approval.");
            notice(&f);
            let revision = take_ready(&f.s, &f.call).unwrap().unwrap();
            assert_eq!(revision["commands"][0]["channel"], "thinking");
            assert_eq!(
                revision["commands"][0]["event_id"],
                format!("permission_{}_current_{decision}", request.id)
            );
            // The revision neither rewrites the attempted notice nor proves
            // any tool execution or completion. It is not audio cancellation.
            let retained: String =
                f.s.db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT updates FROM companion_voice_batches WHERE id=?1",
                        [batch_id],
                        |row| row.get(0),
                    )
                    .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&retained).unwrap(),
                batch["commands"]
            );
            assert_eq!(state(&f, batch_id), "sent");
            sent(&f.s, &f.call, revision["batch_id"].as_str().unwrap()).unwrap();
            notice(&f);
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        }
    }
    #[test]
    fn permission_sent_then_resolved_updates_context_once_without_speaking_or_completion_claim() {
        let f = fixture();
        let request = permission(&f);
        notice(&f);
        let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
        sent(&f.s, &f.call, batch["batch_id"].as_str().unwrap()).unwrap();
        permission_decide(&f, &request.id, Some("yes"));
        notice(&f);
        let revision = take_ready(&f.s, &f.call)
            .unwrap()
            .expect("A sent pending notice needs current context");
        assert_eq!(revision["commands"][0]["channel"], "thinking");
        let content = revision["commands"][0]["content"].as_str().unwrap();
        assert!(content.contains("allowed"));
        assert!(content.contains("does not establish"));
        assert!(!content.contains("cannot continue"));
        sent(&f.s, &f.call, revision["batch_id"].as_str().unwrap()).unwrap();
        notice(&f);
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }

    #[test]
    fn permission_dispatch_race_and_restart_unknown_keep_original_evidence_and_one_revision() {
        for prior in ["dispatching", "sent", "unknown"] {
            let f = fixture();
            let request = permission(&f);
            notice(&f);
            let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE companion_voice_batches SET state=?2 WHERE id=?1",
                    params![batch["batch_id"].as_str().unwrap(), prior],
                )
                .unwrap();
            permission_decide(&f, &request.id, Some("no"));
            notice(&f);
            notice(&f);
            assert_eq!(state(&f, batch["batch_id"].as_str().unwrap()), prior);
            let revision = take_ready(&f.s, &f.call).unwrap().unwrap();
            assert_eq!(revision["commands"][0]["channel"], "thinking");
            assert!(
                revision["commands"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("rejected")
            );
            assert_ne!(
                revision["commands"][0]["event_id"],
                batch["commands"][0]["event_id"]
            );
            notice(&f);
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        }
    }
    #[test]
    fn permission_resolve_audit_gap_never_infers_a_grant_and_later_exact_audit_supersedes_unknown()
    {
        let f = fixture();
        let request = permission(&f);
        notice(&f);
        let original = take_ready(&f.s, &f.call).unwrap().unwrap();
        f.s.permissions
            .resolve(&f.room, "owner", &request.id, Some("yes"))
            .unwrap();
        notice(&f);
        // The unknown revision is queued, but actual decision evidence arrives
        // before its dispatch. Current-state validation must reject that revision.
        f.s.audit(
            "permission.decided",
            Some(&f.room),
            json!({"actor":"owner","request_id":request.id,"decision":"allow_once"}),
        )
        .unwrap();
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        notice(&f);
        let revision = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert!(
            revision["commands"][0]["content"]
                .as_str()
                .unwrap()
                .contains("allowed")
        );
        assert_eq!(
            state(&f, original["batch_id"].as_str().unwrap()),
            "dispatching"
        );
        assert!(
            f.s.permissions
                .resolve(&f.room, "owner", &request.id, Some("yes"))
                .is_err()
        );
    }
    #[test]
    fn permission_missing_and_wrong_scope_decisions_are_only_unconfirmed() {
        for mode in [
            "dropped",
            "wrong-actor",
            "wrong-room",
            "wrong-request",
            "invalid-decision",
        ] {
            let f = fixture();
            let request = permission(&f);
            notice(&f);
            take_ready(&f.s, &f.call).unwrap().unwrap();
            let id = request.id.clone();
            drop(request);
            if mode != "dropped" {
                f.s.audit(
                    "permission.decided",
                    Some(if mode == "wrong-room" {
                        "other-room"
                    } else {
                        &f.room
                    }),
                    json!({
                        "actor":if mode=="wrong-actor" {"other-owner"} else {"owner"},
                        "request_id":if mode=="wrong-request" {"another-request"} else {&id},
                        "decision":if mode=="invalid-decision" {"complete"} else {"allow_once"}
                    }),
                )
                .unwrap();
            }
            notice(&f);
            let revision = take_ready(&f.s, &f.call).unwrap().unwrap();
            let text = revision["commands"][0]["content"].as_str().unwrap();
            assert!(text.contains("no longer pending"));
            assert!(text.contains("not confirmed"));
            assert!(!text.contains("allowed"));
            assert!(!text.contains("rejected"));
            notice(&f);
            assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        }
    }
    #[test]
    fn permission_current_revision_is_quiet_but_still_requires_fresh_known_private_surface() {
        for mode in [
            "speaking",
            "stale",
            "unknown",
            "private-revoked",
            "mixed-batch",
        ] {
            let f = fixture();
            let request = permission(&f);
            notice(&f);
            take_ready(&f.s, &f.call).unwrap().unwrap();
            permission_decide(&f, &request.id, None);
            notice(&f);
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE companion_surfaces SET user_speaking=1,playback=?2 WHERE call_id=?1",
                    params![
                        f.call,
                        json!({"state":"suppressed","floor":"user"}).to_string()
                    ],
                )
                .unwrap();
            match mode {
                "stale" => {
                    f.s.db
                        .lock()
                        .unwrap()
                        .execute(
                            "UPDATE companion_surfaces SET updated_at=?2 WHERE call_id=?1",
                            params![f.call, now_ms() - 6000],
                        )
                        .unwrap();
                }
                "unknown" => {
                    f.s.db
                        .lock()
                        .unwrap()
                        .execute(
                            "UPDATE companion_surfaces SET playback=?2 WHERE call_id=?1",
                            params![
                                f.call,
                                json!({"state":"unknown","floor":"unknown"}).to_string()
                            ],
                        )
                        .unwrap();
                }
                "private-revoked" => {
                    f.s.db
                        .lock()
                        .unwrap()
                        .execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room])
                        .unwrap();
                }
                "mixed-batch" => {
                    let db = f.s.db.lock().unwrap();
                    let (id,raw):(String,String)=db.query_row("SELECT id,updates FROM companion_voice_batches WHERE call_id=?1 AND state='pending'",[&f.call],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
                    let mut commands: Vec<Value> = serde_json::from_str(&raw).unwrap();
                    commands.push(update("unrelated", "Unrelated quiet context"));
                    db.execute(
                        "UPDATE companion_voice_batches SET updates=?2 WHERE id=?1",
                        params![id, serde_json::to_string(&commands).unwrap()],
                    )
                    .unwrap();
                }
                _ => {}
            }
            let value = take_ready(&f.s, &f.call).unwrap();
            if mode == "speaking" {
                let batch = value.unwrap();
                assert_eq!(batch["commands"][0]["channel"], "thinking");
                assert!(
                    batch["commands"][0]["content"]
                        .as_str()
                        .unwrap()
                        .contains("cancelled")
                );
            } else {
                assert!(value.is_none(), "{mode}");
            }
        }
    }
    #[test]
    fn permission_independent_choices_and_invalid_option_preserve_pending_authority() {
        let f = fixture();
        let first = permission(&f);
        let second = permission(&f);
        notice(&f);
        assert!(
            f.s.permissions
                .resolve(&f.room, "owner", &second.id, Some("invented"))
                .is_err()
        );
        permission_decide(&f, &first.id, Some("yes"));
        notice(&f);
        let batch = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert_eq!(
            batch["commands"][0]["event_id"],
            format!("permission_{}", second.id)
        );
        assert_eq!(f.s.permissions.list(&f.room, "owner")[0]["id"], second.id);
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }
    #[test]
    fn permission_discovery_then_resolution_before_enqueue_is_rejected_at_dispatch() {
        let f = fixture();
        let request = permission(&f);
        // Exact retained discovery, followed by resolve, followed by the delayed
        // old append: the two original non-atomic producer steps can race.
        f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'permission',?3,?4)",params![f.call,format!("permission:{}",request.id),json!({"id":request.id,"state":"pending"}).to_string(),now_ms()]).unwrap();
        permission_decide(&f, &request.id, Some("yes"));
        crate::companion::queue_append(
            &f.s,
            &f.call,
            "commentary",
            None,
            "An action requested your approval.",
            &format!("permission_{}", request.id),
        )
        .unwrap();
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        notice(&f);
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
    }

    #[test]
    fn permission_observed_decision_survives_global_audit_retention_without_repeating_context() {
        let f = fixture();
        let request = permission(&f);
        notice(&f);
        take_ready(&f.s, &f.call).unwrap().unwrap();
        permission_decide(&f, &request.id, Some("yes"));
        notice(&f);
        f.s.db
            .lock()
            .unwrap()
            .execute("DELETE FROM audit WHERE kind='permission.decided'", [])
            .unwrap();
        let revision = take_ready(&f.s, &f.call).unwrap().unwrap();
        assert!(
            revision["commands"][0]["content"]
                .as_str()
                .unwrap()
                .contains("allowed")
        );
        notice(&f);
        assert!(take_ready(&f.s, &f.call).unwrap().is_none());
        assert_eq!(f.s.db.lock().unwrap().query_row("SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='permission' AND json_extract(data,'$.state')='resolved'",[&f.call],|r|r.get::<_,i64>(0)).unwrap(),1);
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
