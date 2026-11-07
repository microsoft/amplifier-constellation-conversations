//! Finite controls for one call's browser surface. Browser acknowledgments are
//! evidence, not a license to retry an uncertain camera effect.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use anyhow::Result;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const FRESH_MS: i64 = 30_000;
const TTL_MS: i64 = 30_000;
const IMAGE_LIMIT: i64 = 512 * 1024;
const VIEW_ACTIONS: &[&str] = &[
    "scroll_up",
    "scroll_down",
    "next_card",
    "previous_card",
    "show_shared",
    "show_transcript",
    "focus_card",
];

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS companion_feedback(call_id TEXT NOT NULL REFERENCES companion_calls(id) ON DELETE CASCADE,operation_id TEXT NOT NULL,delegation_id TEXT NOT NULL,owner TEXT NOT NULL,text TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(call_id,operation_id),UNIQUE(call_id,delegation_id));")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS companion_surface_capabilities(call_id TEXT PRIMARY KEY REFERENCES companion_calls(id) ON DELETE CASCADE,image_input INTEGER,observed_at INTEGER NOT NULL);")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS companion_surfaces(call_id TEXT PRIMARY KEY REFERENCES companion_calls(id) ON DELETE CASCADE,browser_id TEXT NOT NULL,session_hash TEXT NOT NULL,camera_enabled INTEGER NOT NULL,capture_consent INTEGER NOT NULL,user_speaking INTEGER,agent_speaking INTEGER,updated_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS companion_surface_commands(call_id TEXT NOT NULL REFERENCES companion_calls(id) ON DELETE CASCADE,operation_id TEXT NOT NULL,delegation_id TEXT NOT NULL,browser_id TEXT NOT NULL,session_hash TEXT NOT NULL,kind TEXT NOT NULL,command TEXT NOT NULL,state TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,accepted_at INTEGER,attachment_ids TEXT NOT NULL DEFAULT '[]',PRIMARY KEY(call_id,operation_id)); CREATE UNIQUE INDEX IF NOT EXISTS companion_one_capture_per_delegation ON companion_surface_commands(call_id,delegation_id) WHERE kind='capture';")?;
    let columns = db
        .prepare("PRAGMA table_info(companion_surface_commands)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|c| c == "session_hash") {
        db.execute("ALTER TABLE companion_surface_commands ADD COLUMN session_hash TEXT NOT NULL DEFAULT ''",[])?;
    }
    db.execute("UPDATE companion_surface_commands SET state='unknown' WHERE state IN ('queued','accepted') OR (kind='capture' AND state='executed')", [])?;
    db.execute("DELETE FROM companion_surfaces", [])?;
    Ok(())
}

