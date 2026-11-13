//! Optional source bridge: public ACP observations, never native agent storage.
use crate::{
    auth, config,
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
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path as FilePath, sync::Arc};

/// Keep the original Toad credential, source and import IDs when upgrading.
/// The one-time marker prevents a revoked legacy source from returning on restart.
pub fn migrate_legacy(
    home: &FilePath,
    c: &config::Config,
    db: &rusqlite::Connection,
) -> anyhow::Result<()> {
    let done: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name='named-import-sources')",
        [],
        |r| r.get(0),
    )?;
    if done {
        return Ok(());
    }
    let path = home.join("secrets/import-token");
    let token = if path.exists() {
        std::fs::read_to_string(&path)?.trim().to_owned()
    } else {
        let token = auth::random_token();
        config::private_write(&path, token.as_bytes())?;
        token
    };
    anyhow::ensure!(token.len() >= 32, "Invalid import credential file");
    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM tokens WHERE id='source-importer'", [])?;
    tx.execute("INSERT INTO tokens(hash,id,scopes,expires_at) VALUES(?1,'source-importer','[\"import\"]',?2)", params![auth::digest(&token), now_ms() + 100_i64 * 366 * 86400 * 1000])?;
    tx.execute("INSERT INTO import_sources(source,surface,token_id,created_at,updated_at) VALUES(?1,?2,'source-importer',?3,?3)", params![c.import_source,c.import_surface,now_ms()])?;
    tx.execute(
        "INSERT INTO migrations(name) VALUES('named-import-sources')",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    source: String,
    surface: String,
}
fn valid_source(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 80
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn valid_surface(surface: &str) -> bool {
    !surface.is_empty()
        && surface != "web"
        && surface.len() <= 40
        && surface
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_ ".contains(&b))
}
pub async fn sources(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", false)?;
    let db = s.db.lock().unwrap();
    let mut query = db.prepare(
        "SELECT source,surface,enabled,created_at,updated_at FROM import_sources ORDER BY source",
    )?;
    let items = query.query_map([], |r| Ok(json!({"source":r.get::<_,String>(0)?,"surface":r.get::<_,String>(1)?,"enabled":r.get::<_,bool>(2)?,"created_at":r.get::<_,i64>(3)?,"updated_at":r.get::<_,i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"items":items})))
}
fn issue(s: &AppState, source: &str, surface: Option<&str>) -> ApiResult<Value> {
    // Names become private filenames. Keep source identifiers path-free.
    if !valid_source(source) || surface.is_some_and(|v| !valid_surface(v)) {
        return Err(ApiError::bad(
            "Use a source name with letters, digits, hyphens, dots or underscores, and a non-web surface",
        ));
    }
    let token = auth::random_token();
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let old: Option<(String, String, bool)> = tx
        .query_row(
            "SELECT surface,token_id,enabled FROM import_sources WHERE source=?1",
            [source],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (surface, token_id, file) = match (old, surface) {
        (Some((old_surface, _, true)), Some(_)) => {
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!(
                    "Source {source} already exists on {old_surface}; rotate its credential to replace it"
                ),
            ));
        }
        (Some((old_surface, id, _)), candidate) => {
            if candidate.is_some_and(|v| v != old_surface) {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "An import source's original surface cannot change".into(),
                ));
            }
            let file = if id == "source-importer" {
                s.home.join("secrets/import-token")
            } else {
                s.home
                    .join("secrets/import-sources")
                    .join(format!("{source}-token"))
            };
            (old_surface, id, file)
        }
        (None, Some(surface)) => (
            surface.to_owned(),
            format!("source-importer:{source}"),
            s.home
                .join("secrets/import-sources")
                .join(format!("{source}-token")),
        ),
        (None, None) => {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Import source not found".into(),
            ));
        }
    };
    config::private_dir(file.parent().unwrap())?;
    config::private_write(&file, token.as_bytes())?;
    tx.execute("DELETE FROM tokens WHERE id=?1", [&token_id])?;
    tx.execute(
        "INSERT INTO tokens(hash,id,scopes,expires_at) VALUES(?1,?2,'[\"import\"]',?3)",
        params![
            auth::digest(&token),
            token_id,
            now_ms() + 100_i64 * 366 * 86400 * 1000
        ],
    )?;
    tx.execute("INSERT INTO import_sources(source,surface,token_id,enabled,created_at,updated_at) VALUES(?1,?2,?3,1,?4,?4) ON CONFLICT(source) DO UPDATE SET enabled=1,updated_at=excluded.updated_at",params![source,surface,token_id,now_ms()])?;
    tx.commit()?;
    Ok(json!({"source":source,"surface":surface,"enabled":true,"token":token,"token_file":file}))
}
pub async fn create_source(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(input): Json<Source>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let result = issue(&s, &input.source, Some(&input.surface))?;
    s.audit(
        "import_source.created",
        None,
        json!({"source":input.source,"surface":input.surface}),
    )?;
    s.changed();
    Ok(Json(result))
}
pub async fn rotate_source(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(source): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let result = issue(&s, &source, None)?;
    s.audit("import_source.rotated", None, json!({"source":source}))?;
    s.changed();
    Ok(Json(result))
}
pub async fn revoke_source(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(source): Path<String>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        if tx.execute(
            "UPDATE import_sources SET enabled=0,updated_at=?2 WHERE source=?1",
            params![source, now_ms()],
        )? == 0
        {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "Import source not found".into(),
            ));
        }
        tx.execute(
            "DELETE FROM tokens WHERE id=(SELECT token_id FROM import_sources WHERE source=?1)",
            [&source],
        )?;
        tx.commit()?;
    }
    s.audit("import_source.revoked", None, json!({"source":source}))?;
    s.changed();
    Ok(Json(
        json!({"source":source,"enabled":false,"transcripts_retained":true}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    label: String,
    surface: String,
    messages: Vec<Message>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    id: String,
    role: String,
    text: String,
    created_at: i64,
}
pub async fn put(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((source, session)): Path<(String, String)>,
    Json(i): Json<Import>,
) -> ApiResult<Json<Value>> {
    let principal = auth::check(&s, &h, "import", true)?;
    if let Some(id) = principal
        .actor
        .strip_prefix("token:")
        .filter(|id| id.starts_with("source-importer"))
    {
        let allowed: bool = s.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM import_sources WHERE source=?1 AND surface=?2 AND token_id=?3 AND enabled=1)",params![source,i.surface,id],|r|r.get(0))?;
        if !allowed {
            return Err(ApiError::forbidden());
        }
    }
    if source.is_empty()
        || source.len() > 80
        || !source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || session.is_empty()
        || session.len() > 200
        || session.chars().any(char::is_control)
    {
        return Err(ApiError::bad(
            "Supply a stable source name and session identifier",
        ));
    }
    if i.label.trim().is_empty()
        || i.label.chars().count() > 120
        || i.surface.is_empty()
        || i.surface == "web"
        || i.surface.len() > 40
        || !i
            .surface
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_ ".contains(&b))
        || i.messages.len() > 500
        || i.messages.iter().map(|m| m.text.len()).sum::<usize>() > 900_000
    {
        return Err(ApiError::bad(
            "Use a source label, a non-web surface, and at most 500 messages / 900 KB of text",
        ));
    }
    let mut ids = HashSet::new();
    let mut prev_at = 0;
    for m in &i.messages {
        if m.id.is_empty()
            || m.id.len() > 200
            || !ids.insert(m.id.clone())
            || !["user", "assistant"].contains(&m.role.as_str())
            || m.created_at < prev_at
            || m.text.len() > 200_000
        {
            return Err(ApiError::bad(
                "Message IDs must be unique; roles user/assistant; timestamps chronological; each message at most 200 KB",
            ));
        }
        prev_at = m.created_at;
    }
    let namespaced = |id: &str| {
        format!(
            "import:{:x}",
            Sha256::digest(format!("{source}\0{session}\0{id}").as_bytes())
        )
    };
    let conversation;
    let appended;
    {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT conversation_id FROM imports WHERE source=?1 AND session_id=?2",
                params![source, session],
                |r| r.get(0),
            )
            .optional()?;
        conversation = if let Some(id) = existing {
            let (surface, readonly): (String, bool) = tx.query_row(
                "SELECT surface,read_only FROM conversations WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if !readonly || surface != i.surface {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "The original source of this conversation cannot change".into(),
                ));
            }
            id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note,surface,read_only) VALUES(?1,?2,?3,?3,'idle','Transcript from an independent ACP experience',?4,1)",params![id,i.label.trim(),now_ms(),i.surface])?;
            tx.execute(
                "INSERT INTO imports(source,session_id,conversation_id) VALUES(?1,?2,?3)",
                params![source, session, id],
            )?;
            id
        };
        let old = {
            let mut q=tx.prepare("SELECT id,role,text,created_at FROM messages WHERE conversation_id=?1 ORDER BY seq")?;
            q.query_map([&conversation], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if old.len() > i.messages.len() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Import snapshots are append-only; previous messages cannot be removed".into(),
            ));
        }
        for ((id, role, text, at), m) in old.iter().zip(i.messages.iter()) {
            if *id != namespaced(&m.id) || *role != m.role || *text != m.text || *at != m.created_at
            {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Import snapshots must preserve the existing message prefix".into(),
                ));
            }
        }
        appended = i.messages.len() - old.len();
        for m in i.messages.iter().skip(old.len()) {
            tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at) VALUES(?1,?2,?3,?4,'complete',?5,?5)",params![namespaced(&m.id),conversation,m.role,m.text,m.created_at])?;
        }
        tx.execute(
            "UPDATE conversations SET owner=?2 WHERE id=?1 AND owner=''",
            params![conversation, s.config.owner],
        )?;
        tx.execute("INSERT OR IGNORE INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,?2,'owner',?3)",params![conversation,s.config.owner,now_ms()])?;
        // Import connections currently represent the installation's default
        // agent. Keep that source ownership explicit for scoped retrieval.
        if let Some(agent) = &s.config.agent {
            tx.execute(
                "INSERT OR IGNORE INTO room_agents(conversation_id,agent_id) VALUES(?1,'default')",
                [&conversation],
            )?;
            tx.execute("UPDATE messages SET actor=CASE WHEN role='user' THEN ?2 ELSE ?3 END WHERE conversation_id=?1 AND actor=''",params![conversation,s.config.owner,agent.label])?;
        } else {
            tx.execute("UPDATE messages SET actor=?2 WHERE conversation_id=?1 AND role='user' AND actor=''",params![conversation,s.config.owner])?;
        }
        if appended > 0 {
            tx.execute(
                "UPDATE conversations SET updated_at=?2,title=?3 WHERE id=?1",
                params![conversation, now_ms(), i.label.trim()],
            )?;
        }
        tx.commit()?;
    }
    if appended > 0 {
        s.audit(
            "conversation.imported",
            Some(&conversation),
            json!({"source":source,"surface":i.surface,"appended":appended}),
        )?;
        s.changed();
    }
    Ok(Json(
        json!({"conversation_id":conversation,"appended":appended,"read_only":true,"surface":i.surface}),
    ))
}
