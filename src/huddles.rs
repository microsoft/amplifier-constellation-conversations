//! Observable coordination in ordinary rooms. External participants keep their
//! existing controller/session; storing a post never launches a second engine.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const MAX_TEXT: usize = 6000;
pub fn capabilities() -> Value {
    json!({"version":1,"external_participants":true,"read":true,"post":true,
        "post_status":true,"outcomes":true,"quiet_follow":true,"automatic":1,
        "max_text_bytes":MAX_TEXT,"max_evidence_items":8,"max_metadata_bytes":8000,"max_page_items":100})
}
pub fn migrate(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS huddles(
        room_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
        request_id TEXT UNIQUE NOT NULL, purpose TEXT NOT NULL, origin TEXT NOT NULL,
        fingerprint TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'open', revision INTEGER NOT NULL DEFAULT 1);
        CREATE TABLE IF NOT EXISTS huddle_participants(
        room_id TEXT NOT NULL REFERENCES huddles(room_id) ON DELETE CASCADE,
        agent_id TEXT NOT NULL,label TEXT NOT NULL,connection_id TEXT NOT NULL,
        token_hash TEXT NOT NULL, PRIMARY KEY(room_id,agent_id), UNIQUE(token_hash));
        CREATE TABLE IF NOT EXISTS huddle_posts(
        message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
        room_id TEXT NOT NULL REFERENCES huddles(room_id) ON DELETE CASCADE,
        agent_id TEXT NOT NULL, payload TEXT NOT NULL, receipt TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS huddle_preferences(
        room_id TEXT NOT NULL REFERENCES huddles(room_id) ON DELETE CASCADE,
        username TEXT NOT NULL, mode TEXT NOT NULL, PRIMARY KEY(room_id,username));")?;
    Ok(())
}
fn conflict(message: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.contains('\0')
}
fn id(value: &str) -> bool {
    text(value, 200)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}
pub fn is_huddle(s: &AppState, room: &str) -> anyhow::Result<bool> {
    Ok(s.db.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM huddles WHERE room_id=?1)",
        [room],
        |r| r.get(0),
    )?)
}
pub(crate) fn check_open(db: &Connection, room: &str) -> ApiResult<()> {
    let status: Option<String> = db
        .query_row("SELECT state FROM huddles WHERE room_id=?1", [room], |r| {
            r.get(0)
        })
        .optional()?;
    if status.as_deref().is_some_and(|v| v != "open") {
        return Err(conflict(
            "This huddle is stopped. History remains available; no new coordination was accepted.",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OriginActor {
    pub id: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    Conversation {
        conversation_id: String,
        request_id: String,
        message_id: String,
        source_revision: String,
        occurred_at: i64,
        actor: OriginActor,
    },
    External {
        service: String,
        connection_id: String,
        conversation_id: String,
        request_id: String,
        source_id: String,
        source_revision: String,
        occurred_at: i64,
        actor: OriginActor,
    },
}
pub(crate) fn origin_valid(o: &Origin) -> bool {
    match o {
        Origin::Conversation { .. } => true,
        Origin::External {
            service,
            connection_id,
            conversation_id,
            request_id,
            source_id,
            source_revision,
            occurred_at,
            actor,
        } => {
            [
                service,
                connection_id,
                conversation_id,
                request_id,
                source_id,
                source_revision,
                &actor.id,
            ]
            .iter()
            .all(|s| text(s, 200))
                && actor.kind == "human"
                && *occurred_at > 0
                && *occurred_at <= now_ms() + 300_000
        }
    }
}
pub(crate) fn parent_source(
    db: &Connection,
    parent: &str,
    source: &str,
    actor: &str,
) -> ApiResult<Origin> {
    let row: Option<(String,i64)> = db.query_row("SELECT text,created_at FROM messages WHERE id=?1 AND conversation_id=?2 AND actor=?3 AND role='user' AND status='complete'",params![source,parent,actor],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (body, at) =
        row.ok_or_else(|| ApiError::bad("Use your original saved request in this conversation"))?;
    Ok(Origin::Conversation {
        conversation_id: parent.into(),
        request_id: source.into(),
        message_id: source.into(),
        source_revision: auth::digest(&body),
        occurred_at: at,
        actor: OriginActor {
            id: actor.into(),
            kind: "human".into(),
        },
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub request_id: String,
    pub title: String,
    pub purpose: String,
    pub source_message_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalCreate {
    request_id: String,
    title: String,
    purpose: String,
    origin: Origin,
}

fn create_inner(
    s: &AppState,
    p: &auth::Principal,
    request: &str,
    title: &str,
    purpose: &str,
    origin: Origin,
) -> ApiResult<String> {
    if !uuid(request)
        || !text(title, 480)
        || title.chars().count() > 120
        || !text(purpose, 2000)
        || !origin_valid(&origin)
    {
        return Err(ApiError::bad(
            "Use a UUID request_id, a short title, purpose and exact original source",
        ));
    }
    let origin_json = serde_json::to_string(&origin).map_err(anyhow::Error::from)?;
    let fingerprint =
        json!({"owner":p.actor,"title":title,"purpose":purpose,"origin":origin}).to_string();
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    if let Some((room, old)) = tx
        .query_row(
            "SELECT room_id,fingerprint FROM huddles WHERE request_id=?1",
            [request],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if old != fingerprint {
            return Err(conflict(
                "This request_id already identifies a different huddle",
            ));
        }
        return Ok(room);
    }
    if let Origin::Conversation {
        conversation_id,
        message_id,
        actor,
        ..
    } = &origin
    {
        let permitted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversations c JOIN room_members m ON m.conversation_id=c.id WHERE c.id=?1 AND m.username=?2 AND c.read_only=0)",params![conversation_id,p.actor],|r|r.get(0))?;
        if !permitted {
            return Err(ApiError::missing());
        }
        if parent_source(&tx, conversation_id, message_id, &actor.id)? != origin {
            return Err(conflict("The original request changed"));
        }
    }
    let room = uuid::Uuid::new_v4().to_string();
    let at = now_ms();
    tx.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note,owner,shared) VALUES(?1,?2,?3,?3,'idle','',?4,1)",params![room,title,at,p.actor])?;
    tx.execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,?2,'owner',?3)",params![room,p.actor,at])?;
    tx.execute(
        "INSERT INTO huddles(room_id,request_id,purpose,origin,fingerprint) VALUES(?1,?2,?3,?4,?5)",
        params![room, request, purpose, origin_json, fingerprint],
    )?;
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(room)
}
pub async fn create(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(parent): Path<String>,
    Json(body): Json<Create>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&parent, &p)?;
    let origin = parent_source(
        &s.db.lock().unwrap(),
        &parent,
        &body.source_message_id,
        &p.actor,
    )?;
    let room = create_inner(&s, &p, &body.request_id, &body.title, &body.purpose, origin)?;
    Ok((
        StatusCode::CREATED,
        Json(descriptor(&s, &room, Some(&p.actor), None)?),
    ))
}
pub async fn create_external(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<ExternalCreate>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let p = auth::check(&s, &h, "admin", true)?;
    if !matches!(&body.origin, Origin::External { .. }) {
        return Err(ApiError::bad(
            "Use the original conversation route for web requests",
        ));
    }
    let room = create_inner(
        &s,
        &p,
        &body.request_id,
        &body.title,
        &body.purpose,
        body.origin,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(descriptor(&s, &room, Some(&p.actor), None)?),
    ))
}
#[derive(Clone)]
pub(crate) struct Participant {
    agent: String,
    connection: String,
    label: String,
    token_hash: String,
    installation: Option<crate::huddle_auto::Installation>,
}
fn participant(s: &AppState, h: &HeaderMap, room: &str) -> ApiResult<Participant> {
    if let Ok(installation) = crate::huddle_auto::authenticate(s, h) {
        return installed_participant(s, &installation, room);
    }
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?;
    let token_hash = auth::digest(token);
    let row=s.db.lock().unwrap().query_row("SELECT agent_id,connection_id,label FROM huddle_participants WHERE room_id=?1 AND token_hash=?2",params![room,token_hash],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
    let (agent, connection, label) = row.ok_or_else(ApiError::unauthorized)?;
    Ok(Participant {
        agent,
        connection,
        label,
        token_hash,
        installation: None,
    })
}
pub(crate) fn installed_participant(
    s: &AppState,
    installation: &crate::huddle_auto::Installation,
    room: &str,
) -> ApiResult<Participant> {
    let db = s.db.lock().unwrap();
    crate::huddle_auto::current(&db, installation)?;
    let row:Option<(String,String)>=db.query_row("SELECT p.label,p.token_hash FROM huddle_participants p JOIN huddle_auto_members m ON m.room_id=p.room_id JOIN huddle_installations i ON i.id=m.installation_id WHERE m.room_id=?1 AND m.installation_id=?2 AND m.revoked=0 AND p.agent_id=?3 AND p.connection_id=?4",params![room,installation.id,installation.agent_id,installation.connection_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (label, token_hash) = row.ok_or_else(ApiError::forbidden)?;
    Ok(Participant {
        agent: installation.agent_id.clone(),
        connection: installation.connection_id.clone(),
        label,
        token_hash,
        installation: Some(installation.clone()),
    })
}
fn participant_current(db: &Connection, room: &str, p: &Participant) -> ApiResult<()> {
    let permitted:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM huddle_participants WHERE room_id=?1 AND agent_id=?2 AND connection_id=?3 AND token_hash=?4)",params![room,p.agent,p.connection,p.token_hash],|r|r.get(0))?;
    if !permitted {
        return Err(ApiError::unauthorized());
    }
    if let Some(i) = &p.installation {
        crate::huddle_auto::current(db, i)?;
        let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM huddle_auto_members WHERE room_id=?1 AND installation_id=?2 AND revoked=0)",params![room,i.id],|r|r.get(0))?;
        if !active {
            return Err(ApiError::forbidden());
        }
    }
    Ok(())
}
fn reader(
    s: &AppState,
    h: &HeaderMap,
    room: &str,
) -> ApiResult<(Option<String>, Option<Participant>)> {
    if let Ok(p) = auth::check(s, h, "observe", false) {
        s.require_member(room, &p)?;
        return Ok((Some(p.actor), None));
    }
    Ok((None, Some(participant(s, h, room)?)))
}
pub(crate) fn participants(s: &AppState, room: &str) -> anyhow::Result<Vec<Value>> {
    let db = s.db.lock().unwrap();
    let mut q=db.prepare("SELECT agent_id,label,connection_id FROM huddle_participants WHERE room_id=?1 ORDER BY agent_id")?;
    Ok(q.query_map([room],|r|Ok(json!({"id":r.get::<_,String>(0)?,"label":r.get::<_,String>(1)?,"connection_id":r.get::<_,String>(2)?,"delivery":"external"})))?.collect::<rusqlite::Result<_>>()?)
}
pub fn origin(s: &AppState, room: &str) -> anyhow::Result<Option<Origin>> {
    let raw: Option<String> =
        s.db.lock()
            .unwrap()
            .query_row("SELECT origin FROM huddles WHERE room_id=?1", [room], |r| {
                r.get(0)
            })
            .optional()?;
    raw.map(|r| Ok(serde_json::from_str(&r)?)).transpose()
}
pub fn outcomes(s: &AppState, room: &str) -> anyhow::Result<Vec<Value>> {
    let db = s.db.lock().unwrap();
    let mut q=db.prepare("SELECT p.receipt,m.text,p.payload FROM huddle_posts p JOIN messages m ON m.id=p.message_id WHERE p.room_id=?1 AND json_extract(p.payload,'$.kind')='outcome' ORDER BY m.seq DESC LIMIT 50")?;
    let rows = q
        .query_map([room], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(r, text, payload)| {
            let mut v: Value = serde_json::from_str(&r)?;
            let p: Value = serde_json::from_str(&payload)?;
            v["text"] = json!(text);
            v["outcome"] = p["outcome"].clone();
            Ok(v)
        })
        .collect()
}
/// Original-request results for the existing Companion reconciliation loop.
/// Caller performs its own call/endpoint binding; neither source registration
/// nor a participant's claim is permission to choose a new recipient.
pub fn outcomes_for_parent(
    db: &Connection,
    parent: &str,
    owner: &str,
) -> anyhow::Result<Vec<Value>> {
    outcomes_query(db, parent, owner, None)
}
pub fn outcome_for_parent(
    db: &Connection,
    parent: &str,
    owner: &str,
    message_id: &str,
) -> anyhow::Result<Option<Value>> {
    Ok(outcomes_query(db, parent, owner, Some(message_id))?
        .into_iter()
        .next())
}
fn outcomes_query(
    db: &Connection,
    parent: &str,
    owner: &str,
    message_id: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let mut q=db.prepare("SELECT h.room_id,h.origin,p.receipt,p.payload,m.id,m.seq,m.agent_id,m.actor,m.text,m.created_at FROM huddles h JOIN huddle_posts p ON p.room_id=h.room_id JOIN messages m ON m.id=p.message_id WHERE json_extract(h.origin,'$.kind')='conversation' AND json_extract(h.origin,'$.conversation_id')=?1 AND json_extract(h.origin,'$.actor.id')=?2 AND json_extract(p.payload,'$.kind')='outcome' AND (?3 IS NULL OR m.id=?3) AND EXISTS(SELECT 1 FROM room_members WHERE conversation_id=?1 AND username=?2) AND EXISTS(SELECT 1 FROM room_members WHERE conversation_id=h.room_id AND username=?2) ORDER BY m.seq DESC LIMIT 50")?;
    let rows=q.query_map(params![parent,owner,message_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,json!({"id":r.get::<_,String>(4)?,"seq":r.get::<_,i64>(5)?,"agent_id":r.get::<_,String>(6)?,"actor":r.get::<_,String>(7)?,"text":r.get::<_,String>(8)?,"created_at":r.get::<_,i64>(9)?}))))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for (room, origin, receipt, payload, message) in rows {
        let origin: Origin = serde_json::from_str(&origin)?;
        let Origin::Conversation { message_id, .. } = &origin else {
            continue;
        };
        if parent_source(db, parent, message_id, owner).ok().as_ref() != Some(&origin) {
            continue;
        }
        let receipt: Value = serde_json::from_str(&receipt)?;
        let body: Value = serde_json::from_str(&payload)?;
        items.push(json!({"huddle_id":room,"inspect_url":receipt["inspect_url"],"origin":origin,"message":message,"outcome":body["outcome"],"revision":receipt["revision"]}));
    }
    items.reverse();
    Ok(items)
}
fn descriptor(
    s: &AppState,
    room: &str,
    human: Option<&str>,
    principal: Option<&Participant>,
) -> ApiResult<Value> {
    let mut value = summary(s, room, human, principal)?;
    value["outcomes"] = json!(outcomes(s, room)?);
    Ok(value)
}
pub(crate) fn summary(
    s: &AppState,
    room: &str,
    human: Option<&str>,
    principal: Option<&Participant>,
) -> ApiResult<Value> {
    let (purpose, raw, status, revision): (String, String, String, i64) =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT purpose,origin,state,revision FROM huddles WHERE room_id=?1",
                [room],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .ok_or_else(ApiError::missing)?;
    let mode = if let Some(user) = human {
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT mode FROM huddle_preferences WHERE room_id=?1 AND username=?2",
                params![room, user],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| "quiet".into())
    } else {
        "quiet".into()
    };
    let c = s.conversation_info(room)?.ok_or_else(ApiError::missing)?;
    let outcome_summary = {
        let db = s.db.lock().unwrap();
        let count: i64 = db.query_row("SELECT count(*) FROM huddle_posts WHERE room_id=?1 AND json_extract(payload,'$.kind')='outcome'",[room],|r|r.get(0))?;
        let latest: Option<Value> = db.query_row("SELECT m.id,m.created_at,json_extract(p.payload,'$.outcome.kind') FROM huddle_posts p JOIN messages m ON m.id=p.message_id WHERE p.room_id=?1 AND json_extract(p.payload,'$.kind')='outcome' ORDER BY m.seq DESC LIMIT 1",[room],|r|Ok(json!({"id":r.get::<_,String>(0)?,"created_at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?}))).optional()?;
        json!({"count":count,"latest":latest})
    };
    let mut v = json!({"version":1,"id":room,"conversation_id":room,"title":c["title"],"purpose":purpose,"state":status,"revision":revision,"origin":serde_json::from_str::<Value>(&raw).map_err(anyhow::Error::from)?,"inspect_url":format!("{}/?conversation={room}",s.config.public_url.trim_end_matches('/')),"capabilities":capabilities(),"participants":participants(s,room)?,"members":s.members(room)?,"preference":{"mode":mode},"outcome_summary":outcome_summary});
    if let Some(p) = principal {
        v["participant"] = json!({"agent_id":p.agent,"connection_id":p.connection});
    }
    Ok(v)
}
pub async fn read(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
) -> ApiResult<Json<Value>> {
    let (human, p) = reader(&s, &h, &room)?;
    Ok(Json(descriptor(&s, &room, human.as_deref(), p.as_ref())?))
}
pub fn read_for_installation(
    s: &AppState,
    installation: &crate::huddle_auto::Installation,
    room: &str,
) -> ApiResult<Value> {
    let p = installed_participant(s, installation, room)?;
    descriptor(s, room, None, Some(&p))
}
pub async fn list(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(parent): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    s.require_member(&parent, &p)?;
    let ids: Vec<String> = {
        let db = s.db.lock().unwrap();
        let mut q=db.prepare("SELECT h.room_id FROM huddles h JOIN room_members m ON m.conversation_id=h.room_id WHERE m.username=?1 AND json_extract(h.origin,'$.kind')='conversation' AND json_extract(h.origin,'$.conversation_id')=?2 ORDER BY h.rowid DESC LIMIT 100")?;
        q.query_map(params![p.actor, parent], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let items = ids
        .iter()
        .map(|id| summary(&s, id, Some(&p.actor), None))
        .collect::<ApiResult<Vec<_>>>()?;
    let current = if is_huddle(&s, &parent)? {
        Some(summary(&s, &parent, Some(&p.actor), None)?)
    } else {
        None
    };
    Ok(Json(json!({"version":1,"items":items,"current":current})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enroll {
    agent_id: String,
    label: String,
    connection_id: String,
}
pub async fn enroll(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
    Json(body): Json<Enroll>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&room, &p)?;
    if !is_huddle(&s, &room)? {
        return Err(ApiError::missing());
    }
    if !id(&body.agent_id) || !id(&body.connection_id) || !text(&body.label, 120) {
        return Err(ApiError::bad(
            "Use stable agent/connection IDs and a short label",
        ));
    }
    let token = auth::random_token();
    {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM huddle_participants WHERE room_id=?1 AND agent_id<>?2",
            params![room, body.agent_id],
            |r| r.get(0),
        )?;
        if count >= 8 {
            return Err(ApiError::bad("A huddle supports up to eight participants"));
        }
        // Explicit per-room replacement must not leave an older installation
        // credential authorized under the newly enrolled participant's identity.
        tx.execute("UPDATE huddle_auto_members SET revoked=1 WHERE room_id=?1 AND installation_id IN (SELECT id FROM huddle_installations WHERE json_extract(definition,'$.agent_id')=?2)",params![room,body.agent_id])?;
        tx.execute("INSERT INTO huddle_participants(room_id,agent_id,label,connection_id,token_hash) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(room_id,agent_id) DO UPDATE SET label=excluded.label,connection_id=excluded.connection_id,token_hash=excluded.token_hash",params![room,body.agent_id,body.label,body.connection_id,auth::digest(&token)])?;
        tx.execute(
            "UPDATE huddles SET revision=revision+1 WHERE room_id=?1",
            [&room],
        )?;
        tx.commit()?;
    }
    s.changed();
    Ok(Json(
        json!({"version":1,"room_id":room,"agent_id":body.agent_id,"connection_id":body.connection_id,"token":token,"capabilities":capabilities()}),
    ))
}
pub async fn revoke(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((room, agent)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&room, &p)?;
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    tx.execute("UPDATE huddle_auto_members SET revoked=1 WHERE room_id=?1 AND installation_id IN (SELECT id FROM huddle_installations WHERE json_extract(definition,'$.agent_id')=?2)",params![room,agent])?;
    tx.execute(
        "DELETE FROM huddle_participants WHERE room_id=?1 AND agent_id=?2",
        params![room, agent],
    )?;
    tx.execute(
        "UPDATE huddles SET revision=revision+1 WHERE room_id=?1",
        [&room],
    )?;
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(Json(json!({"revoked":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preference {
    mode: String,
}
pub async fn preference(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
    Json(body): Json<Preference>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&room, &p)?;
    if !is_huddle(&s, &room)? {
        return Err(ApiError::missing());
    }
    if !["quiet", "follow"].contains(&body.mode.as_str()) {
        return Err(ApiError::bad("Choose quiet or follow"));
    }
    s.db.lock().unwrap().execute("INSERT INTO huddle_preferences(room_id,username,mode) VALUES(?1,?2,?3) ON CONFLICT(room_id,username) DO UPDATE SET mode=excluded.mode",params![room,p.actor,body.mode])?;
    s.changed();
    Ok(Json(
        json!({"mode":body.mode,"membership_changed":false,"work_authority_changed":false}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateWrite {
    state: String,
    expected_revision: i64,
}
pub async fn set_state(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
    Json(body): Json<StateWrite>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&room, &p)?;
    if !["open", "stopped"].contains(&body.state.as_str()) {
        return Err(ApiError::bad("Choose open or stopped"));
    }
    let changed = s.db.lock().unwrap().execute(
        "UPDATE huddles SET state=?2,revision=revision+1 WHERE room_id=?1 AND revision=?3",
        params![room, body.state, body.expected_revision],
    )?;
    if changed != 1 {
        return Err(conflict(
            "The huddle changed; inspect its current state before acting",
        ));
    }
    s.changed();
    Ok(Json(descriptor(&s, &room, Some(&p.actor), None)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub after: Option<i64>,
    pub limit: Option<usize>,
}
pub async fn messages(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
    Query(page): Query<Page>,
) -> ApiResult<Json<Value>> {
    let (human, p) = reader(&s, &h, &room)?;
    Ok(Json(messages_inner(
        &s,
        &room,
        human.as_deref(),
        p.as_ref(),
        page,
    )?))
}
pub fn messages_for_installation(
    s: &AppState,
    installation: &crate::huddle_auto::Installation,
    room: &str,
    page: Page,
) -> ApiResult<Value> {
    let p = installed_participant(s, installation, room)?;
    messages_inner(s, room, None, Some(&p), page)
}
fn messages_inner(
    s: &AppState,
    room: &str,
    human: Option<&str>,
    p: Option<&Participant>,
    page: Page,
) -> ApiResult<Value> {
    let after = page.after.unwrap_or(0);
    let limit = page.limit.unwrap_or(50).clamp(1, 100);
    if after < 0 {
        return Err(ApiError::bad("Use a nonnegative message cursor"));
    }
    let mut rows: Vec<Value> = {
        let db = s.db.lock().unwrap();
        let mut q=db.prepare("SELECT m.id,m.seq,m.role,m.actor,m.agent_id,m.text,m.status,m.created_at,m.targets,p.payload FROM messages m LEFT JOIN huddle_posts p ON p.message_id=m.id WHERE m.conversation_id=?1 AND m.seq>?2 ORDER BY m.seq LIMIT ?3")?;
        q.query_map(params![room,after,(limit+1) as i64],|r|Ok(json!({"id":r.get::<_,String>(0)?,"seq":r.get::<_,i64>(1)?,"role":r.get::<_,String>(2)?,"actor":r.get::<_,String>(3)?,"agent_id":r.get::<_,Option<String>>(4)?,"text":r.get::<_,String>(5)?,"status":r.get::<_,String>(6)?,"created_at":r.get::<_,i64>(7)?,"target_agent_ids":serde_json::from_str::<Value>(&r.get::<_,String>(8)?).unwrap_or(json!([])),"post":r.get::<_,Option<String>>(9)?.and_then(|p|serde_json::from_str::<Value>(&p).ok())})))?.collect::<rusqlite::Result<_>>()?
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    for row in &mut rows {
        let post = row
            .as_object_mut()
            .unwrap()
            .remove("post")
            .unwrap_or(Value::Null);
        row["source"] = post["source"].clone();
        row["kind"] = if post.is_null() {
            json!("message")
        } else {
            post["kind"].clone()
        };
        row["outcome"] = post["outcome"].clone();
    }
    let next = rows.last().and_then(|r| r["seq"].as_i64()).unwrap_or(after);
    let d = summary(s, room, human, p)?;
    Ok(
        json!({"version":1,"items":rows,"next_cursor":next,"has_more":more,"coverage":{"after":after,"through":next,"complete_through_cursor":true},"state":d["state"],"revision":d["revision"]}),
    )
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    id: String,
    revision: String,
    occurred_at: i64,
    conversation_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    label: String,
    reference: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    kind: String,
    #[serde(default)]
    evidence: Vec<Evidence>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Post {
    request_id: String,
    text: String,
    #[serde(default)]
    target_agent_ids: Vec<String>,
    kind: String,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    outcome: Option<Outcome>,
}
fn validate_post(p: &Post) -> ApiResult<()> {
    let mut targets = p.target_agent_ids.clone();
    targets.sort();
    targets.dedup();
    let valid = uuid(&p.request_id)
        && text(&p.text, MAX_TEXT)
        && targets.len() == p.target_agent_ids.len()
        && targets.len() <= 8
        && targets.iter().all(|t| id(t))
        && (p.kind == "message" && p.outcome.is_none()
            || p.kind == "outcome" && p.outcome.is_some())
        && p.source.as_ref().is_none_or(|s| {
            [&s.id, &s.revision, &s.conversation_id]
                .iter()
                .all(|v| text(v, 200))
                && s.occurred_at > 0
                && s.occurred_at <= now_ms() + 300_000
        })
        && p.outcome.as_ref().is_none_or(|o| {
            ["progress", "result", "blocked", "decision"].contains(&o.kind.as_str())
                && o.evidence.len() <= 8
                && o.evidence
                    .iter()
                    .all(|e| text(&e.label, 200) && text(&e.reference, 1000))
        });
    if !valid
        || serde_json::to_vec(&json!({"source":p.source,"outcome":p.outcome}))
            .map_err(anyhow::Error::from)?
            .len()
            > 8000
    {
        return Err(ApiError::bad(
            "Invalid bounded huddle post; use the advertised message/outcome schema",
        ));
    }
    Ok(())
}
fn parent_return(
    db: &Connection,
    origin: &Origin,
    room: &str,
    actor: &Participant,
    body: &Post,
    inspect: &str,
) -> ApiResult<Value> {
    let Origin::Conversation {
        conversation_id,
        message_id,
        actor: owner,
        ..
    } = origin
    else {
        return Ok(json!({"state":"available","delivery":"domain_owned"}));
    };
    let member: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_members WHERE conversation_id=?1 AND username=?2)",
        params![conversation_id, owner.id],
        |r| r.get(0),
    )?;
    if !member
        || parent_source(db, conversation_id, message_id, &owner.id)
            .ok()
            .as_ref()
            != Some(origin)
    {
        return Ok(json!({"state":"blocked","reason":"original_source_or_audience_changed"}));
    }
    if crate::companion_huddles::is_companion(db, conversation_id)? {
        return Ok(json!({"state":"available","delivery":"companion_owned"}));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let at = now_ms();
    let message = format!("{}\n\n[Inspect coordination]({inspect})", body.text);
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,?5,?6)",params![id,conversation_id,message,at,actor.label,actor.agent])?;
    db.execute(
        "UPDATE conversations SET updated_at=?2 WHERE id=?1",
        params![conversation_id, at],
    )?;
    Ok(
        json!({"state":"stored","message_id":id,"conversation_id":conversation_id,"huddle_id":room,"read_by_human":null}),
    )
}
pub async fn post(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(room): Path<String>,
    Json(body): Json<Post>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let actor = participant(&s, &h, &room)?;
    let (status, value) = post_inner(&s, &room, actor, body, |_| Ok(()))?;
    Ok((status, Json(value)))
}
pub fn post_for_installation(
    s: &AppState,
    installation: &crate::huddle_auto::Installation,
    room: &str,
    body: Post,
) -> ApiResult<Value> {
    post_for_installation_guarded(s, installation, room, body, |_| Ok(()))
}
pub fn post_for_installation_guarded(
    s: &AppState,
    installation: &crate::huddle_auto::Installation,
    room: &str,
    body: Post,
    guard: impl FnOnce(&Connection) -> ApiResult<()>,
) -> ApiResult<Value> {
    let p = installed_participant(s, installation, room)?;
    Ok(post_inner(s, room, p, body, guard)?.1)
}
fn post_inner(
    s: &AppState,
    room: &str,
    actor: Participant,
    body: Post,
    guard: impl FnOnce(&Connection) -> ApiResult<()>,
) -> ApiResult<(StatusCode, Value)> {
    validate_post(&body)?;
    let payload = serde_json::to_string(&body).map_err(anyhow::Error::from)?;
    let receipt = {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        guard(&tx)?;
        participant_current(&tx, room, &actor)?;
        if let Some((old_room, old_agent, old_payload, receipt)) = tx
            .query_row(
                "SELECT room_id,agent_id,payload,receipt FROM huddle_posts WHERE message_id=?1",
                [&body.request_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        {
            let saved: Value = serde_json::from_str(&receipt).map_err(anyhow::Error::from)?;
            if old_room != room
                || old_agent != actor.agent
                || old_payload != payload
                || saved["connection_id"] != actor.connection
            {
                return Err(conflict(
                    "This request_id already identifies a different post",
                ));
            }
            return Ok((StatusCode::OK, saved));
        }
        check_open(&tx, room)?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",
            [&body.request_id],
            |r| r.get(0),
        )?;
        if existing {
            return Err(conflict(
                "This request_id already identifies another message",
            ));
        }
        for target in &body.target_agent_ids {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM huddle_participants WHERE room_id=?1 AND agent_id=?2)",
                params![room, target],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(ApiError::bad("Address only current huddle participants"));
            }
        }
        let raw: String = tx.query_row(
            "SELECT origin FROM huddles WHERE room_id=?1",
            [&room],
            |r| r.get(0),
        )?;
        let origin: Origin = serde_json::from_str(&raw).map_err(anyhow::Error::from)?;
        let at = now_ms();
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id,targets) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,?5,?6,?7)",params![body.request_id,room,body.text,at,actor.label,actor.agent,serde_json::to_string(&body.target_agent_ids).map_err(anyhow::Error::from)?])?;
        let seq = tx.last_insert_rowid();
        let returned = if body.kind == "outcome" {
            parent_return(
                &tx,
                &origin,
                room,
                &actor,
                &body,
                &format!(
                    "{}/?conversation={room}",
                    s.config.public_url.trim_end_matches('/')
                ),
            )?
        } else {
            json!({"state":"not_requested"})
        };
        let mut receipt = json!({"version":1,"status":"saved","message_id":body.request_id,"request_id":body.request_id,"room_id":room,"sequence":seq,"kind":body.kind,"created_at":at,"agent_id":actor.agent,"connection_id":actor.connection,"origin":origin,"return":returned,"revision":auth::digest(&payload),"text_sha256":auth::digest(&body.text),"source":body.source,"outcome":body.outcome,"target_agent_ids":body.target_agent_ids,"inspect_url":format!("{}/?conversation={room}",s.config.public_url.trim_end_matches('/'))});
        tx.execute("INSERT INTO huddle_posts(message_id,room_id,agent_id,payload,receipt) VALUES(?1,?2,?3,?4,?5)",params![body.request_id,room,actor.agent,payload,receipt.to_string()])?;
        if body.kind == "outcome"
            && let Origin::Conversation {
                conversation_id,
                actor: owner,
                ..
            } = &origin
            && crate::companion_huddles::is_companion(&tx, conversation_id)?
        {
            receipt["return"] = if let Some(row) =
                outcome_for_parent(&tx, conversation_id, &owner.id, &body.request_id)?
            {
                crate::companion_huddles::record_return(s, &tx, &row)?
            } else {
                json!({"state":"blocked","delivery":"companion_owned","reason":"original_request_or_private_audience_changed"})
            };
            tx.execute(
                "UPDATE huddle_posts SET receipt=?2 WHERE message_id=?1",
                params![body.request_id, receipt.to_string()],
            )?;
        }
        tx.execute(
            "UPDATE conversations SET updated_at=?2 WHERE id=?1",
            params![room, at],
        )?;
        tx.execute(
            "UPDATE huddles SET revision=revision+1 WHERE room_id=?1",
            [&room],
        )?;
        tx.commit()?;
        receipt
    };
    s.changed();
    Ok((StatusCode::CREATED, receipt))
}
pub async fn post_status(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((room, request)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let actor = participant(&s, &h, &room)?;
    Ok(Json(post_status_inner(&s, &room, &request, &actor)?))
}
pub fn post_status_for_installation(
    s: &AppState,
    installation: &crate::huddle_auto::Installation,
    room: &str,
    request: &str,
) -> ApiResult<Value> {
    let p = installed_participant(s, installation, room)?;
    post_status_inner(s, room, request, &p)
}
fn post_status_inner(
    s: &AppState,
    room: &str,
    request: &str,
    actor: &Participant,
) -> ApiResult<Value> {
    let raw: Option<String> = s
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT receipt FROM huddle_posts WHERE room_id=?1 AND message_id=?2 AND agent_id=?3 AND json_extract(receipt,'$.connection_id')=?4",
            params![room, request, actor.agent, actor.connection],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(&raw.ok_or_else(ApiError::missing)?).map_err(anyhow::Error::from)?)
}