/// Capture the actual initialize result for the ACP connection receiving this
/// call's MCP descriptor. A browser probe or static profile is not authoritative.
pub(crate) fn record_capabilities(
    s: &AppState,
    room: &str,
    agent: &str,
    capabilities: &Value,
) -> Result<()> {
    let image = capabilities["promptCapabilities"]["image"].as_bool();
    s.db.lock().unwrap().execute("INSERT INTO companion_surface_capabilities(call_id,image_input,observed_at) SELECT id,?3,?4 FROM companion_calls WHERE conversation_id=?1 AND agent_id=?2 AND state IN ('connecting','connected') ON CONFLICT(call_id) DO UPDATE SET image_input=excluded.image_input,observed_at=excluded.observed_at",params![room,agent,image,now_ms()])?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presence {
    browser_id: String,
    camera_enabled: bool,
    capture_consent: bool,
    user_speaking: Option<bool>,
    agent_speaking: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    operation_id: String,
    delegation_id: String,
    action: String,
    card_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    operation_id: String,
    delegation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hangup {
    operation_id: String,
    delegation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feedback {
    operation_id: String,
    delegation_id: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserQuery {
    browser_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    browser_id: String,
    state: String,
    #[serde(default)]
    attachment_ids: Vec<String>,
}

fn valid_identity(operation: &str, delegation: &str) -> bool {
    uuid::Uuid::parse_str(operation).is_ok() && !delegation.is_empty() && delegation.len() <= 160
}
fn valid_feedback(f: &Feedback) -> bool {
    valid_identity(&f.operation_id, &f.delegation_id)
        && !f.text.trim().is_empty()
        && f.text.len() <= 4000
}
fn valid_view(v: &View) -> bool {
    valid_identity(&v.operation_id, &v.delegation_id)
        && VIEW_ACTIONS.contains(&v.action.as_str())
        && if v.action == "focus_card" {
            v.card_id.as_deref().is_some_and(|x| {
                !x.is_empty() && x.len() <= 20 && x.parse::<i64>().is_ok_and(|v| v > 0)
            })
        } else {
            v.card_id.is_none()
        }
}
fn browser(
    s: &AppState,
    id: &str,
    h: &HeaderMap,
    mutation: bool,
) -> ApiResult<(auth::Principal, String)> {
    let p = auth::check(s, h, "chat", mutation)?;
    // The MCP credential and installation control token cannot attest that a
    // human enabled this browser's camera.
    if p.session_hash.is_none() {
        return Err(ApiError::forbidden());
    }
    let room: Option<String> =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT conversation_id FROM companion_calls WHERE id=?1 AND owner=?2",
                params![id, p.actor],
                |r| r.get(0),
            )
            .optional()?;
    let room = room.ok_or_else(ApiError::missing)?;
    if !crate::companion::room_identity_valid(s, &room)? {
        return Err(ApiError::forbidden());
    }
    Ok((p, room))
}
fn active(s: &AppState, id: &str) -> Result<bool> {
    if !crate::companion_tools::enabled(s)? {
        return Ok(false);
    }
    Ok(s.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM companion_calls c JOIN companion_tool_grants g ON g.call_id=c.id WHERE c.id=?1 AND c.state IN ('connecting','connected') AND c.heartbeat_at>=?2 AND g.port>0)", params![id,now_ms()-FRESH_MS],|r|r.get(0))?)
}
pub(crate) fn activity(s: &AppState, id: &str) -> Result<(Option<bool>, Option<bool>, i64)> {
    Ok(s.db.lock().unwrap().query_row("SELECT user_speaking,agent_speaking,updated_at FROM companion_surfaces WHERE call_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.unwrap_or((None,None,0)))
}
fn surface(s: &AppState, id: &str, capture: bool) -> Result<Option<String>> {
    if !active(s, id)? {
        return Ok(None);
    }
    Ok(s.db.lock().unwrap().query_row("SELECT browser_id FROM companion_surfaces WHERE call_id=?1 AND updated_at>=?2 AND (?3=0 OR (camera_enabled=1 AND capture_consent=1))", params![id,now_ms()-FRESH_MS,capture],|r|r.get(0)).optional()?)
}
fn source_authorized(
    s: &AppState,
    id: &str,
    owner: &str,
    agent: &str,
    room: &str,
    delegation: &str,
) -> Result<bool> {
    let source:Option<String>=s.db.lock().unwrap().query_row("SELECT d.message_id FROM companion_delegations d JOIN messages m ON m.id=d.message_id JOIN companion_calls c ON c.id=d.call_id WHERE d.call_id=?1 AND d.id=?2 AND d.state='dispatched' AND c.agent_id=?3 AND c.owner=?4 AND c.conversation_id=?5 AND m.role='user' AND m.actor=?4 AND m.conversation_id=?5 AND EXISTS(SELECT 1 FROM input_deliveries i WHERE i.message_id=m.id AND i.agent_id=?3 AND i.state IN ('submitted','received_remote','incorporated'))",params![id,delegation,agent,owner,room],|r|r.get(0)).optional()?;
    match source {
        Some(mid) => crate::delivery::authorized(s, &mid, owner),
        None => Ok(false),
    }
}
fn card_exists(s: &AppState, id: &str, card: &str) -> Result<bool> {
    Ok(s.db.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_events WHERE call_id=?1 AND seq=?2 AND kind='card')",
        params![id, card],
        |r| r.get(0),
    )?)
}
pub fn permission_valid(
    s: &AppState,
    id: &str,
    owner: &str,
    agent: &str,
    room: &str,
    tool: &str,
    args: Value,
) -> Result<bool> {
    if tool == "companion_surface_status" {
        return Ok(args.as_object().is_some_and(|a| {
            a.len() == 1
                && a.get("operation_id")
                    .and_then(Value::as_str)
                    .is_some_and(|x| uuid::Uuid::parse_str(x).is_ok())
        }));
    }
    let (delegation, capture) = match tool {
        "companion_view" => {
            let Ok(v) = serde_json::from_value::<View>(args) else {
                return Ok(false);
            };
            if !valid_view(&v)
                || v.card_id
                    .as_ref()
                    .is_some_and(|card| !card_exists(s, id, card).unwrap_or(false))
            {
                return Ok(false);
            };
            (v.delegation_id, false)
        }
        "companion_capture" => {
            let Ok(c) = serde_json::from_value::<Capture>(args) else {
                return Ok(false);
            };
            if !valid_identity(&c.operation_id, &c.delegation_id) {
                return Ok(false);
            };
            (c.delegation_id, true)
        }
        "companion_hangup" => {
            let Ok(h) = serde_json::from_value::<Hangup>(args) else {
                return Ok(false);
            };
            if !valid_identity(&h.operation_id, &h.delegation_id) {
                return Ok(false);
            }
            (h.delegation_id, false)
        }
        "companion_feedback" => {
            let Ok(f) = serde_json::from_value::<Feedback>(args) else {
                return Ok(false);
            };
            if !valid_feedback(&f) {
                return Ok(false);
            }
            (f.delegation_id, false)
        }
        _ => return Ok(false),
    };
    Ok(surface(s, id, capture)?.is_some()
        && source_authorized(s, id, owner, agent, room, &delegation)?)
}

fn reconcile(db: &Connection, id: &str) -> Result<()> {
    db.execute("UPDATE companion_surface_commands SET state='unknown' WHERE call_id=?1 AND (state IN ('queued','accepted') OR (kind='capture' AND state='executed')) AND (expires_at<?2 OR NOT EXISTS(SELECT 1 FROM companion_calls c WHERE c.id=?1 AND c.state IN ('connecting','connected')))",params![id,now_ms()])?;
    Ok(())
}
fn row(db: &Connection, id: &str, operation: &str) -> Result<Option<Value>> {
    reconcile(db, id)?;
    Ok(db.query_row("SELECT operation_id,delegation_id,browser_id,kind,command,state,created_at,expires_at,attachment_ids FROM companion_surface_commands WHERE call_id=?1 AND operation_id=?2",params![id,operation],|r|{
        let command:String=r.get(4)?; let attachments:String=r.get(8)?;
        Ok(json!({"operation_id":r.get::<_,String>(0)?,"delegation_id":r.get::<_,String>(1)?,"browser_id":r.get::<_,String>(2)?,"kind":r.get::<_,String>(3)?,"command":serde_json::from_str::<Value>(&command).unwrap_or(Value::Null),"state":r.get::<_,String>(5)?,"created_at":r.get::<_,i64>(6)?,"expires_at":r.get::<_,i64>(7)?,"artifact_ids":serde_json::from_str::<Value>(&attachments).unwrap_or(json!([])),"inspection":false,"guidance":"Only a confirmed accepted acknowledgment may execute once. Unknown outcomes must not be replayed. Uploaded means saved bytes, not inspection."}))
    }).optional()?)
}
fn record_event(
    db: &Connection,
    id: &str,
    operation: &str,
    state: &str,
    kind: &str,
    data: &Value,
) -> Result<()> {
    db.execute("INSERT OR IGNORE INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,?3,?4,?5)",params![id,format!("surface:{operation}:{state}"),kind,data.to_string(),now_ms()])?;
    Ok(())
}
pub async fn presence(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(presence): Json<Presence>,
) -> ApiResult<Json<Value>> {
    let (p, _) = browser(&s, &id, &h, true)?;
    if uuid::Uuid::parse_str(&presence.browser_id).is_err()
        || (presence.capture_consent && !presence.camera_enabled)
    {
        return Err(ApiError::bad(
            "Capture consent requires an already enabled camera",
        ));
    }
    if !active(&s, &id)? {
        return Err(ApiError::forbidden());
    }
    let db = s.db.lock().unwrap();
    // A fresh different browser cannot silently take over this call surface.
    let current: Option<(String, String, i64)> = db
        .query_row(
            "SELECT browser_id,session_hash,updated_at FROM companion_surfaces WHERE call_id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if current.is_some_and(|(browser, session, at)| {
        at >= now_ms() - FRESH_MS
            && (browser != presence.browser_id || Some(session) != p.session_hash)
    }) {
        return Err(ApiError::busy());
    }
    db.execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(call_id) DO UPDATE SET browser_id=excluded.browser_id,session_hash=excluded.session_hash,camera_enabled=excluded.camera_enabled,capture_consent=excluded.capture_consent,user_speaking=excluded.user_speaking,agent_speaking=excluded.agent_speaking,updated_at=excluded.updated_at",params![id,presence.browser_id,p.session_hash,presence.camera_enabled,presence.capture_consent,presence.user_speaking,presence.agent_speaking,now_ms()])?;
    Ok(Json(
        json!({"browser_id":presence.browser_id,"camera_enabled":presence.camera_enabled,"capture_consent":presence.capture_consent,"expires_at":now_ms()+FRESH_MS}),
    ))
}
fn enqueue(
    s: &AppState,
    id: &str,
    authority: (&str, &str, &str),
    operation: &str,
    delegation: &str,
    kind: &str,
    command: Value,
) -> ApiResult<Value> {
    let (owner, agent, room) = authority;
    let capture = kind == "capture";
    let browser = surface(s, id, capture)?.ok_or_else(|| {
        ApiError::bad(if capture {
            "No fresh browser with explicit camera capture consent"
        } else {
            "No fresh active browser surface"
        })
    })?;
    if !source_authorized(s, id, owner, agent, room, delegation)? {
        return Err(ApiError::forbidden());
    }
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_feedback WHERE call_id=?1 AND operation_id=?2)",
        params![id, operation],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(ApiError::busy());
    }
    if let Some(old) = row(&tx, id, operation)? {
        if old["kind"] != kind || old["delegation_id"] != delegation || old["command"] != command {
            return Err(ApiError::busy());
        }
        tx.commit()?;
        return Ok(old);
    }
    if capture {
        let previous:Option<String>=tx.query_row("SELECT operation_id FROM companion_surface_commands WHERE call_id=?1 AND delegation_id=?2 AND kind='capture'",params![id,delegation],|r|r.get(0)).optional()?;
        if let Some(previous) = previous {
            let receipt = row(&tx, id, &previous)?.unwrap();
            tx.commit()?;
            return Ok(receipt);
        }
    }
    let (total,recent,pending):(i64,i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(created_at>=?2),0),COALESCE(SUM(state IN ('queued','accepted')),0) FROM companion_surface_commands WHERE call_id=?1",params![id,now_ms()-60_000],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if total >= 256 || recent >= 16 || pending >= 8 {
        return Err(ApiError::bad("Surface command limit reached"));
    }
    let session:String = tx.query_row("SELECT session_hash FROM companion_surfaces WHERE call_id=?1 AND browser_id=?2 AND updated_at>=?3 AND (?4=0 OR (camera_enabled=1 AND capture_consent=1))",params![id,browser,now_ms()-FRESH_MS,capture],|r|r.get(0)).optional()?.ok_or_else(ApiError::forbidden)?;
    let at = now_ms();
    tx.execute("INSERT INTO companion_surface_commands(call_id,operation_id,delegation_id,browser_id,session_hash,kind,command,state,created_at,expires_at) VALUES(?1,?2,?3,?4,?9,?5,?6,'queued',?7,?8)",params![id,operation,delegation,browser,kind,command.to_string(),at,at+TTL_MS,session])?;
    let receipt = row(&tx, id, operation)?.unwrap();
    record_event(&tx, id, operation, "queued", "surface_command", &receipt)?;
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(receipt)
}
pub async fn view(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(v): Json<View>,
) -> ApiResult<Json<Value>> {
    let (owner, agent, room) = crate::companion_tools::authorize(&s, &id, &h)?;
    if !valid_view(&v) {
        return Err(ApiError::bad(
            "Choose one bounded surface action and an operation UUID",
        ));
    }
    if let Some(card) = &v.card_id
        && !card_exists(&s, &id, card)?
    {
        return Err(ApiError::missing());
    }
    Ok(Json(enqueue(
        &s,
        &id,
        (&owner, &agent, &room),
        &v.operation_id,
        &v.delegation_id,
        "view",
        json!({"action":v.action,"card_id":v.card_id}),
    )?))
}
pub async fn capture(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(c): Json<Capture>,
) -> ApiResult<Json<Value>> {
    let (owner, agent, room) = crate::companion_tools::authorize(&s, &id, &h)?;
    if !valid_identity(&c.operation_id, &c.delegation_id) {
        return Err(ApiError::bad(
            "Use an operation UUID and the exact active delegation",
        ));
    }
    Ok(Json(enqueue(
        &s,
        &id,
        (&owner, &agent, &room),
        &c.operation_id,
        &c.delegation_id,
        "capture",
        json!({"photos":1,"max_bytes":IMAGE_LIMIT,"upload_id":c.operation_id}),
    )?))
}
pub async fn hangup(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(request): Json<Hangup>,
) -> ApiResult<Json<Value>> {
    let (owner, agent, room) = crate::companion_tools::authorize(&s, &id, &h)?;
    if !valid_identity(&request.operation_id, &request.delegation_id) {
        return Err(ApiError::bad(
            "Use an operation UUID and the exact active delegation",
        ));
    }
    Ok(Json(enqueue(
        &s,
        &id,
        (&owner, &agent, &room),
        &request.operation_id,
        &request.delegation_id,
        "hangup",
        json!({"action":"end_call"}),
    )?))
}

