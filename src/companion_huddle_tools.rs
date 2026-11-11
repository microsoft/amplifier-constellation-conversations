//! Call-owned tools for coordination. The shared huddle service owns rooms,
//! participants and receipts; the call supplies only its actual saved source.
use crate::{
    error::{ApiError, ApiResult},
    huddle_auto,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/v1/companion/calls/{id}/tools/huddles", get(directory))
        .route(
            "/api/v1/companion/calls/{id}/tools/huddles/ensure",
            post(ensure),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/huddles/ensure/{request}",
            get(ensure_status),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/huddles/{room}/messages",
            get(messages),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/huddles/{room}/posts",
            post(post_message),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/huddles/{room}/posts/{request}",
            get(post_status),
        )
}

#[path = "companion_huddle_source.rs"]
mod source;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Directory {
    pub installation_id: Option<String>,
    pub after: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ensure {
    pub installation_id: String,
    pub request_id: String,
    pub delegation_id: String,
    pub title: String,
    pub purpose: String,
    pub peer_installation_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub installation_id: String,
    pub after: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub installation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Post {
    pub installation_id: String,
    pub delegation_id: String,
    pub request_id: String,
    pub text: String,
    #[serde(default)]
    pub target_agent_ids: Vec<String>,
    pub kind: String,
    pub outcome: Option<crate::huddles::Outcome>,
}

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}
fn text(value: &str, bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= bytes && !value.contains('\0')
}
fn delegation(value: &str) -> bool {
    !value.is_empty() && value.len() <= 160 && !value.chars().any(char::is_control)
}
fn ids(values: &[String], maximum: usize) -> bool {
    values.len() <= maximum
        && values.iter().all(|v| id(v))
        && values
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == values.len()
}
impl Directory {
    fn valid(&self) -> bool {
        self.installation_id.as_deref().is_none_or(id)
            && self.after.is_none_or(|v| v >= 0)
            && self.limit.is_none_or(|v| (1..=50).contains(&v))
            && (self.installation_id.is_some() || self.after.is_none() && self.limit.is_none())
    }
}
impl Ensure {
    fn valid(&self) -> bool {
        id(&self.installation_id)
            && uuid(&self.request_id)
            && delegation(&self.delegation_id)
            && text(&self.title, 480)
            && self.title.chars().count() <= 120
            && text(&self.purpose, 2000)
            && ids(&self.peer_installation_ids, 7)
    }
}
impl Page {
    fn valid(&self) -> bool {
        id(&self.installation_id)
            && self.after.is_none_or(|v| v >= 0)
            && self.limit.is_none_or(|v| (1..=100).contains(&v))
    }
}
impl Post {
    fn valid(&self) -> bool {
        let outcome = self
            .outcome
            .as_ref()
            .and_then(|o| serde_json::to_value(o).ok());
        id(&self.installation_id)
            && delegation(&self.delegation_id)
            && uuid(&self.request_id)
            && text(&self.text, crate::huddles::MAX_TEXT)
            && ids(&self.target_agent_ids, 8)
            && match (self.kind.as_str(), outcome) {
                ("message", None) => true,
                ("outcome", Some(o)) => {
                    ["progress", "result", "blocked", "decision"]
                        .contains(&o["kind"].as_str().unwrap_or(""))
                        && o["evidence"].as_array().is_some_and(|e| {
                            e.len() <= 8
                                && e.iter().all(|item| {
                                    item["label"].as_str().is_some_and(|v| text(v, 200))
                                        && item["reference"].as_str().is_some_and(|v| text(v, 1000))
                                })
                        })
                        && o.to_string().len() <= 8000
                }
                _ => false,
            }
    }
}

/// Exact MCP arguments are checked before an allow-once permission response.
/// Each HTTP handler also checks the call, installation and operation authority.
pub(crate) fn permission_shape_valid(tool: &str, args: &Value) -> bool {
    match tool {
        "companion_huddles" => {
            serde_json::from_value::<Directory>(args.clone()).is_ok_and(|v| v.valid())
        }
        "companion_huddle_ensure" => {
            serde_json::from_value::<Ensure>(args.clone()).is_ok_and(|v| v.valid())
        }
        "companion_huddle_read" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Read {
                installation_id: String,
                huddle_id: String,
                after: Option<i64>,
                limit: Option<usize>,
            }
            serde_json::from_value::<Read>(args.clone()).is_ok_and(|v| {
                uuid(&v.huddle_id)
                    && Page {
                        installation_id: v.installation_id,
                        after: v.after,
                        limit: v.limit,
                    }
                    .valid()
            })
        }
        "companion_huddle_post" => {
            let Some(mut args) = args.as_object().cloned() else {
                return false;
            };
            let Some(room) = args.remove("huddle_id") else {
                return false;
            };
            room.as_str().is_some_and(uuid)
                && serde_json::from_value::<Post>(Value::Object(args)).is_ok_and(|v| v.valid())
        }
        "companion_huddle_status" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Status {
                installation_id: String,
                request_id: String,
                operation: String,
                huddle_id: Option<String>,
            }
            serde_json::from_value::<Status>(args.clone()).is_ok_and(|v| {
                id(&v.installation_id)
                    && uuid(&v.request_id)
                    && match v.operation.as_str() {
                        "ensure" => v.huddle_id.is_none(),
                        "post" => v.huddle_id.as_deref().is_some_and(uuid),
                        _ => false,
                    }
            })
        }
        _ => false,
    }
}

