//! A source of web conversations, never an agent's private memory store.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, header},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn grant(state: &AppState, conversation: &str) -> anyhow::Result<String> {
    anyhow::ensure!(state.exists(conversation)?, "Unknown conversation");
    let token = auth::random_token();
    let db = state.db.lock().unwrap();
    let owner_scope: bool = db.query_row(
        "SELECT owner=?2 AND shared=0 FROM conversations WHERE id=?1",
        params![conversation, state.config.owner],
        |r| r.get(0),
    )?;
    db.execute("DELETE FROM grants WHERE expires_at<=?1", [now_ms()])?;
    db.execute(
        "INSERT INTO grants(hash,conversation_id,expires_at,owner_scope) VALUES(?1,?2,?3,?4)",
        params![
            auth::digest(&token),
            conversation,
            now_ms() + 8 * 3600 * 1000,
            owner_scope
        ],
    )?;
    Ok(token)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    operation: String,
    access: Access,
    conversation_id: Option<String>,
    thread_id: Option<String>,
    limit: Option<usize>,
    before: Option<i64>,
    query: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Access {
    Owner,
    Grant { grant: String },
}

pub async fn query(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(q): Json<Query>,
) -> ApiResult<Json<Value>> {
    // Source access is an explicit machine capability, never inherited from a
    // browser cookie or a guessed conversation identifier.
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(ApiError::unauthorized());
    }
    auth::check(&state, &headers, "context", false)?;
    execute(&state, q, "default")
}

/// Each credential names one registered ACP connection. A source grant never
/// widens that connection's current room membership.
pub async fn agent_query(
    State(state): State<Arc<AppState>>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(q): Json<Query>,
) -> ApiResult<Json<Value>> {
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(ApiError::unauthorized());
    }
    auth::check(&state, &headers, &format!("context:{agent}"), false)?;
    require_agent(&state, &agent)?;
    execute(&state, q, &agent)
}

fn require_agent(state: &AppState, agent: &str) -> ApiResult<()> {
    if state.agent(agent)?.is_none() {
        return Err(ApiError::missing());
    }
    Ok(())
}

fn attached(state: &AppState, agent: &str, room: &str) -> ApiResult<bool> {
    Ok(state.db.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM room_agents WHERE agent_id=?1 AND conversation_id=?2)",
        params![agent, room],
        |r| r.get(0),
    )?)
}

fn execute(state: &AppState, q: Query, agent: &str) -> ApiResult<Json<Value>> {
    if q.thread_id.is_some() {
        return Err(ApiError::bad(
            "Web conversations do not have separate threads",
        ));
    }
    let mut owner_scope = true;
    let grant_conversation = match &q.access {
        Access::Owner => None,
        Access::Grant { grant } => {
            if grant.len() > 256 {
                return Err(ApiError::forbidden());
            }
            let id = state
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT conversation_id,owner_scope FROM grants WHERE hash=?1 AND expires_at>?2",
                    params![auth::digest(grant), now_ms()],
                    |r| Ok((r.get::<_, String>(0)?,r.get::<_, bool>(1)?)),
                )
                .optional()?
                .ok_or_else(ApiError::forbidden)?;
            owner_scope = id.1 && state.owner_context(&id.0)?;
            if !attached(state, agent, &id.0)? {
                return Err(ApiError::forbidden());
            }
            Some(id.0)
        }
    };
    let selected = q
        .conversation_id
        .as_deref()
        .or(grant_conversation.as_deref());
    if let Some(id) = selected
        && ((!owner_scope && Some(id) != grant_conversation.as_deref())
            || (owner_scope && !state.member(id, &state.config.owner)?))
    {
        return Err(ApiError::forbidden());
    }
    if let Some(id) = selected
        && !attached(state, agent, id)?
    {
        return Err(ApiError::forbidden());
    }
    if q.operation == "authorize" {
        let label = if let Some(id) = selected {
            state.conversation_info(id)?.ok_or_else(ApiError::missing)?["title"].clone()
        } else {
            json!(state.config.label)
        };
        return Ok(Json(
            json!({"owner":owner_scope,"conversation_id":selected,"thread_id":null,"actor_id":if owner_scope{state.config.owner.clone()}else{format!("room:{}",grant_conversation.as_deref().unwrap_or(""))},"surface":"web","label":label,"observed_at":now_ms(),"provider_id":state.config.provider_id()}),
        ));
    }
    let limit = q.limit.unwrap_or(40).clamp(1, 100);
    let search = q.query.as_deref().unwrap_or("");
    if search.len() > 200 {
        return Err(ApiError::bad("Search is too long"));
    }
    let response = match q.operation.as_str() {
        "list" => {
            let all = if owner_scope {
                state.list_for_agent(&state.config.owner, agent)?
            } else {
                grant_conversation
                    .as_deref()
                    .map(|id| state.conversation_info(id))
                    .transpose()?
                    .flatten()
                    .into_iter()
                    .collect()
            };
            let complete = all.len() < 200 && all.len() <= limit;
            let items:Vec<Value>=all.into_iter().take(limit).map(|c| {
                let id=c["id"].as_str().unwrap();
                json!({"id":id,"conversation_id":id,"thread_id":null,"label":c["title"],"surface":c["surface"],"url":source_url(state,id),"updated_at":c["updated_at"],"coverage":if c["read_only"]==true{"Conversation transcript supplied through an authorized source bridge"}else{"Conversation messages observed by this web experience"}})
            }).collect();
            json!({"items":items,"complete":complete,"observed_at":now_ms(),"provider_id":state.config.provider_id()})
        }
        "read" | "orientation" | "search" => {
            let id = selected.ok_or_else(|| ApiError::bad("Select a conversation"))?;
            let c = state.conversation_info(id)?.ok_or_else(ApiError::missing)?;
            let all = state.source_message_page(id, limit + 1, q.before, search)?;
            let count = all.len();
            let mut remaining = 60_000usize;
            let mut messages = Vec::new();
            let mut truncated_any = false;
            for m in all.into_iter().rev().take(limit) {
                let original = m["text"].as_str().unwrap_or("");
                if remaining == 0 {
                    break;
                }
                // Leave space for nearby messages even when the newest response
                // is large. Never return an empty room because one reply is long.
                let mut end = original.len().min(remaining).min(20_000);
                while !original.is_char_boundary(end) {
                    end -= 1;
                }
                let text = &original[..end];
                let original_bytes = m["original_bytes"]
                    .as_u64()
                    .unwrap_or(original.len() as u64) as usize;
                let truncated = end < original_bytes;
                truncated_any |= truncated;
                remaining -= text.len();
                messages.push(json!({"id":m["id"],"seq":m["seq"],"text":text,"truncated":truncated,"original_bytes":original_bytes,"actor":m["actor"],"role":m["role"],"label":m["actor"],"at":m["created_at"],"status":m["status"],"url":source_url(state,id),"thread_id":null}));
            }
            messages.reverse();
            let complete =
                q.before.is_none() && count <= limit && messages.len() == count && !truncated_any;
            let next_cursor = if count > messages.len() {
                messages.first().and_then(|m| m["seq"].as_i64())
            } else {
                None
            };
            json!({"conversation_id":id,"thread_id":null,"label":c["title"],"surface":c["surface"],"read_only":c["read_only"],"messages":messages,"complete":complete,"next_cursor":next_cursor,"query":search,"observed_at":now_ms(),"provider_id":state.config.provider_id(),"url":source_url(state,id),"coverage":if complete{"All observed messages"}else{"Recent bounded conversation context; earlier or larger messages may be omitted"},"owner":state.config.owner})
        }
        _ => {
            return Err(ApiError::bad(
                "Use authorize, list, read, orientation, or search",
            ));
        }
    };
    state.audit(
        "context.read",
        selected,
        json!({"operation":q.operation,"scope":if owner_scope{"owner"}else{"room"},"agent_id":agent}),
    )?;
    Ok(Json(response))
}

