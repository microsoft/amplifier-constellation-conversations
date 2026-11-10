//! Installation-owned huddle discovery and atomic create/reuse. This registry
//! authorizes conversation membership, never execution or a second ACP session.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    huddles::{self, Origin},
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
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub connection_id: String,
    pub service: String,
    pub conversation_id: String,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    pub enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub agent_id: String,
    pub label: String,
    pub connection_id: String,
    pub service: String,
    pub conversation_id: String,
    pub namespace: String,
    pub local_agent_id: Option<String>,
    pub identity: Option<Identity>,
    pub enabled: bool,
    pub token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ensure {
    pub request_id: String,
    pub title: String,
    pub purpose: String,
    pub origin: Origin,
    #[serde(default)]
    pub peer_installation_ids: Vec<String>,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Page {
    #[serde(default)]
    pub after: i64,
    pub limit: Option<usize>,
}
pub fn migrate(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS huddle_installations(
        id TEXT PRIMARY KEY, definition TEXT NOT NULL, token_hash TEXT UNIQUE NOT NULL);
        CREATE TABLE IF NOT EXISTS huddle_auto_rooms(
        room_id TEXT PRIMARY KEY REFERENCES huddles(room_id) ON DELETE CASCADE,
        namespace TEXT NOT NULL, origin_key TEXT NOT NULL, UNIQUE(namespace,origin_key));
        CREATE TABLE IF NOT EXISTS huddle_auto_members(
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        room_id TEXT NOT NULL REFERENCES huddles(room_id) ON DELETE CASCADE,
        installation_id TEXT NOT NULL REFERENCES huddle_installations(id),
        revoked INTEGER NOT NULL DEFAULT 0, UNIQUE(room_id,installation_id));
        CREATE TABLE IF NOT EXISTS huddle_ensures(
        request_id TEXT PRIMARY KEY, installation_id TEXT NOT NULL,
        payload TEXT NOT NULL, room_id TEXT NOT NULL REFERENCES huddles(room_id) ON DELETE CASCADE,
        reused INTEGER NOT NULL);",
    )?;
    Ok(())
}
fn conflict(s: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, s.into())
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn bounded(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.contains('\0')
}
fn load(db: &Connection, id: &str) -> ApiResult<Installation> {
    let raw: String = db
        .query_row(
            "SELECT definition FROM huddle_installations WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(ApiError::missing)?;
    Ok(serde_json::from_str(&raw).map_err(anyhow::Error::from)?)
}
pub(crate) fn current(db: &Connection, installation: &Installation) -> ApiResult<()> {
    if !installation.enabled || load(db, &installation.id)? != *installation {
        return Err(ApiError::forbidden());
    }
    Ok(())
}
pub async fn install(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Install>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let entry = Installation {
        id,
        agent_id: body.agent_id,
        label: body.label,
        connection_id: body.connection_id,
        service: body.service,
        conversation_id: body.conversation_id,
        namespace: body.namespace,
        local_agent_id: body.local_agent_id,
        identity: body.identity,
        enabled: body.enabled,
    };
    if ![
        &entry.id,
        &entry.agent_id,
        &entry.connection_id,
        &entry.service,
        &entry.namespace,
    ]
    .iter()
    .all(|v| valid_id(v))
        || !bounded(&entry.conversation_id, 200)
        || !bounded(&entry.label, 120)
        || entry.local_agent_id.as_ref().is_some_and(|v| !valid_id(v))
        || entry.identity.as_ref().is_some_and(|v| {
            v.player_uuid
                .as_ref()
                .is_some_and(|u| uuid::Uuid::parse_str(u).is_err())
                || v.player_name.as_ref().is_some_and(|n| !bounded(n, 120))
        })
        || body.token.len() < 32
        || body.token.len() > 256
        || !body.token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ApiError::bad(
            "Use stable installation IDs, a known namespace and an installation-owned random secret",
        ));
    }
    if let Some(local) = &entry.local_agent_id
        && s.agent(local)?.is_none()
    {
        return Err(ApiError::bad("Unknown local agent"));
    }
    {
        let db = s.db.lock().unwrap();
        let prior: Option<String> = db
            .query_row(
                "SELECT definition FROM huddle_installations WHERE id=?1",
                [&entry.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            let mut old: Installation =
                serde_json::from_str(&prior).map_err(anyhow::Error::from)?;
            old.label = entry.label.clone();
            old.enabled = entry.enabled;
            if old != entry {
                return Err(conflict(
                    "Installation identity is immutable; enroll a distinct installation for another connection",
                ));
            }
        }
        let duplicate: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM huddle_installations WHERE id<>?1 AND token_hash=?2)",
            params![entry.id, auth::digest(&body.token)],
            |r| r.get(0),
        )?;
        if duplicate {
            return Err(conflict("Use a distinct credential for each installation"));
        }
        db.execute("INSERT INTO huddle_installations(id,definition,token_hash) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition,token_hash=excluded.token_hash",params![entry.id,serde_json::to_string(&entry).map_err(anyhow::Error::from)?,auth::digest(&body.token)])?;
    }
    s.changed();
    Ok(Json(
        json!({"version":1,"installation":entry,"capabilities":huddles::capabilities()}),
    ))
}
pub async fn installations(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", false)?;
    let db = s.db.lock().unwrap();
    let mut q = db.prepare("SELECT definition FROM huddle_installations ORDER BY id")?;
    let rows = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let items = rows
        .into_iter()
        .map(|v| serde_json::from_str::<Value>(&v))
        .collect::<Result<Vec<_>, _>>()
        .map_err(anyhow::Error::from)?;
    Ok(Json(json!({"version":1,"items":items})))
}
pub fn authenticate(s: &AppState, h: &HeaderMap) -> ApiResult<Installation> {
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?;
    let db = s.db.lock().unwrap();
    let raw: Option<String> = db
        .query_row(
            "SELECT definition FROM huddle_installations WHERE token_hash=?1",
            [auth::digest(token)],
            |r| r.get(0),
        )
        .optional()?;
    let entry: Installation = serde_json::from_str(&raw.ok_or_else(ApiError::unauthorized)?)
        .map_err(anyhow::Error::from)?;
    if !entry.enabled {
        return Err(ApiError::forbidden());
    }
    Ok(entry)
}
/// Call-owned Companion tooling supplies its verified owner and current agent.
/// Neither a model-selected owner nor a bearer for another call can use this.
pub fn local_installations(s: &AppState, owner: &str, agent: &str) -> ApiResult<Vec<Installation>> {
    if owner != s.config.owner || s.agent(agent)?.is_none() {
        return Err(ApiError::forbidden());
    }
    let db = s.db.lock().unwrap();
    let mut q=db.prepare("SELECT definition FROM huddle_installations WHERE json_extract(definition,'$.local_agent_id')=?1 AND json_extract(definition,'$.enabled')=1 ORDER BY id")?;
    let rows = q
        .query_map([agent], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|r| serde_json::from_str(&r).map_err(|e| anyhow::Error::from(e).into()))
        .collect()
}
pub fn roster_for(s: &AppState, caller: &Installation) -> ApiResult<Value> {
    let db = s.db.lock().unwrap();
    current(&db, caller)?;
    let mut q=db.prepare("SELECT definition FROM huddle_installations WHERE json_extract(definition,'$.namespace')=?1 AND json_extract(definition,'$.enabled')=1 ORDER BY id")?;
    let rows = q
        .query_map([&caller.namespace], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for raw in rows {
        let mut value: Value = serde_json::from_str(&raw).map_err(anyhow::Error::from)?;
        value["availability"] = json!("unknown");
        items.push(value);
    }
    Ok(
        json!({"version":1,"installation":caller,"items":items,"capabilities":huddles::capabilities()}),
    )
}
pub async fn roster(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    Ok(Json(roster_for(&s, &authenticate(&s, &h)?)?))
}
fn source_key(origin: &Origin) -> String {
    let value = match origin {
        Origin::External {
            service,
            connection_id,
            conversation_id,
            source_id,
            ..
        } => json!([
            "external",
            service,
            connection_id,
            conversation_id,
            source_id
        ]),
        Origin::Conversation {
            conversation_id,
            message_id,
            ..
        } => json!(["conversation", conversation_id, message_id]),
    };
    auth::digest(&value.to_string())
}
fn validate_origin(
    db: &Connection,
    s: &AppState,
    caller: &Installation,
    origin: &Origin,
    external_only: bool,
) -> ApiResult<()> {
    if !huddles::origin_valid(origin) {
        return Err(ApiError::bad("Use the exact original human source"));
    }
    match origin {
        Origin::External {
            service,
            connection_id,
            conversation_id,
            ..
        } if service == &caller.service
            && connection_id == &caller.connection_id
            && conversation_id == &caller.conversation_id =>
        {
            Ok(())
        }
        Origin::Conversation {
            conversation_id,
            message_id,
            actor,
            ..
        } if !external_only && actor.id == s.config.owner => {
            let member:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM room_members WHERE conversation_id=?1 AND username=?2)",params![conversation_id,actor.id],|r|r.get(0))?;
            if !member
                || huddles::parent_source(db, conversation_id, message_id, &actor.id)? != *origin
            {
                return Err(ApiError::forbidden());
            }
            Ok(())
        }
        _ => Err(ApiError::forbidden()),
    }
}
pub fn ensure_for(s: &AppState, caller: &Installation, body: Ensure) -> ApiResult<Value> {
    ensure_impl(s, caller, body, false, |_| Ok(()))
}
pub fn ensure_for_guarded(
    s: &AppState,
    caller: &Installation,
    body: Ensure,
    guard: impl FnOnce(&Connection) -> ApiResult<()>,
) -> ApiResult<Value> {
    ensure_impl(s, caller, body, false, guard)
}
fn ensure_impl(
    s: &AppState,
    caller: &Installation,
    mut body: Ensure,
    external_only: bool,
    guard: impl FnOnce(&Connection) -> ApiResult<()>,
) -> ApiResult<Value> {
    if uuid::Uuid::parse_str(&body.request_id).is_err()
        || !bounded(&body.title, 480)
        || body.title.chars().count() > 120
        || !bounded(&body.purpose, 2000)
        || body.peer_installation_ids.len() > 7
        || body.peer_installation_ids.iter().any(|p| !valid_id(p))
    {
        return Err(ApiError::bad(
            "Use a UUID, short title/purpose, and up to seven known peers",
        ));
    }
    body.peer_installation_ids.sort();
    if body.peer_installation_ids.windows(2).any(|w| w[0] == w[1])
        || body.peer_installation_ids.contains(&caller.id)
    {
        return Err(ApiError::bad(
            "The creator is implicit; name distinct peers",
        ));
    }
    let payload = serde_json::to_string(&body).map_err(anyhow::Error::from)?;
    let (room, reused) = {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        guard(&tx)?;
        current(&tx, caller)?;
        validate_origin(&tx, s, caller, &body.origin, external_only)?;
        if let Some((who,old,room,reused))=tx.query_row("SELECT installation_id,payload,room_id,reused FROM huddle_ensures WHERE request_id=?1",[&body.request_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,bool>(3)?))).optional()? {
            if who!=caller.id || old!=payload {return Err(conflict("This request_id already identifies another ensure operation"));}
            (room,reused)
        }else{
            let key=source_key(&body.origin);
            let old:Option<(String,String)>=tx.query_row("SELECT h.room_id,h.origin FROM huddles h JOIN huddle_auto_rooms a ON a.room_id=h.room_id WHERE a.namespace=?1 AND a.origin_key=?2",params![caller.namespace,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let reused=old.is_some();
            let room=if let Some((room,origin))=old {
                if serde_json::from_str::<Origin>(&origin).map_err(anyhow::Error::from)?!=body.origin {return Err(conflict("Original source changed; do not replace its huddle"));}
                room
            }else{
                let room=uuid::Uuid::new_v4().to_string();let at=now_ms();
                tx.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note,owner,shared) VALUES(?1,?2,?3,?3,'idle','',?4,1)",params![room,body.title,at,s.config.owner])?;
                tx.execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,?2,'owner',?3)",params![room,s.config.owner,at])?;
                tx.execute("INSERT INTO huddles(room_id,request_id,purpose,origin,fingerprint) VALUES(?1,?2,?3,?4,?5)",params![room,body.request_id,body.purpose,serde_json::to_string(&body.origin).map_err(anyhow::Error::from)?,payload])?;
                tx.execute("INSERT INTO huddle_auto_rooms(room_id,namespace,origin_key) VALUES(?1,?2,?3)",params![room,caller.namespace,key])?; room
            };
            // A stopped room is a saved result, not an instruction to reopen it.
            // Repeating ensure cannot undo a human revocation or change origin.
            let open:bool=tx.query_row("SELECT state='open' FROM huddles WHERE room_id=?1",[&room],|r|r.get(0))?;
            let creator_revoked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM huddle_auto_members WHERE room_id=?1 AND installation_id=?2 AND revoked=1)",params![room,caller.id],|r|r.get(0))?;
            if open && !creator_revoked {
                let mut all=vec![caller.clone()];
                for peer in &body.peer_installation_ids {let p=load(&tx,peer)?;if !p.enabled || p.namespace!=caller.namespace{return Err(ApiError::forbidden());}all.push(p);}
                let mut agents=BTreeSet::new();for p in &all {if !agents.insert(&p.agent_id){return Err(ApiError::bad("Choose one connection per agent for this huddle"));}}
                for p in all {
                    let known:Option<bool>=tx.query_row("SELECT revoked FROM huddle_auto_members WHERE room_id=?1 AND installation_id=?2",params![room,p.id],|r|r.get(0)).optional()?;
                    if known.is_some() {continue;}
                    let count:i64=tx.query_row("SELECT COUNT(*) FROM huddle_participants WHERE room_id=?1",[&room],|r|r.get(0))?;
                    if count>=8{return Err(ApiError::bad("A huddle supports up to eight participants"));}
                    let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM huddle_participants WHERE room_id=?1 AND agent_id=?2)",params![room,p.agent_id],|r|r.get(0))?;
                    if existing{return Err(conflict("This agent already participates through another connection"));}
                    tx.execute("INSERT INTO huddle_participants(room_id,agent_id,label,connection_id,token_hash) VALUES(?1,?2,?3,?4,?5)",params![room,p.agent_id,p.label,p.connection_id,auth::digest(&auth::random_token())])?;
                    tx.execute("INSERT INTO huddle_auto_members(room_id,installation_id) VALUES(?1,?2)",params![room,p.id])?;
                    tx.execute("UPDATE huddles SET revision=revision+1 WHERE room_id=?1",[&room])?;
                }
            }
            tx.execute("INSERT INTO huddle_ensures(request_id,installation_id,payload,room_id,reused) VALUES(?1,?2,?3,?4,?5)",params![body.request_id,caller.id,payload,room,reused])?;
            tx.commit()?;(room,reused)
        }
    };
    s.changed();
    ensure_result(s, caller, &body.request_id, &room, reused)
}
fn ensure_result(
    s: &AppState,
    caller: &Installation,
    request: &str,
    room: &str,
    reused: bool,
) -> ApiResult<Value> {
    let mut v = member_summary(s, caller, room)?;
    v["installation"] = json!(caller);
    v["request_id"] = json!(request);
    v["status"] = json!("saved");
    v["reused"] = json!(reused);
    Ok(v)
}
pub fn ensure_status_for(s: &AppState, caller: &Installation, request: &str) -> ApiResult<Value> {
    let (room, reused) = {
        let db = s.db.lock().unwrap();
        current(&db, caller)?;
        db.query_row(
            "SELECT room_id,reused FROM huddle_ensures WHERE request_id=?1 AND installation_id=?2",
            params![request, caller.id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
        )
        .optional()?
        .ok_or_else(ApiError::missing)?
    };
    ensure_result(s, caller, request, &room, reused)
}
pub async fn ensure(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<Ensure>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ensure_impl(
        &s,
        &authenticate(&s, &h)?,
        body,
        true,
        |_| Ok(()),
    )?))
}
pub async fn ensure_status(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(request): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(ensure_status_for(
        &s,
        &authenticate(&s, &h)?,
        &request,
    )?))
}
fn member_summary(s: &AppState, caller: &Installation, room: &str) -> ApiResult<Value> {
    let (seq, revoked) = {
        let db = s.db.lock().unwrap();
        current(&db, caller)?;
        db.query_row(
            "SELECT seq,revoked FROM huddle_auto_members WHERE room_id=?1 AND installation_id=?2",
            params![room, caller.id],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)),
        )
        .optional()?
        .ok_or_else(ApiError::forbidden)?
    };
    if revoked {
        return Ok(
            json!({"version":1,"id":room,"membership":"revoked","membership_sequence":seq,"participant":{"agent_id":caller.agent_id,"connection_id":caller.connection_id},"capabilities":huddles::capabilities()}),
        );
    }
    let p = huddles::installed_participant(s, caller, room)?;
    let mut value = huddles::summary(s, room, None, Some(&p))?;
    value["membership"] = json!("active");
    value["membership_sequence"] = json!(seq);
    let last: Option<i64> = s.db.lock().unwrap().query_row(
        "SELECT MAX(seq) FROM messages WHERE conversation_id=?1",
        [room],
        |r| r.get(0),
    )?;
    value["last_message_sequence"] = json!(last.unwrap_or(0));
    Ok(value)
}
pub fn discovery_for(s: &AppState, caller: &Installation, page: Page) -> ApiResult<Value> {
    if page.after < 0 {
        return Err(ApiError::bad("Use a nonnegative membership cursor"));
    }
    let limit = page.limit.unwrap_or(50).clamp(1, 100);
    let mut rows = {
        let db = s.db.lock().unwrap();
        current(&db, caller)?;
        let mut q=db.prepare("SELECT room_id,seq FROM huddle_auto_members WHERE installation_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
        q.query_map(params![caller.id, page.after, (limit + 1) as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    let cursor = rows.last().map(|r| r.1).unwrap_or(page.after);
    let items = rows
        .iter()
        .map(|r| member_summary(s, caller, &r.0))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(
        json!({"version":1,"installation":caller,"items":items,"cursor":cursor,"has_more":more,"coverage":"current_memberships","capabilities":huddles::capabilities()}),
    )
}
pub async fn discovery(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(page): Query<Page>,
) -> ApiResult<Json<Value>> {
    Ok(Json(discovery_for(&s, &authenticate(&s, &h)?, page)?))
}
