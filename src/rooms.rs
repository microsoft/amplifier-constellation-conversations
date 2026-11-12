//! Room membership and independently attached ACP endpoints. No agent private state is read.
use crate::{
    auth,
    config::Agent,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn migrate(db: &rusqlite::Connection, config: &crate::config::Config) -> anyhow::Result<()> {
    for (table, name, definition) in [
        ("conversations", "owner", "TEXT NOT NULL DEFAULT ''"),
        ("conversations", "shared", "INTEGER NOT NULL DEFAULT 0"),
        ("messages", "actor", "TEXT NOT NULL DEFAULT ''"),
        ("messages", "agent_id", "TEXT"),
        ("messages", "targets", "TEXT NOT NULL DEFAULT '[]'"),
        ("views", "owner", "TEXT NOT NULL DEFAULT ''"),
        ("grants", "owner_scope", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let exists = db
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == name);
        if !exists {
            db.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS room_members(conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE, username TEXT NOT NULL, role TEXT NOT NULL, joined_at INTEGER NOT NULL, PRIMARY KEY(conversation_id,username));
        CREATE TABLE IF NOT EXISTS agent_endpoints(id TEXT PRIMARY KEY,config TEXT NOT NULL,updated_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS room_agents(conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE, agent_id TEXT NOT NULL REFERENCES agent_endpoints(id), session_id TEXT, endpoint TEXT, last_seq INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(conversation_id,agent_id));
        CREATE TABLE IF NOT EXISTS attachments(id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,name TEXT NOT NULL,mime_type TEXT NOT NULL,data BLOB NOT NULL,bytes INTEGER NOT NULL,created_by TEXT NOT NULL,created_at INTEGER NOT NULL,message_id TEXT REFERENCES messages(id) ON DELETE CASCADE);")?;
    db.execute("INSERT INTO agent_endpoints(id,config,updated_at) VALUES('default',?1,?2) ON CONFLICT(id) DO UPDATE SET config=excluded.config,updated_at=excluded.updated_at",params![serde_json::to_string(&config.agent)?,now_ms()])?;
    let migrated: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name='shared-rooms')",
        [],
        |r| r.get(0),
    )?;
    if !migrated {
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "UPDATE conversations SET owner=?1 WHERE owner=''",
            [&config.owner],
        )?;
        tx.execute("UPDATE views SET owner=?1 WHERE owner=''", [&config.owner])?;
        tx.execute("UPDATE messages SET actor=CASE WHEN role='user' THEN ?1 ELSE ?2 END,agent_id=CASE WHEN role='assistant' THEN 'default' ELSE NULL END WHERE actor=''",params![config.owner,config.agent.label])?;
        tx.execute("INSERT OR IGNORE INTO room_members(conversation_id,username,role,joined_at) SELECT id,?1,'owner',created_at FROM conversations",[&config.owner])?;
        tx.execute("INSERT OR IGNORE INTO room_agents(conversation_id,agent_id,session_id,endpoint,last_seq) SELECT id,'default',session_id,endpoint,COALESCE((SELECT MAX(seq) FROM messages WHERE conversation_id=conversations.id),0) FROM conversations WHERE read_only=0",[])?;
        // Existing grants were owner-only. Revoking them makes upgraded behavior explicit.
        tx.execute("DELETE FROM grants", [])?;
        tx.execute("INSERT INTO migrations(name) VALUES('shared-rooms')", [])?;
        tx.commit()?;
    }
    let imported_sources_migrated: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name='default-agent-import-sources')",
        [],
        |r| r.get(0),
    )?;
    if !imported_sources_migrated {
        let tx = db.unchecked_transaction()?;
        // Existing import bridges carry the original default agent's public
        // transcript. Add only source membership; retain history and sessions.
        tx.execute("INSERT OR IGNORE INTO room_agents(conversation_id,agent_id) SELECT c.id,'default' FROM conversations c JOIN imports i ON i.conversation_id=c.id WHERE c.read_only=1",[])?;
        tx.execute(
            "INSERT INTO migrations(name) VALUES('default-agent-import-sources')",
            [],
        )?;
        tx.commit()?;
    }
    Ok(())
}
impl AppState {
    pub fn member(&self, room: &str, user: &str) -> anyhow::Result<bool> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM room_members WHERE conversation_id=?1 AND username=?2)",
            params![room, user],
            |r| r.get(0),
        )?)
    }
    pub fn require_member(&self, room: &str, p: &auth::Principal) -> ApiResult<()> {
        if !self.member(room, &p.actor)? {
            return Err(ApiError::missing());
        }
        Ok(())
    }
    pub fn require_owner(&self, room: &str, p: &auth::Principal) -> ApiResult<()> {
        self.require_member(room, p)?;
        if self
            .conversation_info(room)?
            .is_none_or(|c| c["owner"] != p.actor)
        {
            return Err(ApiError::forbidden());
        }
        if self.running.lock().unwrap().contains_key(room) {
            return Err(ApiError::busy());
        }
        Ok(())
    }
    pub fn members(&self, room: &str) -> anyhow::Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut q=db.prepare("SELECT username,role,joined_at FROM room_members WHERE conversation_id=?1 ORDER BY joined_at,username")?;
        Ok(q.query_map([room], |r| Ok(json!({"username":r.get::<_,String>(0)?,"role":r.get::<_,String>(1)?,"joined_at":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<_>>()?)
    }
    pub fn agent(&self, id: &str) -> anyhow::Result<Option<Agent>> {
        let value: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT config FROM agent_endpoints WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|v| Ok(serde_json::from_str(&v)?)).transpose()
    }
    pub fn room_agents(&self, room: &str) -> anyhow::Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut q=db.prepare("SELECT e.id,e.config FROM room_agents r JOIN agent_endpoints e ON e.id=r.agent_id WHERE r.conversation_id=?1 ORDER BY e.id")?;
        let mut items: Vec<Value> = q
            .query_map([room], |r| {
                let c: Value = serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default();
                Ok(json!({"id":r.get::<_,String>(0)?,"label":c["label"]}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(q);
        drop(db);
        items.extend(crate::huddles::participants(self, room)?);
        Ok(items)
    }
    pub fn owner_context(&self, room: &str) -> anyhow::Result<bool> {
        let c = self.conversation_info(room)?;
        Ok(c.is_some_and(|c| c["owner"] == self.config.owner && c["shared"] == false))
    }
    pub fn reset_room(&self, room: &str) -> anyhow::Result<()> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction()?;
        tx.execute("DELETE FROM grants WHERE conversation_id=?1", [room])?;
        tx.execute("UPDATE room_agents SET session_id=NULL,endpoint=NULL,last_seq=0 WHERE conversation_id=?1",[room])?;
        tx.execute("UPDATE conversations SET shared=1,session_id=NULL,endpoint=NULL,updated_at=?2 WHERE id=?1",params![room,now_ms()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn create_room(
        &self,
        title: &str,
        owner: &str,
        agents: &[String],
    ) -> anyhow::Result<Value> {
        let id = {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            let id = insert_room(&tx, title, owner, agents)?;
            tx.commit()?;
            id
        };
        self.audit("conversation.created", Some(&id), json!({"actor":owner}))?;
        self.changed();
        Ok(self.conversation(&id)?.unwrap())
    }
}
/// Insert room identity in the caller's transaction, allowing call admission to
/// reserve its room and active-call row atomically with the installation gate.
pub(crate) fn insert_room(
    db: &rusqlite::Connection,
    title: &str,
    owner: &str,
    agents: &[String],
) -> anyhow::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let at = now_ms();
    db.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note,owner,shared) VALUES(?1,?2,?3,?3,'idle','',?4,?5)",params![id,title,at,owner,agents.len()>1])?;
    db.execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,?2,'owner',?3)",params![id,owner,at])?;
    for agent in agents {
        db.execute(
            "INSERT INTO room_agents(conversation_id,agent_id) VALUES(?1,?2)",
            params![id, agent],
        )?;
    }
    Ok(id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberWrite {
    username: String,
    #[serde(default)]
    share_history: bool,
}
pub async fn members(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    s.require_member(&id, &p)?;
    Ok(Json(
        json!({"items":s.members(&id)?,"agents":s.room_agents(&id)?}),
    ))
}
pub async fn add_member(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(m): Json<MemberWrite>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&id, &p)?;
    native(&s, &id)?;
    if !m.share_history {
        return Err(ApiError::bad(
            "Confirm share_history:true to share this room's existing messages and files with this person",
        ));
    }
    if !s.config.allows_user(&m.username) {
        return Err(ApiError::bad(
            "This person needs an existing host account and must first be allowed by the service owner",
        ));
    }
    if s.members(&id)?.len() >= 50 {
        return Err(ApiError::bad("A room supports at most 50 people"));
    }
    let running = s.running.lock().unwrap();
    if running.contains_key(&id) {
        return Err(ApiError::busy());
    }
    s.reset_room(&id)?;
    s.db.lock().unwrap().execute("INSERT OR IGNORE INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,?2,'member',?3)",params![id,m.username,now_ms()])?;
    drop(running);
    s.audit(
        "room.member_added",
        Some(&id),
        json!({"username":m.username,"history_shared":true}),
    )?;
    s.changed();
    Ok(Json(s.conversation(&id)?.unwrap()))
}
pub async fn remove_member(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, user)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&id, &p)?;
    if s.conversation_info(&id)?
        .is_none_or(|c| c["owner"] != p.actor)
    {
        return Err(ApiError::forbidden());
    }
    if s.conversation_info(&id)?.unwrap()["owner"] == user {
        return Err(ApiError::bad("The room owner cannot be removed"));
    }
    let running = s.running.lock().unwrap();
    if let Some(cancel) = running.get(&id) {
        let _ = cancel.send(true);
    }
    s.reset_room(&id)?;
    s.db.lock().unwrap().execute(
        "DELETE FROM room_members WHERE conversation_id=?1 AND username=?2",
        params![id, user],
    )?;
    // Views contain drafts: remove the departed member's room-specific view as well.
    s.db.lock().unwrap().execute(
        "DELETE FROM views WHERE owner=?1 AND json_extract(value,'$.selected_conversation_id')=?2",
        params![user, id],
    )?;
    drop(running);
    s.audit("room.member_removed", Some(&id), json!({"username":user}))?;
    s.changed();
    Ok(Json(json!({"removed":true})))
}
fn native(s: &AppState, id: &str) -> ApiResult<()> {
    if s.conversation_info(id)?
        .is_some_and(|c| c["read_only"] == true)
    {
        Err(ApiError::bad(
            "Imported conversations remain private and read-only. Start a new room to share.",
        ))
    } else {
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentWrite {
    id: String,
    config: Agent,
}
pub async fn agents(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    let db = s.db.lock().unwrap();
    let mut q = db.prepare("SELECT id,config FROM agent_endpoints ORDER BY id")?;
    let items = q
        .query_map([], |r| {
            let config: Value = serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default();
            let mut v = json!({"id":r.get::<_,String>(0)?,"label":config["label"]});
            if p.admin {
                v["config"] = config;
            }
            Ok(v)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"items":items})))
}
pub async fn add_agent(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(a): Json<AgentWrite>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    if a.id.is_empty()
        || a.id.len() > 80
        || !a
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(ApiError::bad(
            "Use a short, stable agent ID containing letters, numbers, hyphens or underscores",
        ));
    }
    a.config
        .validate()
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let _update_activity = crate::update_admission::begin_configuration(&s)?;
    let running = s.running.lock().unwrap();
    if !running.is_empty() {
        return Err(ApiError::busy());
    }
    let current = s.agent(&a.id)?;
    if current.is_none() {
        let count: i64 =
            s.db.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM agent_endpoints", [], |r| r.get(0))?;
        if count >= 100 {
            return Err(ApiError::bad("At most 100 connections can be configured"));
        }
    }
    if a.id == "default" {
        let mut c = crate::config::Config::load(&s.home)?;
        c.agent = a.config.clone();
        c.save(&s.home)?;
    }
    {
        let db = s.db.lock().unwrap();
        let tx = db.unchecked_transaction()?;
        tx.execute("DELETE FROM grants WHERE conversation_id IN (SELECT conversation_id FROM room_agents WHERE agent_id=?1)",[&a.id])?;
        tx.execute(
            "UPDATE room_agents SET session_id=NULL,endpoint=NULL,last_seq=0 WHERE agent_id=?1",
            [&a.id],
        )?;
        tx.execute("INSERT INTO agent_endpoints(id,config,updated_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET config=excluded.config,updated_at=excluded.updated_at",params![a.id,serde_json::to_string(&a.config).unwrap(),now_ms()])?;
        tx.commit()?;
    }
    drop(running);
    s.audit(
        "agent.configured",
        None,
        json!({"id":a.id,"label":a.config.label}),
    )?;
    s.changed();
    Ok(Json(json!({"id":a.id,"saved":true})))
}
pub async fn delete_agent(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    if id == "default" {
        return Err(ApiError::bad(
            "Keep the default connection; it can be edited",
        ));
    }
    let _update_activity = crate::update_admission::begin_configuration(&s)?;
    let db = s.db.lock().unwrap();
    let in_use: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM room_agents WHERE agent_id=?1)",
        [&id],
        |r| r.get(0),
    )?;
    if in_use {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Remove this agent from its rooms first".into(),
        ));
    }
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM tokens WHERE id=?1",
        [format!("context-agent:{id}")],
    )?;
    tx.execute("DELETE FROM agent_endpoints WHERE id=?1", [&id])?;
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(Json(json!({"deleted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachAgent {
    agent_id: String,
    #[serde(default)]
    share_history: bool,
}
pub async fn attach_agent(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(a): Json<AttachAgent>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&id, &p)?;
    native(&s, &id)?;
    if crate::huddles::is_huddle(&s, &id)? {
        return Err(ApiError::bad(
            "Enroll the existing external participant for this huddle; do not start a second ACP session",
        ));
    }
    if !a.share_history {
        return Err(ApiError::bad(
            "Confirm share_history:true to let this agent read the room's existing messages and files",
        ));
    }
    if s.agent(&a.agent_id)?.is_none() {
        return Err(ApiError::bad("Choose a configured agent connection"));
    }
    if s.room_agents(&id)?.len() >= 8 {
        return Err(ApiError::bad("A room supports at most eight agents"));
    }
    let running = s.running.lock().unwrap();
    if running.contains_key(&id) {
        return Err(ApiError::busy());
    }
    s.reset_room(&id)?;
    s.db.lock().unwrap().execute(
        "INSERT OR IGNORE INTO room_agents(conversation_id,agent_id) VALUES(?1,?2)",
        params![id, a.agent_id],
    )?;
    drop(running);
    s.audit(
        "room.agent_added",
        Some(&id),
        json!({"agent_id":a.agent_id,"history_shared":true}),
    )?;
    s.changed();
    Ok(Json(s.conversation(&id)?.unwrap()))
}
pub async fn detach_agent(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, agent)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&id, &p)?;
    native(&s, &id)?;
    if crate::huddles::is_huddle(&s, &id)? {
        return Err(ApiError::bad(
            "Revoke this huddle participant through its participant control",
        ));
    }
    let running = s.running.lock().unwrap();
    if running.contains_key(&id) {
        return Err(ApiError::busy());
    }
    s.reset_room(&id)?;
    s.db.lock().unwrap().execute(
        "DELETE FROM room_agents WHERE conversation_id=?1 AND agent_id=?2",
        params![id, agent],
    )?;
    drop(running);
    s.audit("room.agent_removed", Some(&id), json!({"agent_id":agent}))?;
    s.changed();
    Ok(Json(json!({"removed":true})))
}