fn credential_id(agent: &str) -> String {
    format!("context-agent:{agent}")
}

fn credential_info(state: &AppState, agent: &str, expires_at: Option<i64>) -> Value {
    let path = format!("/agents/{agent}/context/v1/query");
    let url = (!state.config.public_url.is_empty())
        .then(|| format!("{}{path}", state.config.public_url.trim_end_matches('/')));
    json!({"agent_id":agent,"scope":format!("context:{agent}"),"provider_id":state.config.provider_id(),"query_path":path,"query_url":url,"surface":"web","enabled":expires_at.is_some_and(|at| at>now_ms()),"expires_at":expires_at})
}

pub async fn credential(
    State(state): State<Arc<AppState>>,
    Path(agent): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", false)?;
    require_agent(&state, &agent)?;
    let expires_at = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT expires_at FROM tokens WHERE id=?1",
            [credential_id(&agent)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(Json(credential_info(&state, &agent, expires_at)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueCredential {
    expires_in_seconds: Option<i64>,
}

pub async fn issue_credential(
    State(state): State<Arc<AppState>>,
    Path(agent): Path<String>,
    headers: HeaderMap,
    Json(input): Json<IssueCredential>,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", true)?;
    let expires_at = now_ms()
        + input
            .expires_in_seconds
            .unwrap_or(90 * 86400)
            .clamp(60, 90 * 86400)
            * 1000;
    let secret = auth::random_token();
    {
        let db = state.db.lock().unwrap();
        let tx = db.unchecked_transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_endpoints WHERE id=?1)",
            [&agent],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(ApiError::missing());
        }
        tx.execute("DELETE FROM tokens WHERE id=?1", [credential_id(&agent)])?;
        tx.execute(
            "INSERT INTO tokens(hash,id,scopes,expires_at) VALUES(?1,?2,?3,?4)",
            params![
                auth::digest(&secret),
                credential_id(&agent),
                json!([format!("context:{agent}")]).to_string(),
                expires_at
            ],
        )?;
        tx.commit()?;
    }
    state.audit(
        "context.credential_issued",
        None,
        json!({"agent_id":agent,"expires_at":expires_at}),
    )?;
    let mut result = credential_info(&state, &agent, Some(expires_at));
    result["token"] = json!(secret);
    Ok(Json(result))
}

pub async fn revoke_credential(
    State(state): State<Arc<AppState>>,
    Path(agent): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", true)?;
    require_agent(&state, &agent)?;
    state
        .db
        .lock()
        .unwrap()
        .execute("DELETE FROM tokens WHERE id=?1", [credential_id(&agent)])?;
    state.audit(
        "context.credential_revoked",
        None,
        json!({"agent_id":agent}),
    )?;
    Ok(Json(credential_info(&state, &agent, None)))
}
fn source_url(state: &AppState, id: &str) -> Option<String> {
    (!state.config.public_url.is_empty()).then(|| {
        format!(
            "{}/?conversation={id}",
            state.config.public_url.trim_end_matches('/')
        )
    })
}
