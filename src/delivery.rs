//! Durable browser intake and per-recipient delivery. Receiving is not incorporation.
use crate::{
    auth, driver,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use anyhow::{Result, ensure};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::watch;

pub const METHOD: &str = "_session/steering";
pub const RECEIPTS: &str = "io.github.bkrabach.agent-runtime";
pub fn supported(init: &Value) -> bool {
    init["_meta"]["steering"]["supported"] == true
}

/// Process-local liveness, published only by the initialized active prompt.
/// It is never recovered from a historical session or a persisted capability.
#[derive(Clone)]
pub(crate) struct ActiveSteering {
    pub target_turn: String,
    pub session_id: String,
}
pub(crate) struct SteeringGuard {
    state: Arc<AppState>,
    key: (String, String),
    target: ActiveSteering,
}
impl SteeringGuard {
    pub(crate) fn open(
        state: &Arc<AppState>,
        room: &str,
        agent: &str,
        target_turn: &str,
        session_id: &str,
        supported: bool,
    ) -> Self {
        let guard = Self {
            state: state.clone(),
            key: (room.into(), agent.into()),
            target: ActiveSteering {
                target_turn: target_turn.into(),
                session_id: session_id.into(),
            },
        };
        if supported {
            state
                .active_steering
                .lock()
                .unwrap()
                .insert(guard.key.clone(), guard.target.clone());
        }
        guard
    }
    pub(crate) fn close(&self) {
        let mut active = self.state.active_steering.lock().unwrap();
        if active.get(&self.key).is_some_and(|v| {
            v.target_turn == self.target.target_turn && v.session_id == self.target.session_id
        }) {
            active.remove(&self.key);
        }
    }
}
impl Drop for SteeringGuard {
    fn drop(&mut self) {
        self.close();
    }
}
pub(crate) fn active_steering(state: &AppState, room: &str, agent: &str) -> Option<ActiveSteering> {
    state
        .active_steering
        .lock()
        .unwrap()
        .get(&(room.into(), agent.into()))
        .cloned()
}
/// Only the local Companion intake may set this window; it is not a public
/// message field or an ACP extension. The timestamp controls ordinary fallback.
pub(crate) struct EarlyDelivery {
    pub active: ActiveSteering,
    pub ordinary_not_before: i64,
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS input_authority(message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,browser_hash TEXT,token_hash TEXT)")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS input_deliveries(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,agent_id TEXT NOT NULL,participation TEXT NOT NULL,state TEXT NOT NULL,mode TEXT,target_turn TEXT,session_id TEXT,endpoint TEXT,receipt TEXT,attention TEXT,updated_at INTEGER NOT NULL,UNIQUE(message_id,agent_id)); CREATE INDEX IF NOT EXISTS input_delivery_queue ON input_deliveries(conversation_id,sequence);")?;
    for (name, declaration) in [
        ("ordinary_not_before", "INTEGER NOT NULL DEFAULT 0"),
        ("steering_target_turn", "TEXT"),
        ("steering_session_id", "TEXT"),
    ] {
        let exists = db
            .prepare("PRAGMA table_info(input_deliveries)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == name);
        if !exists {
            db.execute_batch(&format!(
                "ALTER TABLE input_deliveries ADD COLUMN {name} {declaration}"
            ))?;
        }
    }
    crate::huddle_delivery::migrate(db)?;
    // No retry follows a crash after transmission. Keep the session binding and
    // source text so its owner can inspect the outcome before proceeding.
    db.execute("UPDATE input_deliveries SET state=CASE WHEN state='preparing' THEN 'notAccepted' ELSE 'unknown' END,updated_at=?1 WHERE state IN ('preparing','offering','submitted','received_remote','remoteQueued') OR (state='incorporated' AND target_turn IN (SELECT id FROM messages WHERE status='streaming'))",[now_ms()])?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Input {
    pub id: String,
    pub message_id: String,
    pub seq: i64,
    pub room: String,
    pub agent: String,
    pub actor: String,
    pub text: String,
    pub participation: String,
    pub attachments: Vec<String>,
}
fn input(db: &Connection, id: &str) -> Result<Input> {
    let mut i = db.query_row("SELECT d.id,d.message_id,m.seq,d.conversation_id,d.agent_id,m.actor,m.text,d.participation FROM input_deliveries d JOIN messages m ON m.id=d.message_id WHERE d.id=?1",[id],|r|Ok(Input{id:r.get(0)?,message_id:r.get(1)?,seq:r.get(2)?,room:r.get(3)?,agent:r.get(4)?,actor:r.get(5)?,text:r.get(6)?,participation:r.get(7)?,attachments:vec![]}))?;
    i.attachments = db
        .prepare("SELECT id FROM attachments WHERE message_id=?1 ORDER BY created_at,id")?
        .query_map([&i.message_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(i)
}
fn pending(
    db: &Connection,
    room: &str,
    agent: Option<&str>,
    include_peers: bool,
) -> Result<Option<Input>> {
    db.execute("UPDATE input_deliveries SET state='cancelled',updated_at=?2 WHERE conversation_id=?1 AND state IN ('received','promptRequired') AND NOT EXISTS(SELECT 1 FROM huddle_peer_deliveries p WHERE p.input_id=input_deliveries.id) AND (NOT EXISTS(SELECT 1 FROM room_members r JOIN messages m ON m.actor=r.username WHERE r.conversation_id=input_deliveries.conversation_id AND m.id=input_deliveries.message_id) OR NOT EXISTS(SELECT 1 FROM room_agents WHERE conversation_id=input_deliveries.conversation_id AND agent_id=input_deliveries.agent_id))",params![room,now_ms()])?;
    loop {
        // An uncertain earlier input blocks only this agent in this conversation.
        let id:Option<String>=db.query_row("SELECT d.id FROM input_deliveries d JOIN messages m ON m.id=d.message_id WHERE d.conversation_id=?1 AND (?2 IS NULL OR d.agent_id=?2) AND d.state IN ('received','promptRequired') AND (?3 OR NOT EXISTS(SELECT 1 FROM huddle_peer_deliveries p WHERE p.input_id=d.id)) AND (EXISTS(SELECT 1 FROM room_members WHERE conversation_id=d.conversation_id AND username=m.actor) OR EXISTS(SELECT 1 FROM huddle_peer_deliveries p WHERE p.input_id=d.id)) AND EXISTS(SELECT 1 FROM room_agents WHERE conversation_id=d.conversation_id AND agent_id=d.agent_id) AND NOT EXISTS(SELECT 1 FROM input_deliveries x WHERE x.conversation_id=d.conversation_id AND x.agent_id=d.agent_id AND x.state IN ('unknown','notAccepted')) ORDER BY d.sequence LIMIT 1",params![room,agent,include_peers],|r|r.get(0)).optional()?;
        if let Some(id) = id {
            let input = input(db, &id)?;
            if input.participation == "peer" && !crate::huddle_delivery::valid_input(db, &id)? {
                db.execute(
                    "UPDATE input_deliveries SET state='cancelled',updated_at=?2 WHERE id=?1",
                    params![id, now_ms()],
                )?;
                continue;
            }
            return Ok(Some(input));
        }
        return Ok(None);
    }
}
fn delivery_window(
    db: &Connection,
    input: &Input,
) -> Result<(i64, Option<String>, Option<String>)> {
    Ok(db.query_row("SELECT ordinary_not_before,steering_target_turn,steering_session_id FROM input_deliveries WHERE id=?1", [&input.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}
pub fn next(db: &Connection, room: &str, agent: Option<&str>) -> Result<Option<Input>> {
    let Some(input) = pending(db, room, agent, true)? else {
        return Ok(None);
    };
    Ok((delivery_window(db, &input)?.0 <= now_ms()).then_some(input))
}
pub(crate) fn next_steering(
    db: &Connection,
    room: &str,
    agent: &str,
    target: &str,
    session: &str,
) -> Result<Option<Input>> {
    let Some(input) = pending(db, room, Some(agent), false)? else {
        return Ok(None);
    };
    if input.participation == "peer" {
        return Ok(None);
    }
    let (not_before, bound_target, bound_session) = delivery_window(db, &input)?;
    Ok((not_before <= now_ms()
        || (bound_target.as_deref() == Some(target) && bound_session.as_deref() == Some(session)))
    .then_some(input))
}
fn deferred_until(db: &Connection, room: &str) -> Result<Option<i64>> {
    let Some(input) = pending(db, room, None, true)? else {
        return Ok(None);
    };
    let at = delivery_window(db, &input)?.0;
    Ok((at > now_ms()).then_some(at))
}
/// Explain the existing no-replay fence without changing or acknowledging it.
/// A locally received input behind this fence has not reached a model turn.
pub(crate) fn blockers(state: &AppState, room: &str, agent: &str) -> Result<Vec<Value>> {
    let db = state.db.lock().unwrap();
    Ok(db.prepare("SELECT id,message_id,state,target_turn FROM input_deliveries WHERE conversation_id=?1 AND agent_id=?2 AND state IN ('unknown','notAccepted') ORDER BY sequence")?
        .query_map(params![room,agent],|r|Ok(json!({"input_id":r.get::<_,String>(0)?,"message_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"target_turn":r.get::<_,Option<String>>(3)?})))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
pub fn set(state: &AppState, id: &str, status: &str) -> Result<()> {
    state.db.lock().unwrap().execute(
        "UPDATE input_deliveries SET state=?2,updated_at=?3 WHERE id=?1",
        params![id, status, now_ms()],
    )?;
    state.changed();
    Ok(())
}
pub fn bind(
    state: &AppState,
    id: &str,
    turn: &str,
    sid: &str,
    endpoint: &str,
    mode: &str,
    status: &str,
) -> Result<()> {
    state.db.lock().unwrap().execute("UPDATE input_deliveries SET state=?2,target_turn=?3,session_id=?4,endpoint=?5,mode=?6,updated_at=?7 WHERE id=?1",params![id,status,turn,sid,endpoint,mode,now_ms()])?;
    state.changed();
    Ok(())
}
pub fn attention(state: &AppState, id: &str, offer: &Value) -> Result<()> {
    state.db.lock().unwrap().execute(
        "UPDATE input_deliveries SET attention=?2 WHERE id=?1",
        params![id, offer.to_string()],
    )?;
    Ok(())
}
pub fn restored_attention(state: &AppState, id: &str) -> Result<Option<Value>> {
    let value:Option<String>=state.db.lock().unwrap().query_row("SELECT attention FROM input_deliveries WHERE id=?1 AND state IN ('preparing','promptRequired')",[id],|r|r.get(0)).optional()?.flatten();
    value.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
}
pub fn receipt(state: &AppState, turn: &str, value: &Value) -> Result<()> {
    let (Some(id), Some(status)) = (value["inputId"].as_str(), value["state"].as_str()) else {
        return Ok(());
    };
    let status = match status {
        "received" => "received_remote",
        "submitted" | "remoteQueued" | "incorporated" | "notAccepted" | "unknown" => status,
        _ => return Ok(()),
    };
    let identifier = |key: &str| {
        value[key]
            .as_str()
            .map(|s| s.chars().take(1024).collect::<String>())
    };
    let safe = json!({"inputId":id,"state":status,"turnId":identifier("turnId"),"sequence":value["sequence"].as_u64(),"responseId":identifier("responseId"),"recorded":value["recorded"].as_bool()});
    state.db.lock().unwrap().execute("UPDATE input_deliveries SET state=?3,receipt=?4,updated_at=?5 WHERE id=?1 AND target_turn=?2 AND state IN ('submitted','received_remote','remoteQueued','incorporated') AND (state<>'incorporated' OR ?3 IN ('unknown','notAccepted','incorporated')) AND (state<>'remoteQueued' OR ?3 IN ('remoteQueued','unknown','notAccepted','incorporated'))",params![id,turn,status,safe.to_string(),now_ms()])?;
    state.changed();
    Ok(())
}
pub fn finish(state: &AppState, turn: &str, success: bool) -> Result<()> {
    let db = state.db.lock().unwrap();
    if success {
        db.execute("UPDATE input_deliveries SET state=CASE WHEN mode='prompt' AND state IN ('submitted','received_remote') THEN 'completed' ELSE 'unknown' END,updated_at=?2 WHERE target_turn=?1 AND state IN ('submitted','received_remote','remoteQueued')",params![turn,now_ms()])?;
    } else {
        db.execute("UPDATE input_deliveries SET state=CASE WHEN state='preparing' THEN 'notAccepted' ELSE 'unknown' END,updated_at=?2 WHERE target_turn=?1 AND state IN ('preparing','offering','submitted','received_remote','remoteQueued','incorporated')",params![turn,now_ms()])?;
    }
    Ok(())
}
pub fn list(state: &AppState, room: &str) -> Result<Vec<Value>> {
    let db = state.db.lock().unwrap();
    let mut q=db.prepare("SELECT d.id,d.message_id,d.agent_id,d.state,d.mode,d.target_turn,d.session_id,d.sequence,d.updated_at,d.receipt,m.seq FROM input_deliveries d JOIN messages m ON m.id=d.message_id WHERE d.conversation_id=?1 AND (d.state IN ('received','preparing','offering','submitted','received_remote','remoteQueued','promptRequired','unknown','notAccepted','ack_pending') OR d.sequence IN (SELECT sequence FROM input_deliveries WHERE conversation_id=?1 ORDER BY sequence DESC LIMIT 256)) ORDER BY d.sequence DESC")?;
    Ok(q.query_map([room],|r|Ok(json!({"id":r.get::<_,String>(0)?,"message_id":r.get::<_,String>(1)?,"agent_id":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"mode":r.get::<_,Option<String>>(4)?,"target_turn":r.get::<_,Option<String>>(5)?,"session_id":r.get::<_,Option<String>>(6)?,"sequence":r.get::<_,i64>(7)?,"updated_at":r.get::<_,i64>(8)?,"source_sequence":r.get::<_,i64>(10)?,"receipt":r.get::<_,Option<String>>(9)?.and_then(|s|serde_json::from_str::<Value>(&s).ok())})))?.collect::<rusqlite::Result<_>>()?)
}
pub async fn read(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    s.require_member(&room, &p)?;
    Ok(Json(json!({"items":list(&s,&room)?})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    disposition: String,
}
pub async fn resolve(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((room, id)): Path<(String, String)>,
    Json(body): Json<Resolution>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&room, &p)?;
    if body.disposition != "proceed_without_replay" {
        return Err(ApiError::bad(
            "Use proceed_without_replay after reviewing the previous work",
        ));
    }
    let running = s.running.lock().unwrap();
    if running.contains_key(&room) {
        return Err(ApiError::busy());
    }
    let changed=s.db.lock().unwrap().execute("UPDATE input_deliveries SET state='ack_pending',updated_at=?3 WHERE id=?1 AND conversation_id=?2 AND state IN ('unknown','notAccepted') AND message_id IN (SELECT id FROM messages WHERE actor=?4)",params![id,room,now_ms(),p.actor])?;
    if changed == 0 {
        return Err(ApiError::bad(
            "Only the message author can acknowledge its uncertain delivery",
        ));
    }
    s.status(
        &room,
        "working",
        "Reviewing the saved delivery without resending it…",
    )?;
    drop(running);
    s.audit(
        "input.acknowledged",
        Some(&room),
        json!({"input_id":id,"actor":p.actor,"disposition":body.disposition}),
    )?;
    launch(&s, &room);
    s.changed();
    Ok(Json(json!({"acknowledged":true,"replayed":false})))
}
pub struct Acknowledgment {
    pub id: String,
    pub session: Option<String>,
    pub mode: Option<String>,
    pub endpoint: Option<String>,
}
pub fn pending_ack(state: &AppState, room: &str, agent: &str) -> Result<Vec<Acknowledgment>> {
    let db = state.db.lock().unwrap();
    let mut q=db.prepare("SELECT id,session_id,mode,endpoint FROM input_deliveries WHERE conversation_id=?1 AND agent_id=?2 AND state='ack_pending' ORDER BY sequence")?;
    Ok(q.query_map(params![room, agent], |r| {
        Ok(Acknowledgment {
            id: r.get(0)?,
            session: r.get(1)?,
            mode: r.get(2)?,
            endpoint: r.get(3)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

/// Caller holds running before the database lock at intake and finalization.
pub fn launch(state: &Arc<AppState>, room: &str) {
    let mut running = state.running.lock().unwrap();
    if running.contains_key(room) {
        return;
    }
    let (tx, rx) = watch::channel(false);
    running.insert(room.to_owned(), tx);
    let state = state.clone();
    let room = room.to_owned();
    tokio::spawn(async move {
        worker(state, room, rx).await;
    });
}
pub fn resume(state: &Arc<AppState>) {
    let rooms: Vec<String> = {
        let db = state.db.lock().unwrap();
        db.prepare("SELECT DISTINCT conversation_id FROM input_deliveries WHERE state IN ('received','promptRequired','ack_pending')").and_then(|mut q|q.query_map([],|r|r.get(0))?.collect()).unwrap_or_default()
    };
    for room in rooms {
        launch(state, &room);
    }
}
fn next_work(db: &Connection, room: &str) -> Result<Option<(Input, bool)>> {
    let ack:Option<String>=db.query_row("SELECT d.id FROM input_deliveries d JOIN messages m ON m.id=d.message_id WHERE d.conversation_id=?1 AND d.state='ack_pending' AND EXISTS(SELECT 1 FROM room_members WHERE conversation_id=d.conversation_id AND username=m.actor) ORDER BY d.sequence LIMIT 1",[room],|r|r.get(0)).optional()?;
    if let Some(id) = ack {
        return Ok(Some((input(db, &id)?, true)));
    }
    Ok(next(db, room, None)?.map(|i| (i, false)))
}
async fn worker(state: Arc<AppState>, room: String, mut cancel: watch::Receiver<bool>) {
    let mut failed = false;
    loop {
        // A normal next prompt takes its own fair place among other rooms.
        // Steering within one active turn keeps that turn's existing permit.
        let permit = if *cancel.borrow() {
            None
        } else {
            tokio::select! {p=state.turn_gate.clone().acquire_owned()=>p.ok(),_=cancel.changed()=>None}
        };
        let mut wait_until = None;
        let next = {
            let mut running = state.running.lock().unwrap();
            let db = state.db.lock().unwrap();
            if *cancel.borrow() || permit.is_none() {
                let _=db.execute("UPDATE input_deliveries SET state='cancelled',updated_at=?2 WHERE conversation_id=?1 AND state IN ('received','promptRequired')",params![room,now_ms()]);
            }
            let next = if !*cancel.borrow() && permit.is_some() {
                match next_work(&db, &room) {
                    Ok(next) => next,
                    Err(_) => {
                        failed = true;
                        None
                    }
                }
            } else {
                None
            };
            if next.is_none() && !failed && !*cancel.borrow() && permit.is_some() {
                match deferred_until(&db, &room) {
                    Ok(at) => wait_until = at,
                    Err(_) => failed = true,
                }
            }
            if next.is_none() && wait_until.is_none() {
                let uncertain:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM input_deliveries WHERE conversation_id=?1 AND state IN ('unknown','notAccepted'))",[&room],|r|r.get(0)).unwrap_or(true);
                let stopped = *cancel.borrow();
                let status = if failed || uncertain || stopped {
                    "error"
                } else {
                    "idle"
                };
                let note = if stopped {
                    "You stopped this turn. Work already performed is not undone."
                } else if uncertain {
                    "A delivery outcome needs review. Messages are saved and have not been sent again."
                } else if failed {
                    "An agent could not finish. Review its delivery status or connection before continuing."
                } else {
                    ""
                };
                let _ = db.execute(
                    "UPDATE conversations SET status=?2,note=?3,updated_at=?4 WHERE id=?1",
                    params![room, status, note, now_ms()],
                );
                running.remove(&room);
            }
            next
        };
        if let Some(at) = wait_until {
            // A terminal/promptRequired race may leave an early Companion input
            // queued. Preserve its original grace deadline even after restart.
            drop(permit);
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(at.saturating_sub(now_ms()).max(0) as u64)) => {},
                _ = cancel.changed() => {},
            }
            continue;
        }
        let Some((input, recovery)) = next else {
            state.changed();
            return;
        };
        let result = if recovery {
            driver::reconcile(state.clone(), input.clone()).await
        } else {
            driver::deliver(state.clone(), input.clone()).await
        };
        if result.is_err() {
            failed = true;
            if recovery {
                let _=state.db.lock().unwrap().execute("UPDATE input_deliveries SET state='unknown',updated_at=?3 WHERE conversation_id=?1 AND agent_id=?2 AND state='ack_pending'",params![room,input.agent,now_ms()]);
            }
        }
        state.changed();
    }
}

pub fn safe_history(state: &AppState, room: &str, agent: &str, trigger: i64) -> Result<Vec<Value>> {
    let excluded: Vec<String> = {
        let db = state.db.lock().unwrap();
        db.prepare("SELECT message_id FROM input_deliveries WHERE conversation_id=?1 AND agent_id=?2 AND state IN ('unknown','ack_pending','dismissed','cancelled')")?.query_map(params![room,agent],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    Ok(state
        .messages(room, 100)?
        .into_iter()
        .filter(|m| {
            m["role"] != "user"
                || (m["seq"].as_i64().unwrap_or(0) <= trigger
                    && !excluded.iter().any(|id| m["id"] == *id))
        })
        .collect())
}
pub fn check_owner(state: &AppState, input: &Input) -> Result<()> {
    if input.participation == "peer" {
        ensure!(
            crate::huddle_delivery::valid_input(&state.db.lock().unwrap(), &input.id)?,
            "Peer source or original recipient changed"
        );
        return Ok(());
    }
    ensure!(
        state.member(&input.room, &input.actor)?,
        "The source author is no longer a room member"
    );
    ensure!(
        state
            .room_agents(&input.room)?
            .iter()
            .any(|a| a["id"] == input.agent),
        "The agent is no longer attached to this room"
    );
    Ok(())
}

/// Credential references never leave this private table or enter ACP metadata.
pub fn authorized(state: &AppState, message_id: &str, actor: &str) -> Result<bool> {
    let db = state.db.lock().unwrap();
    authorized_in(state, &db, message_id, actor)
}

/// Use the same credential rules while the caller holds its mutation transaction.
pub(crate) fn authorized_in(
    state: &AppState,
    db: &Connection,
    message_id: &str,
    actor: &str,
) -> Result<bool> {
    if let Some((original, owner)) =
        crate::huddle_delivery::original_authority(db, message_id, actor)?
    {
        return Ok(!original.is_empty() && authorized_in(state, db, &original, &owner)?);
    }
    let row: Option<(Option<String>, Option<String>)> = db
        .query_row(
            "SELECT browser_hash,token_hash FROM input_authority WHERE message_id=?1",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((browser, token)) = row else {
        return Ok(false);
    };
    if let Some(hash) = browser {
        return Ok(state.config.allows_user(actor)&&db.query_row("SELECT EXISTS(SELECT 1 FROM browser_sessions WHERE hash=?1 AND owner=?2 AND expires_at>?3)",params![hash,actor,now_ms()],|r|r.get(0))?);
    }
    if let Some(hash) = token {
        let row: Option<(String, String)> = db
            .query_row(
                "SELECT id,scopes FROM tokens WHERE hash=?1 AND expires_at>?2",
                params![hash, now_ms()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, scopes)) = row {
            let scopes: Vec<String> = serde_json::from_str(&scopes)?;
            let admin = scopes.iter().any(|s| s == "admin");
            return Ok((admin || scopes.iter().any(|s| s == "chat"))
                && actor
                    == if admin {
                        state.config.owner.clone()
                    } else {
                        format!("token:{id}")
                    });
        }
    }
    Ok(false)
}

#[cfg(test)]
mod continuation_tests {
    use super::*;

    fn database(path: &std::path::Path) -> Connection {
        let db = Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS conversations(id TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS messages(seq INTEGER PRIMARY KEY,id TEXT UNIQUE,conversation_id TEXT,actor TEXT,text TEXT,status TEXT); CREATE TABLE IF NOT EXISTS room_members(conversation_id TEXT,username TEXT); CREATE TABLE IF NOT EXISTS room_agents(conversation_id TEXT,agent_id TEXT); CREATE TABLE IF NOT EXISTS attachments(id TEXT,message_id TEXT,created_at INTEGER);").unwrap();
        migrate(&db).unwrap();
        db
    }
    #[test]
    fn durable_early_input_is_bound_to_exact_active_turn_and_session_and_fallback_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("intake.sqlite3");
        let db = database(&path);
        let due = now_ms() + 60_000;
        db.execute_batch("INSERT INTO conversations VALUES('room'); INSERT INTO room_members VALUES('room','human'); INSERT INTO room_agents VALUES('room','agent'); INSERT INTO messages VALUES(1,'source','room','human','A distinct correction and another question.','complete'); INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES('input','source','room','agent','direct','received',0);").unwrap();
        db.execute("UPDATE input_deliveries SET ordinary_not_before=?1,steering_target_turn='turn-a',steering_session_id='session-a'", [due]).unwrap();
        assert!(next(&db, "room", None).unwrap().is_none());
        assert!(
            next_steering(&db, "room", "agent", "turn-b", "session-a")
                .unwrap()
                .is_none()
        );
        assert!(
            next_steering(&db, "room", "agent", "turn-a", "session-b")
                .unwrap()
                .is_none()
        );
        assert!(
            next_steering(&db, "room", "other-agent", "turn-a", "session-a")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            next_steering(&db, "room", "agent", "turn-a", "session-a")
                .unwrap()
                .unwrap()
                .message_id,
            "source"
        );
        assert_eq!(deferred_until(&db, "room").unwrap(), Some(due));
        drop(db);
        let db = database(&path);
        assert!(
            next(&db, "room", None).unwrap().is_none(),
            "Restart must retain ordinary deadline"
        );
        db.execute(
            "UPDATE input_deliveries SET ordinary_not_before=?1",
            [now_ms() - 1],
        )
        .unwrap();
        assert_eq!(
            next(&db, "room", None).unwrap().unwrap().text,
            "A distinct correction and another question."
        );
        // A lost acknowledgment is uncertain even if the old time has elapsed.
        db.execute("UPDATE input_deliveries SET state='received_remote',mode='steering',target_turn='turn-a',session_id='session-a'", []).unwrap();
        drop(db);
        let db = database(&path);
        assert_eq!(
            db.query_row("SELECT state FROM input_deliveries", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "unknown"
        );
        assert!(next(&db, "room", None).unwrap().is_none());
        assert!(
            next_steering(&db, "room", "agent", "turn-a", "session-a")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.query_row("SELECT text FROM messages WHERE id='source'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "A distinct correction and another question."
        );
    }
}
