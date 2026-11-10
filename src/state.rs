use crate::{
    auth,
    config::{Config, private_dir, private_write},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{Semaphore, watch};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub struct AppState {
    pub companion: crate::companion::Runtime,
    pub permissions: crate::permissions::Permissions,
    pub elicitations: crate::elicitation::Elicitations,
    pub home: PathBuf,
    pub config: Config,
    pub db: Mutex<Connection>,
    pub changes: watch::Sender<u64>,
    pub login_gate: Arc<Semaphore>,
    pub turn_gate: Arc<Semaphore>,
    pub running: Mutex<HashMap<String, watch::Sender<bool>>>,
    pub(crate) active_steering: Mutex<HashMap<(String, String), crate::delivery::ActiveSteering>>,
}
impl AppState {
    pub fn open(home: PathBuf) -> Result<Arc<Self>> {
        private_dir(&home)?;
        private_dir(&home.join("secrets"))?;
        let config = Config::load(&home)?;
        let db = Connection::open(home.join("state.sqlite3"))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS conversations(id TEXT PRIMARY KEY,title TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,status TEXT NOT NULL,note TEXT NOT NULL,session_id TEXT,endpoint TEXT,agent_info TEXT,surface TEXT NOT NULL DEFAULT 'web',read_only INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,role TEXT NOT NULL,text TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS messages_conversation ON messages(conversation_id,seq);
            CREATE TABLE IF NOT EXISTS tokens(hash TEXT PRIMARY KEY,id TEXT NOT NULL,scopes TEXT NOT NULL,expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS browser_sessions(hash TEXT PRIMARY KEY,owner TEXT NOT NULL,csrf TEXT NOT NULL,expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS login_attempts(key TEXT PRIMARY KEY,failures INTEGER NOT NULL,locked_until INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS grants(hash TEXT PRIMARY KEY,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,expires_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS views(id TEXT PRIMARY KEY,value TEXT NOT NULL,revision INTEGER NOT NULL,updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS audit(seq INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,kind TEXT NOT NULL,conversation_id TEXT,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS imports(source TEXT NOT NULL,session_id TEXT NOT NULL,conversation_id TEXT UNIQUE NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,PRIMARY KEY(source,session_id));
            CREATE TABLE IF NOT EXISTS import_sources(source TEXT PRIMARY KEY,surface TEXT NOT NULL,token_id TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS migrations(name TEXT PRIMARY KEY);")?;
        crate::rooms::migrate(&db, &config)?;
        crate::delivery::migrate(&db)?;
        crate::companion::migrate(&db)?;
        crate::companion_tools::migrate(&db)?;
        crate::companion_surface::migrate(&db)?;
        crate::huddles::migrate(&db)?;
        crate::huddle_auto::migrate(&db)?;
        db.execute("UPDATE conversations SET status='error',note='The service restarted during this turn. Review delivery status before continuing; work was not replayed.' WHERE status='working'", [])?;
        db.execute(
            "UPDATE messages SET status='interrupted' WHERE status='streaming'",
            [],
        )?;
        for (file, id, scopes) in [
            ("control-token", "local-control", "[\"admin\"]"),
            ("context-token", "context-provider", "[\"context\"]"),
        ] {
            let path = home.join("secrets").join(file);
            let token = if path.exists() {
                std::fs::read_to_string(&path)?.trim().to_owned()
            } else {
                let token = auth::random_token();
                private_write(&path, token.as_bytes())?;
                token
            };
            anyhow::ensure!(token.len() >= 32, "Invalid credential file");
            db.execute("DELETE FROM tokens WHERE id=?1", [id])?;
            db.execute(
                "INSERT INTO tokens(hash,id,scopes,expires_at) VALUES(?1,?2,?3,?4)",
                params![
                    auth::digest(&token),
                    id,
                    scopes,
                    now_ms() + 100_i64 * 366 * 86400 * 1000
                ],
            )?;
        }
        crate::imports::migrate_legacy(&home, &config, &db)?;
        Ok(Arc::new(Self {
            companion: crate::companion::Runtime::default(),
            permissions: crate::permissions::Permissions::default(),
            elicitations: crate::elicitation::Elicitations::default(),
            home,
            config,
            db: Mutex::new(db),
            changes: watch::channel(0).0,
            login_gate: Arc::new(Semaphore::new(2)),
            turn_gate: Arc::new(Semaphore::new(4)),
            running: Mutex::new(HashMap::new()),
            active_steering: Mutex::new(HashMap::new()),
        }))
    }
    pub fn changed(&self) {
        self.changes.send_modify(|v| *v += 1);
    }
    pub fn audit(&self, kind: &str, conversation: Option<&str>, data: Value) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO audit(at,kind,conversation_id,data) VALUES(?1,?2,?3,?4)",
            params![now_ms(), kind, conversation, data.to_string()],
        )?;
        db.execute(
            "DELETE FROM audit WHERE seq <= (SELECT COALESCE(MAX(seq),0)-5000 FROM audit)",
            [],
        )?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt=db.prepare("SELECT id,title,created_at,updated_at,status,note,agent_info,surface,read_only,owner,shared FROM conversations ORDER BY updated_at DESC LIMIT 200")?;
        Ok(stmt
            .query_map([], conversation_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn list_for_agent(&self, user: &str, agent: &str) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut q=db.prepare("SELECT c.id,c.title,c.created_at,c.updated_at,c.status,c.note,c.agent_info,c.surface,c.read_only,c.owner,c.shared FROM conversations c JOIN room_members m ON m.conversation_id=c.id AND m.username=?1 JOIN room_agents a ON a.conversation_id=c.id AND a.agent_id=?2 ORDER BY c.updated_at DESC,c.id DESC LIMIT 200")?;
        Ok(q.query_map(params![user, agent], conversation_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn conversation(&self, id: &str) -> Result<Option<Value>> {
        let mut row = self.conversation_info(id)?;
        if let Some(ref mut c) = row {
            let mut messages = self.messages(id, 100)?;
            self.present_messages(id, &mut messages)?;
            c["messages"] = json!(messages);
            c["deliveries"] = json!(crate::delivery::list(self, id)?);
            c["members"] = json!(self.members(id)?);
            c["agents"] = json!(self.room_agents(id)?);
            c["has_older"] = json!(self.db.lock().unwrap().query_row(
                "SELECT count(*)>100 FROM messages WHERE conversation_id=?1",
                [id],
                |r| r.get::<_, bool>(0)
            )?);
        }
        Ok(row)
    }
    pub fn conversation_info(&self, id: &str) -> Result<Option<Value>> {
        let db = self.db.lock().unwrap();
        let mut value = db.query_row("SELECT id,title,created_at,updated_at,status,note,agent_info,surface,read_only,owner,shared FROM conversations WHERE id=?1",[id],conversation_row).optional()?;
        if let Some(room) = &mut value {
            let huddle: Option<Value> = db.query_row("SELECT state,revision FROM huddles WHERE room_id=?1",[id],|r|Ok(json!({"id":id,"state":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?}))).optional()?;
            if let Some(huddle) = huddle {
                room["huddle"] = huddle;
            }
        }
        Ok(value)
    }
    pub fn source_message_page(
        &self,
        id: &str,
        limit: usize,
        before: Option<i64>,
        search: &str,
    ) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        // Cap text inside SQLite before allocating a context projection.
        let mut q=db.prepare("SELECT id,role,substr(text,1,20000),status,created_at,length(CAST(text AS BLOB)),actor,seq FROM (SELECT seq,id,role,text,status,created_at,actor FROM messages WHERE conversation_id=?1 AND (?3 IS NULL OR seq<?3) AND (?4='' OR instr(lower(text),lower(?4))>0) ORDER BY seq DESC LIMIT ?2) ORDER BY seq")?;
        Ok(q.query_map(params![id,limit as i64,before,search],|r|Ok(json!({"id":r.get::<_,String>(0)?,"role":r.get::<_,String>(1)?,"text":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?,"original_bytes":r.get::<_,i64>(5)?,"actor":r.get::<_,String>(6)?,"seq":r.get::<_,i64>(7)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn messages(&self, id: &str, limit: usize) -> Result<Vec<Value>> {
        self.message_page(id, None, limit, "")
    }
    /// Browser presentation only. Canonical message text, source context and
    /// backend input stay unchanged, including already saved call histories.
    pub fn present_messages(&self, room: &str, messages: &mut [Value]) -> Result<()> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(
            "SELECT CASE WHEN typeof(d.request_text)='text' AND length(CAST(d.request_text AS BLOB))<=49152 THEN d.request_text ELSE NULL END
             FROM companion_delegations d JOIN companion_calls c ON c.id=d.call_id
             JOIN messages m ON m.id=d.message_id AND m.conversation_id=c.conversation_id
             WHERE m.id=?1 AND m.conversation_id=?2 AND m.role='user' AND m.actor=c.owner LIMIT 2",
        )?;
        for message in messages {
            if message["role"] != "user" {
                continue;
            }
            let labels = query
                .query_map(params![message["id"].as_str().unwrap_or(""), room], |r| {
                    r.get::<_, Option<String>>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if labels.is_empty() {
                continue;
            }
            // Never recognize orchestration by a text prefix. Ambiguous or
            // missing labels remain neutral instead of inventing caller words.
            let text = if labels.len() == 1 {
                labels[0]
                    .as_deref()
                    .filter(|s| !s.trim().is_empty() && !s.contains('\0'))
            } else {
                None
            };
            message["presentation"] = json!({
                "kind":"spoken_request", "label":"From voice",
                "text":text.unwrap_or("Spoken request"), "excerpt":true,
                "canonical_text_unchanged":true
            });
        }
        Ok(())
    }
    pub fn message_page(
        &self,
        id: &str,
        before: Option<i64>,
        limit: usize,
        search: &str,
    ) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt=db.prepare("SELECT seq,id,role,text,status,created_at,updated_at,actor,agent_id,targets FROM (SELECT * FROM messages WHERE conversation_id=?1 AND (?2 IS NULL OR seq<?2) AND (?3='' OR instr(lower(text),lower(?3))>0) ORDER BY seq DESC LIMIT ?4) ORDER BY seq")?;
        let mut rows=stmt.query_map(params![id,before,search,limit as i64],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,"role":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?,"updated_at":r.get::<_,i64>(6)?,"actor":r.get::<_,String>(7)?,"agent_id":r.get::<_,Option<String>>(8)?,"target_agent_ids":serde_json::from_str::<Value>(&r.get::<_,String>(9)?).unwrap_or(json!([]))})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for m in &mut rows {
            let mut q=db.prepare("SELECT id,name,mime_type,bytes FROM attachments WHERE message_id=?1 ORDER BY created_at")?;
            m["attachments"]=json!(q.query_map([m["id"].as_str().unwrap_or("")], |r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"mime_type":r.get::<_,String>(2)?,"bytes":r.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(rows)
    }
    pub fn create(&self, title: &str) -> Result<Value> {
        self.create_room(title, &self.config.owner, &["default".into()])
    }
    pub fn exists(&self, id: &str) -> Result<bool> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn status(&self, id: &str, status: &str, note: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE conversations SET status=?2,note=?3,updated_at=?4 WHERE id=?1",
            params![id, status, note, now_ms()],
        )?;
        self.changed();
        Ok(())
    }
    pub fn update_message(&self, id: &str, text: &str, status: &str) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE messages SET text=?2,status=?3,updated_at=?4 WHERE id=?1",
            params![id, text, status, now_ms()],
        )?;
        self.changed();
        Ok(())
    }
    pub fn session(&self, id: &str, endpoint: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT session_id FROM conversations WHERE id=?1 AND endpoint=?2",
                params![id, endpoint],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn bind(&self, id: &str, endpoint: &str, session: &str, info: Value) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE conversations SET endpoint=?2,session_id=?3,agent_info=?4 WHERE id=?1",
            params![id, endpoint, session, info.to_string()],
        )?;
        self.changed();
        Ok(())
    }
}
fn conversation_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let info: Option<String> = r.get(6)?;
    Ok(
        json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?,"updated_at":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"note":r.get::<_,String>(5)?,"agent_info":info.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"surface":r.get::<_,String>(7)?,"read_only":r.get::<_,bool>(8)?,"owner":r.get::<_,String>(9)?,"shared":r.get::<_,bool>(10)?}),
    )
}