fn installation(
    s: &AppState,
    owner: &str,
    agent: &str,
    selected: &str,
) -> ApiResult<huddle_auto::Installation> {
    if !id(selected) {
        return Err(ApiError::bad(
            "Use an installation ID from companion_huddles",
        ));
    }
    huddle_auto::local_installations(s, owner, agent)?
        .into_iter()
        .find(|v| v.id == selected)
        .ok_or_else(ApiError::forbidden)
}

/// Discovery may include older or external rooms belonging to this installed
/// participant. Their outcomes still return to their immutable original parent;
/// current-call writes must not route a new report into another conversation.
/// A later delegation in this same private parent retains its own post Source.
fn same_parent(db: &Connection, room: &str, parent: &str, owner: &str) -> rusqlite::Result<bool> {
    let raw: Option<String> = db
        .query_row("SELECT origin FROM huddles WHERE room_id=?1", [room], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(raw.is_some_and(|raw| {
        matches!(
            serde_json::from_str::<crate::huddles::Origin>(&raw),
            Ok(crate::huddles::Origin::Conversation { conversation_id, actor, .. })
                if conversation_id == parent && actor.id == owner && actor.kind == "human"
        )
    }))
}

/// Small installation metadata beside the existing read catalog. Do not copy
/// room transcripts or suggest that configured teammates have received work.
pub(crate) fn catalog(s: &AppState, call: &str) -> anyhow::Result<Option<Value>> {
    let row: Option<(String, String, String)> = s.db.lock().unwrap().query_row(
        "SELECT owner,agent_id,conversation_id FROM companion_calls WHERE id=?1 AND state IN ('connecting','connected')",
        [call], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;
    let Some((owner, agent, parent)) = row else {
        return Ok(None);
    };
    if !crate::companion::room_identity_valid(s, &parent)? {
        return Ok(None);
    }
    let Ok(installed) = huddle_auto::local_installations(s, &owner, &agent) else {
        return Ok(None);
    };
    if installed.is_empty() {
        return Ok(None);
    }
    let omitted = installed.len().saturating_sub(16);
    let items = installed
        .into_iter()
        .take(16)
        .map(|i| {
            json!({
                "installation_id":i.id,"label":i.label,"connection_id":i.connection_id,
                "service":i.service,"namespace":i.namespace
            })
        })
        .collect::<Vec<_>>();
    Ok(Some(
        json!({"installations":items,"omitted":omitted,"discovery_tool":"companion_huddles",
        "ensure_tool":"companion_huddle_ensure","availability":"unknown",
        "guidance":"For useful teammate coordination, discover approved peers and create or reuse a huddle with this request's exact delegation_id. No manual room setup is needed. This call can post only to huddles whose original conversation is this same call conversation; older-call and external rooms remain inspectable. A later delegation in this call keeps its own source without changing the room's original request or return destination. Keep routine discussion in the huddle; return meaningful outcomes. Saved room or post receipts do not prove peers accepted or completed work."}),
    ))
}

pub(crate) fn permission_valid(
    s: &AppState,
    call: &str,
    owner: &str,
    agent: &str,
    parent: &str,
    tool: &str,
    args: &Value,
) -> anyhow::Result<bool> {
    if !permission_shape_valid(tool, args) {
        return Ok(false);
    }
    if tool == "companion_huddles" && args.get("installation_id").is_none() {
        return Ok(huddle_auto::local_installations(s, owner, agent).is_ok());
    }
    let Some(selected) = args["installation_id"].as_str() else {
        return Ok(false);
    };
    if installation(s, owner, agent, selected).is_err() {
        return Ok(false);
    }
    if ["companion_huddle_ensure", "companion_huddle_post"].contains(&tool) {
        let db = s.db.lock().unwrap();
        let delegation = args["delegation_id"].as_str().unwrap();
        let source_valid = if tool == "companion_huddle_post" {
            source::resolve_post_in(
                s,
                &db,
                call,
                owner,
                agent,
                parent,
                delegation,
                args["huddle_id"].as_str().unwrap(),
                selected,
            )
            .is_ok()
        } else {
            source::resolve_in(s, &db, call, owner, agent, parent, delegation).is_ok()
        };
        return Ok(source_valid
            && (tool != "companion_huddle_post"
                || same_parent(&db, args["huddle_id"].as_str().unwrap(), parent, owner)?));
    }
    Ok(true)
}

pub async fn directory(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(call): Path<String>,
    Query(page): Query<Directory>,
) -> ApiResult<Json<Value>> {
    if !page.valid() {
        return Err(ApiError::bad(
            "Choose a discovered installation before paging its huddles",
        ));
    }
    let (owner, agent, _) = super::authorize(&s, &call, &h)?;
    let locals = huddle_auto::local_installations(&s, &owner, &agent)?;
    let Some(selected) = page.installation_id else {
        return Ok(Json(
            json!({"version":1,"installations":locals,"availability":"unknown"}),
        ));
    };
    let selected = locals
        .into_iter()
        .find(|v| v.id == selected)
        .ok_or_else(ApiError::forbidden)?;
    let roster = huddle_auto::roster_for(&s, &selected)?;
    let rooms = huddle_auto::discovery_for(
        &s,
        &selected,
        huddle_auto::Page {
            after: page.after.unwrap_or(0),
            limit: page.limit,
        },
    )?;
    Ok(Json(
        json!({"version":1,"installation":selected,"roster":roster,"huddles":rooms}),
    ))
}

pub async fn ensure(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(call): Path<String>,
    Json(body): Json<Ensure>,
) -> ApiResult<Json<Value>> {
    if !body.valid() {
        return Err(ApiError::bad(
            "Use a discovered installation, exact delegation and bounded huddle request",
        ));
    }
    let (owner, agent, parent) = super::authorize(&s, &call, &h)?;
    let selected = installation(&s, &owner, &agent, &body.installation_id)?;
    let origin = source::resolve_in(
        &s,
        &s.db.lock().unwrap(),
        &call,
        &owner,
        &agent,
        &parent,
        &body.delegation_id,
    )?;
    let expected = origin.clone();
    let request = huddle_auto::Ensure {
        request_id: body.request_id,
        title: body.title,
        purpose: body.purpose,
        origin,
        peer_installation_ids: body.peer_installation_ids,
    };
    let result = huddle_auto::ensure_for_guarded(&s, &selected, request, |db| {
        if source::resolve_in(&s, db, &call, &owner, &agent, &parent, &body.delegation_id)?
            != expected
        {
            return Err(ApiError::forbidden());
        }
        Ok(())
    })?;
    Ok(Json(result))
}

pub async fn ensure_status(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((call, request)): Path<(String, String)>,
    Query(query): Query<Installation>,
) -> ApiResult<Json<Value>> {
    if !uuid(&request) {
        return Err(ApiError::bad("Use the original ensure request UUID"));
    }
    let (owner, agent, _) = super::authorize_receipt(&s, &call, &h)?;
    let selected = installation(&s, &owner, &agent, &query.installation_id)?;
    Ok(Json(huddle_auto::ensure_status_for(
        &s, &selected, &request,
    )?))
}

pub async fn messages(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((call, room)): Path<(String, String)>,
    Query(page): Query<Page>,
) -> ApiResult<Json<Value>> {
    if !uuid(&room) || !page.valid() {
        return Err(ApiError::bad(
            "Use a returned huddle ID and bounded message cursor",
        ));
    }
    let (owner, agent, _) = super::authorize(&s, &call, &h)?;
    let selected = installation(&s, &owner, &agent, &page.installation_id)?;
    Ok(Json(crate::huddles::messages_for_installation(
        &s,
        &selected,
        &room,
        crate::huddles::Page {
            after: page.after,
            limit: page.limit,
        },
    )?))
}

pub async fn post_message(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((call, room)): Path<(String, String)>,
    Json(body): Json<Post>,
) -> ApiResult<Json<Value>> {
    if !uuid(&room) || !body.valid() {
        return Err(ApiError::bad(
            "Use a returned huddle ID and a bounded message or outcome",
        ));
    }
    let (owner, agent, parent) = super::authorize(&s, &call, &h)?;
    let selected = installation(&s, &owner, &agent, &body.installation_id)?;
    let origin = source::resolve_post_in(
        &s,
        &s.db.lock().unwrap(),
        &call,
        &owner,
        &agent,
        &parent,
        &body.delegation_id,
        &room,
        &body.installation_id,
    )?;
    let crate::huddles::Origin::Conversation {
        message_id,
        source_revision,
        occurred_at,
        conversation_id,
        ..
    } = &origin
    else {
        return Err(ApiError::forbidden());
    };
    // The model authors coordination text, not a human identity or proof of
    // execution. Source references come from the current actual delegation.
    let post = serde_json::from_value::<crate::huddles::Post>(json!({
        "request_id":body.request_id,"text":body.text,"target_agent_ids":body.target_agent_ids,
        "kind":body.kind,"outcome":body.outcome,
        "source":{"id":message_id,"revision":source_revision,"occurred_at":occurred_at,"conversation_id":conversation_id}
    })).map_err(anyhow::Error::from)?;
    Ok(Json(crate::huddles::post_for_installation_guarded(
        &s,
        &selected,
        &room,
        post,
        |db| {
            if source::resolve_post_in(
                &s,
                db,
                &call,
                &owner,
                &agent,
                &parent,
                &body.delegation_id,
                &room,
                &body.installation_id,
            )? != origin
            {
                return Err(ApiError::forbidden());
            }
            if !same_parent(db, &room, &parent, &owner)? {
                return Err(ApiError(
                    axum::http::StatusCode::FORBIDDEN,
                    "This huddle returns to another conversation. Use a huddle for this call's request."
                        .into(),
                ));
            }
            Ok(())
        },
    )?))
}

pub async fn post_status(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((call, room, request)): Path<(String, String, String)>,
    Query(query): Query<Installation>,
) -> ApiResult<Json<Value>> {
    if !uuid(&room) || !uuid(&request) {
        return Err(ApiError::bad("Use the original huddle and post UUIDs"));
    }
    let (owner, agent, _) = super::authorize_receipt(&s, &call, &h)?;
    let selected = installation(&s, &owner, &agent, &query.installation_id)?;
    Ok(Json(crate::huddles::post_status_for_installation(
        &s, &selected, &room, &request,
    )?))
}