fn feedback_receipt(operation: &str, delegation: &str, text: &str, at: i64) -> Value {
    json!({"operation_id":operation,"delegation_id":delegation,"state":"saved","scope":"private_installation","submitted_externally":false,"text":text,"created_at":at,"guidance":"Feedback is durably saved privately on this installation. It has not been sent to developers or any external service."})
}

pub async fn feedback(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(request): Json<Feedback>,
) -> ApiResult<Json<Value>> {
    let (owner, agent, room) = crate::companion_tools::authorize(&s, &id, &h)?;
    if !valid_feedback(&request) {
        return Err(ApiError::bad(
            "Use an operation UUID, the exact active delegation, and 1–4000 UTF-8 bytes of feedback",
        ));
    }
    if surface(&s, &id, false)?.is_none()
        || !source_authorized(&s, &id, &owner, &agent, &room, &request.delegation_id)?
    {
        return Err(ApiError::forbidden());
    }
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    if tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_surface_commands WHERE call_id=?1 AND operation_id=?2)",params![id,request.operation_id],|r|r.get::<_,bool>(0))? {
        return Err(ApiError::busy());
    }
    let previous: Option<(String, String, String, i64)> = tx.query_row(
        "SELECT operation_id,delegation_id,text,created_at FROM companion_feedback WHERE call_id=?1 AND (operation_id=?2 OR delegation_id=?3) ORDER BY operation_id=?2 DESC LIMIT 1",
        params![id,request.operation_id,request.delegation_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    if let Some((operation, delegation, text, at)) = previous {
        if delegation != request.delegation_id || text != request.text {
            return Err(ApiError::busy());
        }
        return Ok(Json(feedback_receipt(&operation, &delegation, &text, at)));
    }
    let total: i64 = tx.query_row(
        "SELECT COUNT(*) FROM companion_feedback WHERE call_id=?1",
        [&id],
        |r| r.get(0),
    )?;
    if total >= 64 {
        return Err(ApiError::bad("Feedback limit reached for this call"));
    }
    let at = now_ms();
    tx.execute("INSERT INTO companion_feedback(call_id,operation_id,delegation_id,owner,text,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,request.operation_id,request.delegation_id,owner,request.text,at])?;
    let receipt = feedback_receipt(
        &request.operation_id,
        &request.delegation_id,
        &request.text,
        at,
    );
    let card = json!({"title":"Feedback saved privately","text":format!("{}\n\nSaved on this installation. Not sent externally.",request.text),"conversation_id":room,"feedback_receipt":receipt,"observed_only":true});
    tx.execute(
        "INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'card',?3,?4)",
        params![
            id,
            format!("feedback:{}", request.operation_id),
            card.to_string(),
            at
        ],
    )?;
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(Json(receipt))
}
pub async fn pending(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Query(q): Query<BrowserQuery>,
) -> ApiResult<Json<Value>> {
    let (p, _) = browser(&s, &id, &h, false)?;
    let db = s.db.lock().unwrap();
    reconcile(&db, &id)?;
    let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM companion_surfaces WHERE call_id=?1 AND browser_id=?2 AND session_hash=?3)",params![id,q.browser_id,p.session_hash],|r|r.get(0))?;
    if !valid {
        return Err(ApiError::forbidden());
    }
    let ids=db.prepare("SELECT operation_id FROM companion_surface_commands WHERE call_id=?1 AND browser_id=?2 AND session_hash=?3 AND state='queued' ORDER BY created_at LIMIT 8")?.query_map(params![id,q.browser_id,p.session_hash],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let commands = ids
        .iter()
        .map(|operation| row(&db, &id, operation))
        .collect::<Result<Vec<_>>>()?;
    Ok(Json(json!({"commands":commands})))
}
pub async fn ack(
    State(s): State<Arc<AppState>>,
    Path((id, operation)): Path<(String, String)>,
    h: HeaderMap,
    Json(a): Json<Ack>,
) -> ApiResult<Json<Value>> {
    let (p, room) = browser(&s, &id, &h, true)?;
    if uuid::Uuid::parse_str(&operation).is_err()
        || uuid::Uuid::parse_str(&a.browser_id).is_err()
        || !["accepted", "executed", "uploaded", "failed"].contains(&a.state.as_str())
        || (a.state == "uploaded") != (a.attachment_ids.len() == 1)
        || (a.state != "uploaded" && !a.attachment_ids.is_empty())
    {
        return Err(ApiError::bad("Use one supported browser acknowledgment"));
    }
    let current = {
        let db = s.db.lock().unwrap();
        row(&db, &id, &operation)?.ok_or_else(ApiError::missing)?
    };
    if current["browser_id"] != a.browser_id {
        return Err(ApiError::forbidden());
    }
    if a.state == "accepted" {
        let agent: String = s.db.lock().unwrap().query_row(
            "SELECT agent_id FROM companion_calls WHERE id=?1",
            [&id],
            |r| r.get(0),
        )?;
        if surface(&s, &id, current["kind"] == "capture")?.as_deref() != Some(&a.browser_id)
            || !source_authorized(
                &s,
                &id,
                &p.actor,
                &agent,
                &room,
                current["delegation_id"].as_str().unwrap_or(""),
            )?
        {
            return Err(ApiError::forbidden());
        }
    }
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_surfaces p JOIN companion_surface_commands c ON c.call_id=p.call_id AND c.browser_id=p.browser_id AND c.session_hash=p.session_hash WHERE p.call_id=?1 AND p.browser_id=?2 AND p.session_hash=?3 AND c.operation_id=?4)",params![id,a.browser_id,p.session_hash,operation],|r|r.get(0))?;
    if !bound {
        return Err(ApiError::forbidden());
    }
    let previous = row(&tx, &id, &operation)?.ok_or_else(ApiError::missing)?;
    let mut execute = false;
    if a.state == "accepted" {
        execute=tx.execute("UPDATE companion_surface_commands SET state='accepted',accepted_at=?3 WHERE call_id=?1 AND operation_id=?2 AND state='queued' AND expires_at>=?3",params![id,operation,now_ms()])?==1;
    } else {
        let accepted:bool=tx.query_row("SELECT accepted_at IS NOT NULL FROM companion_surface_commands WHERE call_id=?1 AND operation_id=?2",params![id,operation],|r|r.get(0))?;
        if !accepted {
            return Err(ApiError::bad(
                "An operation must be accepted before reporting its effect",
            ));
        }
        if a.state == "uploaded" {
            if previous["kind"] != "capture" {
                return Err(ApiError::bad("Only a capture can upload an image"));
            }
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM companion_uploads u JOIN attachments a ON a.id=?3 WHERE u.call_id=?1 AND u.upload_id=?2 AND u.state='available' AND json_extract(u.data,'$.source')='camera' AND json_array_length(u.data,'$.attachments')=1 AND json_extract(u.data,'$.attachments[0].id')=a.id AND a.conversation_id=?4 AND a.created_by=?5 AND a.created_at>=?6 AND a.bytes>0 AND a.bytes<=?7 AND a.mime_type IN ('image/jpeg','image/png'))",params![id,operation,a.attachment_ids[0],room,p.actor,previous["created_at"].as_i64().unwrap_or(i64::MAX),IMAGE_LIMIT],|r|r.get(0))?;
            if !valid {
                return Err(ApiError::bad(
                    "A saved owned camera image for this operation is required",
                ));
            }
        }
        if previous["state"] == a.state {
            if previous["artifact_ids"] != json!(a.attachment_ids) {
                return Err(ApiError::busy());
            }
        } else if previous["state"] == "uploaded"
            || previous["state"] == "failed"
            || (previous["kind"] != "capture" && previous["state"] == "executed")
        {
            return Err(ApiError::busy());
        } else {
            tx.execute("UPDATE companion_surface_commands SET state=?3,attachment_ids=?4 WHERE call_id=?1 AND operation_id=?2",params![id,operation,a.state,serde_json::to_string(&a.attachment_ids).unwrap()])?;
        }
    }
    let mut receipt = row(&tx, &id, &operation)?.unwrap();
    record_event(
        &tx,
        &id,
        &operation,
        receipt["state"].as_str().unwrap(),
        "surface_receipt",
        &receipt,
    )?;
    tx.commit()?;
    drop(db);
    s.changed();
    receipt["execute"] = json!(execute);
    Ok(Json(receipt))
}
pub async fn status(
    State(s): State<Arc<AppState>>,
    Path((id, operation)): Path<(String, String)>,
    h: HeaderMap,
) -> ApiResult<Json<Value>> {
    let (owner, _, room) = crate::companion_tools::authorize(&s, &id, &h)?;
    let db = s.db.lock().unwrap();
    let feedback: Option<(String, String, i64)> = db.query_row(
        "SELECT delegation_id,text,created_at FROM companion_feedback WHERE call_id=?1 AND operation_id=?2 AND owner=?3",
        params![id,operation,owner], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    if let Some((delegation, text, at)) = feedback {
        return Ok(Json(feedback_receipt(&operation, &delegation, &text, at)));
    }
    let mut receipt = row(&db, &id, &operation)?.ok_or_else(ApiError::missing)?;
    if receipt["state"] == "uploaded" {
        let capability: Option<(Option<bool>,i64)> = db.query_row("SELECT image_input,observed_at FROM companion_surface_capabilities WHERE call_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let image_input = capability.and_then(|v| v.0);
        receipt["image_input"] = json!(image_input);
        receipt["image_capability_observed_at"] = json!(capability.map(|v| v.1));
        if image_input != Some(true) {
            receipt["image_delivery"] = json!(if image_input == Some(false) {
                "unsupported"
            } else {
                "unconfirmed"
            });
            receipt["guidance"] = json!(
                "The photo is saved. Image bytes were not supplied because this ACP connection did not advertise image input. Do not claim inspection or understanding. No model or cloud fallback was used."
            );
            return Ok(Json(receipt));
        }
        let aid = receipt["artifact_ids"][0]
            .as_str()
            .ok_or_else(ApiError::missing)?;
        let image:Option<(String,Vec<u8>)>=db.query_row("SELECT mime_type,data FROM attachments WHERE id=?1 AND conversation_id=?2 AND created_by=?3 AND bytes<=?4",params![aid,room,owner,IMAGE_LIMIT],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((mime, data)) = image {
            receipt["image"] = json!({"mimeType":mime,"data":STANDARD.encode(data)});
            receipt["image_delivery"] = json!("provided");
        }
    }
    Ok(Json(receipt))
}
