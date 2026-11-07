use crate::{
    attachments, auth, context, driver,
    error::{ApiError, ApiResult},
    imports, rooms,
    state::{AppState, now_ms},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};

pub fn router(state: Arc<AppState>) -> Router {
    crate::delivery::resume(&state);
    Router::new()
        .route(
            "/markdown-notices.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../ui/MARKDOWN-NOTICES.txt"),
                )
            }),
        )
        .route(
            "/markdown.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    concat!(
                        include_str!("../ui/markdown.js"),
                        include_str!("../ui/markdown-renderer.js")
                    ),
                )
            }),
        )
        .route(
            "/companion",
            get(|| async { Html(include_str!("../ui/companion.html")) }),
        )
        .route(
            "/companion.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/companion.js"),
                )
            }),
        )
        .route(
            "/companion.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/companion.css"),
                )
            }),
        )
        .route("/", get(index))
        .route("/app.js", get(js))
        .route(
            "/voice.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/voice.js"),
                )
            }),
        )
        .route(
            "/sw.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/sw.js"),
                )
            }),
        )
        .route(
            "/manifest.webmanifest",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/manifest+json")],
                    include_str!("../ui/manifest.webmanifest"),
                )
            }),
        )
        .route(
            "/icon.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../ui/icon.svg"),
                )
            }),
        )
        .route("/style.css", get(css))
        .route("/health", get(|| async { Json(json!({"ok":true})) }))
        .route("/auth/login", post(auth::login))
        .route("/auth/session", get(auth::session))
        .route("/auth/logout", post(auth::logout))
        .route("/api/v1/companion", get(crate::companion::catalog))
        .route(
            "/api/v1/companion/calls/{id}/mcp",
            post(crate::companion_tools::mcp),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/view",
            post(crate::companion_surface::view),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/capture",
            post(crate::companion_surface::capture),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/surface/{operation}",
            get(crate::companion_surface::status),
        )
        .route(
            "/api/v1/companion/calls/{id}/surface/presence",
            post(crate::companion_surface::presence),
        )
        .route(
            "/api/v1/companion/calls/{id}/surface/commands",
            get(crate::companion_surface::pending),
        )
        .route(
            "/api/v1/companion/calls/{id}/surface/commands/{operation}/ack",
            post(crate::companion_surface::ack),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/connections",
            get(crate::companion_tools::connections),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/actions",
            post(crate::companion_tools::action),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/actions/{request}",
            get(crate::companion_tools::status),
        )
        .route(
            "/api/v1/companion/calls/{id}/tools/shared",
            get(crate::companion_tools::shared).post(crate::companion_tools::show),
        )
        .route(
            "/api/v1/companion/agents/{id}/voice-settings",
            get(crate::companion::voice_settings).put(crate::companion::save_voice_settings),
        )
        .route(
            "/api/v1/companion/agents/{id}/capabilities",
            get(crate::companion::capabilities),
        )
        .route("/api/v1/companion/calls", post(crate::companion::create))
        .route("/api/v1/companion/calls/{id}", get(crate::companion::read))
        .route(
            "/api/v1/companion/calls/{id}/uploads",
            post(crate::companion::upload_awareness),
        )
        .route(
            "/api/v1/companion/calls/{id}/results/{message}",
            get(crate::companion::result),
        )
        .route(
            "/api/v1/companion/calls/{id}/heartbeat",
            post(crate::companion::heartbeat),
        )
        .route(
            "/api/v1/companion/calls/{id}/close",
            post(crate::companion::close),
        )
        .route("/api/v1/meta", get(meta))
        .route("/api/v1/config", get(config_get).put(config_put))
        .route("/api/v1/check", post(check))
        .route("/api/v1/agents", get(rooms::agents).post(rooms::add_agent))
        .route(
            "/api/v1/agents/{id}",
            axum::routing::delete(rooms::delete_agent),
        )
        .route("/api/v1/people", get(people))
        .route(
            "/api/v1/conversations/{id}/members",
            get(rooms::members).post(rooms::add_member),
        )
        .route(
            "/api/v1/conversations/{id}/members/{user}",
            axum::routing::delete(rooms::remove_member),
        )
        .route(
            "/api/v1/conversations/{id}/agents",
            post(rooms::attach_agent),
        )
        .route(
            "/api/v1/conversations/{id}/agents/{agent}",
            axum::routing::delete(rooms::detach_agent),
        )
        .route(
            "/api/v1/conversations/{id}/attachments",
            get(attachments::list)
                .post(attachments::upload)
                .layer(DefaultBodyLimit::max(3 * 1024 * 1024)),
        )
        .route(
            "/api/v1/conversations/{id}/attachments/{attachment}",
            get(attachments::download).delete(attachments::remove),
        )
        .route("/api/v1/conversations", get(list).post(create))
        .route(
            "/api/v1/conversations/{id}",
            get(read).patch(rename).delete(remove),
        )
        .route(
            "/api/v1/conversations/{id}/messages",
            get(message_history).post(send),
        )
        .route(
            "/api/v1/conversations/{id}/deliveries",
            get(crate::delivery::read),
        )
        .route(
            "/api/v1/conversations/{id}/deliveries/{input}/resolve",
            post(crate::delivery::resolve),
        )
        .route("/api/v1/conversations/{id}/cancel", post(cancel))
        .route(
            "/api/v1/conversations/{id}/permissions/{permission}",
            post(decide_permission),
        )
        .route(
            "/api/v1/conversations/{id}/elicitations/{elicitation}",
            post(decide_elicitation),
        )
        .route("/api/v1/activity", get(activity))
        .route("/api/v1/views", get(views))
        .route("/api/v1/views/{id}", get(view_get).put(view_put))
        .route("/api/v1/events", get(events))
        .route(
            "/api/v1/import-sources",
            get(imports::sources).post(imports::create_source),
        )
        .route(
            "/api/v1/import-sources/{source}",
            axum::routing::delete(imports::revoke_source),
        )
        .route(
            "/api/v1/import-sources/{source}/rotate",
            post(imports::rotate_source),
        )
        .route(
            "/api/v1/imports/{source}/{session_id}",
            axum::routing::put(imports::put).layer(DefaultBodyLimit::max(1024 * 1024)),
        )
        .route("/context/v1/query", post(context::query))
        .route(
            "/agents/{agent}/context/v1/query",
            post(context::agent_query),
        )
        .route(
            "/api/v1/agents/{agent}/context-credential",
            get(context::credential)
                .post(context::issue_credential)
                .delete(context::revoke_credential),
        )
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), headers))
        .with_state(state)
}
async fn headers(
    State(state): State<Arc<AppState>>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    // HTTP/2 carries the origin authority in :authority, rather than Host.
    // Normalize it before cookie-authenticated CSRF checks.
    if !request.headers().contains_key(header::HOST)
        && let Some(authority) = request.uri().authority().map(|a| a.as_str().to_owned())
        && let Ok(value) = authority.parse()
    {
        request.headers_mut().insert(header::HOST, value);
    }
    let companion_page = request.uri().path() == "/companion";
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    let media_policy = if companion_page {
        "img-src 'self' data: blob:; media-src 'self' blob:;"
    } else {
        "img-src 'self' data:;"
    };
    h.insert(header::CONTENT_SECURITY_POLICY,format!("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; {media_policy} object-src 'none'; base-uri 'none'; frame-ancestors 'self' {}; form-action 'self'",state.config.embed_origins.join(" ")).parse().unwrap());
    response
}
async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    let name = state
        .config
        .agent
        .label
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    Html(include_str!("../ui/index.html").replace(
        "<title>Conversations · Your agent, wherever you are</title>",
        &format!("<title>{name}: Conversations</title>"),
    ))
}
async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../ui/app.js"),
    )
}
async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        concat!(
            include_str!("../ui/style.css"),
            "\n",
            include_str!("../ui/markdown.css")
        ),
    )
}
async fn meta(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    let agent = s.agent("default")?.ok_or_else(ApiError::missing)?;
    let mut safe = json!({"label":agent.label});
    if p.admin {
        safe["command"] = json!(agent.command);
        safe["args"] = json!(agent.args);
        safe["cwd"] = json!(agent.cwd);
    }
    Ok(Json(
        json!({"id":s.config.id,"label":s.config.label,"owner":p.actor,"admin":p.admin,"agent":safe,"provider":{"id":s.config.provider_id(),"query_path":"/context/v1/query","surface":"web","access":"audience_scoped"},"public_url":s.config.public_url,"version":env!("CARGO_PKG_VERSION"),"approval_mode":"interactive","features":{"rooms":true,"multiple_agents":true,"attachments":true,"message_search":true},"notice":"Conversations are private until their owner explicitly shares them. Each agent keeps its own working session in each room."}),
    ))
}
async fn people(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "observe", false)?;
    let mut users = s.config.allowed_users.clone();
    users.push(s.config.owner.clone());
    users.sort();
    users.dedup();
    Ok(Json(json!({"items":users})))
}
async fn config_get(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", false)?;
    Ok(Json(json!(s.config)))
}
async fn config_put(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(c): Json<crate::config::Config>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    if c.id != s.config.id || c.owner != s.config.owner {
        return Err(ApiError::bad(
            "The instance ID and owner are installation settings",
        ));
    }
    c.validate().map_err(|e| ApiError::bad(e.to_string()))?;
    c.save(&s.home)?;
    s.audit(
        "configuration.saved",
        None,
        json!({"restart_required":true}),
    )?;
    s.changed();
    Ok(Json(json!({"saved":true,"restart_required":true})))
}
async fn check(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let _permit = s
        .turn_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::busy())?;
    driver::probe(&s.agent("default")?.ok_or_else(ApiError::missing)?).await.map(Json).map_err(|error| {
        if crate::acp_auth::authentication_required(&error) { return ApiError::bad(error.to_string()); }
        ApiError::bad(
            "The agent connection could not be opened. Check its service and connection command.",
        )
    })
}
#[derive(Deserialize, Default)]
struct ListQuery {
    before: Option<String>,
    limit: Option<usize>,
    search: Option<String>,
}
async fn list(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    let search = q.search.unwrap_or_default();
    if search.len() > 200 {
        return Err(ApiError::bad("Search is too long"));
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    let (before_at, before_id) = match q.before {
        Some(c) => {
            let (at, id) = c
                .split_once(':')
                .ok_or_else(|| ApiError::bad("Invalid conversation cursor"))?;
            (
                at.parse::<i64>()
                    .map_err(|_| ApiError::bad("Invalid conversation cursor"))?,
                id.to_string(),
            )
        }
        None => (i64::MAX, String::new()),
    };
    let ids = {
        let db = s.db.lock().unwrap();
        let mut stmt=db.prepare("SELECT c.id FROM conversations c JOIN room_members m ON m.conversation_id=c.id AND m.username=?1 WHERE (c.updated_at<?2 OR (c.updated_at=?2 AND c.id<?3)) AND (?4='' OR instr(lower(c.title),lower(?4))>0 OR EXISTS(SELECT 1 FROM messages x WHERE x.conversation_id=c.id AND instr(lower(x.text),lower(?4))>0)) ORDER BY c.updated_at DESC,c.id DESC LIMIT ?5")?;
        stmt.query_map(
            params![p.actor, before_at, before_id, search, limit as i64 + 1],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let more = ids.len() > limit;
    let items = ids
        .into_iter()
        .take(limit)
        .map(|id| s.conversation_info(&id).map(|c| c.unwrap()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let cursor = if more {
        items.last().map(|c| {
            format!(
                "{}:{}",
                c["updated_at"].as_i64().unwrap_or(0),
                c["id"].as_str().unwrap_or("")
            )
        })
    } else {
        None
    };
    Ok(Json(json!({"items":items,"next_cursor":cursor})))
}
#[derive(Deserialize, Default)]
struct HistoryQuery {
    before: Option<i64>,
    limit: Option<usize>,
    search: Option<String>,
}
async fn message_history(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    s.require_member(&id, &p)?;
    let search = q.search.unwrap_or_default();
    if search.len() > 200 {
        return Err(ApiError::bad("Search is too long"));
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 100);
    let mut items = s.message_page(&id, q.before, limit + 1, &search)?;
    let more = items.len() > limit;
    if more {
        items.remove(0);
    }
    let cursor = if more {
        items.first().and_then(|m| m["seq"].as_i64())
    } else {
        None
    };
    Ok(Json(
        json!({"items":items,"next_cursor":cursor,"search":search}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Title {
    title: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    title: String,
    #[serde(default)]
    agent_ids: Option<Vec<String>>,
}
fn title(value: &str) -> ApiResult<&str> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > 120 {
        Err(ApiError::bad("Use a title between 1 and 120 characters"))
    } else {
        Ok(v)
    }
}
async fn create(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(t): Json<Create>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let p = auth::check(&s, &h, "chat", true)?;
    let agents = t.agent_ids.unwrap_or_else(|| vec!["default".into()]);
    if agents.len() > 8
        || agents
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != agents.len()
    {
        return Err(ApiError::bad("Choose up to eight distinct agents"));
    }
    for id in &agents {
        if s.agent(id)?.is_none() {
            return Err(ApiError::bad("Unknown agent connection"));
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(s.create_room(title(&t.title)?, &p.actor, &agents)?),
    ))
}
async fn read(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    s.require_member(&id, &p)?;
    let mut room = s.conversation(&id)?.ok_or_else(ApiError::missing)?;
    room["permissions"] = json!(s.permissions.list(&id, &p.actor));
    room["elicitations"] = json!(s.elicitations.list(&id, &p.actor));
    Ok(Json(room))
}
async fn rename(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(t): Json<Title>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&id, &p)?;
    if !s.exists(&id)? {
        return Err(ApiError::missing());
    }
    if s.conversation_info(&id)?
        .is_some_and(|c| c["read_only"] == true)
    {
        return Err(ApiError::bad(
            "This conversation belongs to its original experience and is read-only here",
        ));
    }
    s.db.lock().unwrap().execute(
        "UPDATE conversations SET title=?2,updated_at=?3 WHERE id=?1",
        params![id, title(&t.title)?, now_ms()],
    )?;
    s.changed();
    Ok(Json(s.conversation(&id)?.unwrap()))
}
async fn remove(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_owner(&id, &p)?;
    if s.running.lock().unwrap().contains_key(&id) {
        return Err(ApiError::busy());
    }
    if s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM conversations WHERE id=?1", [&id])?
        == 0
    {
        return Err(ApiError::missing());
    }
    s.audit("conversation.removed",Some(&id),json!({"note":"The web transcript and source grants were removed. The agent may retain its own prior working context."}))?;
    s.changed();
    Ok(Json(json!({"deleted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Send {
    pub(crate) text: String,
    pub(crate) request_id: String,
    #[serde(default)]
    pub(crate) target_agent_ids: Vec<String>,
    #[serde(default)]
    pub(crate) attachment_ids: Vec<String>,
}
pub(crate) async fn send(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(msg): Json<Send>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&id, &p)?;
    if !crate::companion::room_identity_valid(&s, &id)? {
        return Err(ApiError::bad(
            "This call's participant or connection changed. Start a new call to the intended agent.",
        ));
    }
    if (msg.text.trim().is_empty() && msg.attachment_ids.is_empty())
        || msg.text.len() > 60_000
        || uuid::Uuid::parse_str(&msg.request_id).is_err()
        || msg.attachment_ids.len() > 4
        || msg
            .attachment_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != msg.attachment_ids.len()
    {
        return Err(ApiError::bad(
            "Send up to 60,000 bytes, four attachments, and a UUID request_id",
        ));
    }
    let room = s.conversation_info(&id)?.ok_or_else(ApiError::missing)?;
    if room["read_only"] == true {
        return Err(ApiError::bad(
            "Continue this imported conversation in its original experience",
        ));
    }
    let participants = s.room_agents(&id)?;
    if msg
        .target_agent_ids
        .iter()
        .any(|id| !participants.iter().any(|a| a["id"] == *id))
        || msg
            .target_agent_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != msg.target_agent_ids.len()
    {
        return Err(ApiError::bad("Address only agents who are in this room"));
    }
    let targets = serde_json::to_string(&msg.target_agent_ids).unwrap();
    let was_working;
    {
        // Intake and worker finalization both take running before the database.
        // The durable row therefore belongs to a live worker or its successor.
        let running = s.running.lock().unwrap();
        was_working = running.contains_key(&id);
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let old:Option<(String,String,String,String)>=tx.query_row("SELECT conversation_id,text,actor,targets FROM messages WHERE id=?1 AND role='user'",[&msg.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((room, text, actor, old_targets)) = old {
            let old_files: Vec<String> = tx
                .prepare("SELECT id FROM attachments WHERE message_id=?1 ORDER BY id")?
                .query_map([&msg.request_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut files = msg.attachment_ids.clone();
            files.sort();
            if room != id
                || text != msg.text
                || actor != p.actor
                || targets != old_targets
                || old_files != files
            {
                return Err(ApiError::bad(
                    "This request_id has already been used for another message",
                ));
            }
            return Ok((
                StatusCode::OK,
                Json(json!({"accepted":true,"duplicate":true,"message_id":msg.request_id})),
            ));
        }
        let member: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_members WHERE conversation_id=?1 AND username=?2)",
            params![id, p.actor],
            |r| r.get(0),
        )?;
        if !member {
            return Err(ApiError::missing());
        }
        let mut bytes = msg.text.len() as i64;
        for attachment in &msg.attachment_ids {
            let size:Option<i64>=tx.query_row("SELECT bytes FROM attachments WHERE id=?1 AND conversation_id=?2 AND created_by=?3 AND message_id IS NULL",params![attachment,id,p.actor],|r|r.get(0)).optional()?;
            bytes += size.ok_or_else(|| {
                ApiError::bad("Use your own unsent attachments from this conversation")
            })?;
        }
        let (count,queued_bytes):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(text AS BLOB))+(SELECT COALESCE(SUM(bytes),0) FROM attachments WHERE message_id=m.id)),0) FROM messages m WHERE m.id IN (SELECT message_id FROM input_deliveries WHERE conversation_id=?1 AND state IN ('received','promptRequired','preparing','offering','submitted','received_remote','remoteQueued','unknown','notAccepted','ack_pending'))",[&id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if count >= 32 || queued_bytes + bytes > 8 * 1024 * 1024 {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS,"This conversation has reached its pending-message capacity. Review delivery status or wait before retrying; this message was not accepted.".into()));
        }
        let agents: Vec<String> = tx
            .prepare("SELECT agent_id FROM room_agents WHERE conversation_id=?1 ORDER BY agent_id")?
            .query_map([&id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if msg.target_agent_ids.iter().any(|a| !agents.contains(a)) {
            return Err(ApiError::bad(
                "The selected agent is no longer in this room",
            ));
        }
        let people: i64 = tx.query_row(
            "SELECT COUNT(*) FROM room_members WHERE conversation_id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let direct = people == 1 && agents.len() == 1;
        let at = now_ms();
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,targets) VALUES(?1,?2,'user',?3,'complete',?4,?4,?5,?6)",params![msg.request_id,id,msg.text,at,p.actor,targets])?;
        let token_hash = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(auth::digest);
        tx.execute(
            "INSERT INTO input_authority(message_id,browser_hash,token_hash) VALUES(?1,?2,?3)",
            params![msg.request_id, p.session_hash, token_hash],
        )?;
        for attachment in &msg.attachment_ids {
            tx.execute(
                "UPDATE attachments SET message_id=?2 WHERE id=?1",
                params![attachment, msg.request_id],
            )?;
        }
        for agent in agents
            .iter()
            .filter(|a| msg.target_agent_ids.is_empty() || msg.target_agent_ids.contains(a))
        {
            let participation = if direct || msg.target_agent_ids.contains(agent) {
                "direct"
            } else {
                "ambient"
            };
            tx.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?1,?2,?3,?4,?5,'received',?6)",params![uuid::Uuid::new_v4().to_string(),msg.request_id,id,agent,participation,at])?;
        }
        tx.execute("UPDATE conversations SET status='working',note=CASE WHEN ?3 THEN note ELSE 'Message received; waiting for an agent…' END,updated_at=?2 WHERE id=?1",params![id,at,was_working])?;
        tx.commit()?;
    }
    s.audit(
        "input.received",
        Some(&id),
        json!({"message_id":msg.request_id,"actor":p.actor,"during_work":was_working}),
    )?;
    crate::delivery::launch(&s, &id);
    s.changed();
    Ok((
        StatusCode::ACCEPTED,
        Json(
            json!({"accepted":true,"message_id":msg.request_id,"delivery":"received","during_work":was_working}),
        ),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionDecision {
    option_id: Option<String>,
}
async fn decide_permission(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, permission)): Path<(String, String)>,
    Json(body): Json<PermissionDecision>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&id, &p)?;
    let decision_kind = s
        .permissions
        .resolve(&id, &p.actor, &permission, body.option_id.as_deref())
        .map_err(|message| ApiError(StatusCode::CONFLICT, message.into()))?;
    s.audit(
        "permission.decided",
        Some(&id),
        json!({"actor":p.actor,"request_id":permission,"decision":decision_kind.unwrap_or_else(||"cancelled".into())}),
    )?;
    s.changed();
    Ok(Json(json!({"accepted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ElicitationDecision {
    action: String,
    content: Option<Value>,
}
async fn decide_elicitation(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((id, request)): Path<(String, String)>,
    Json(body): Json<ElicitationDecision>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&id, &p)?;
    s.elicitations
        .resolve(&id, &p.actor, &request, &body.action, body.content.as_ref())
        .map_err(|message| ApiError(StatusCode::CONFLICT, message.into()))?;
    s.audit(
        "elicitation.decided",
        Some(&id),
        json!({"actor":p.actor,"request_id":request,"action":body.action}),
    )?;
    s.changed();
    Ok(Json(json!({"accepted":true})))
}
async fn cancel(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    s.require_member(&id, &p)?;
    if !s.exists(&id)? {
        return Err(ApiError::missing());
    }
    if let Some(tx) = s.running.lock().unwrap().get(&id) {
        let _ = tx.send(true);
    }
    Ok(Json(json!({"requested":true})))
}
async fn activity(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    let db = s.db.lock().unwrap();
    let mut q=db.prepare("SELECT seq,at,kind,conversation_id,data FROM audit WHERE (conversation_id IN (SELECT conversation_id FROM room_members WHERE username=?1)) OR (conversation_id IS NULL AND ?2) ORDER BY seq DESC LIMIT 100")?;
    let rows=q.query_map(params![p.actor,p.admin],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"conversation_id":r.get::<_,Option<String>>(3)?,"data":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null)})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"items":rows})))
}
fn view_id(id: &str) -> ApiResult<()> {
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(ApiError::bad("Invalid view id"));
    }
    Ok(())
}
fn stored_view(s: &AppState, id: &str, owner: &str) -> ApiResult<Value> {
    let row: Option<(String, i64)> =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT value,revision FROM views WHERE id=?1 AND owner=?2",
                params![id, owner],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
    let (state, revision) = row
        .map(|(v, r)| (serde_json::from_str::<Value>(&v).unwrap_or(Value::Null), r))
        .unwrap_or((
            json!({"selected_conversation_id":null,"panel":"conversation","draft":"","search":""}),
            0,
        ));
    Ok(json!({"id":id,"revision":revision,"state":state}))
}
async fn views(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    let db = s.db.lock().unwrap();
    let mut stmt = db.prepare(
        "SELECT id,value,revision,updated_at FROM views WHERE owner=?1 ORDER BY updated_at DESC LIMIT 100",
    )?;
    let rows=stmt.query_map([&p.actor],|r|Ok(json!({"id":r.get::<_,String>(0)?,"state":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(Value::Null),"revision":r.get::<_,i64>(2)?,"updated_at":r.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(json!({"items":rows})))
}
async fn view_get(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "observe", false)?;
    view_id(&id)?;
    Ok(Json(stored_view(&s, &id, &p.actor)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewWrite {
    expected_revision: i64,
    state: ViewState,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ViewState {
    selected_conversation_id: Option<String>,
    panel: String,
    draft: String,
    #[serde(default)]
    draft_request_id: String,
    #[serde(default)]
    draft_request_sha256: String,
    search: String,
    #[serde(default)]
    dialog: Option<String>,
    #[serde(default)]
    title_draft: String,
    #[serde(default)]
    menu_open: bool,
    #[serde(default)]
    details_open: bool,
    #[serde(default)]
    action: Option<ViewAction>,
    #[serde(default)]
    participants_open: bool,
    #[serde(default)]
    target_agent_ids: Vec<String>,
    #[serde(default)]
    attachment_ids: Vec<String>,
    #[serde(default)]
    history_search: String,
    #[serde(default)]
    history_before: Option<i64>,
    #[serde(default)]
    invite_username: String,
    #[serde(default)]
    invite_agent_id: String,
    #[serde(default)]
    share_history: bool,
    #[serde(default)]
    connection_draft: String,
    #[serde(default)]
    registry_open: bool,
    #[serde(default)]
    permission_details: Option<String>,
    #[serde(default)]
    elicitation_drafts: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    voice_enabled: bool,
    #[serde(default)]
    voice_read_aloud: bool,
    #[serde(default)]
    voice_help_open: bool,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ViewAction {
    id: String,
    kind: String,
    target: String,
    #[serde(default)]
    position: Option<i64>,
}
async fn view_put(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(v): Json<ViewWrite>,
) -> ApiResult<Json<Value>> {
    let p = auth::check(&s, &h, "chat", true)?;
    view_id(&id)?;
    if !["conversation", "connection", "activity"].contains(&v.state.panel.as_str())
        || v.state.draft.len() > 60_000
        || (!v.state.draft_request_id.is_empty()
            && uuid::Uuid::parse_str(&v.state.draft_request_id).is_err())
        || (!v.state.draft_request_sha256.is_empty()
            && (v.state.draft_request_sha256.len() != 64
                || !v
                    .state
                    .draft_request_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())))
        || v.state.search.len() > 200
        || v.state.dialog.as_deref().is_some_and(|d| d != "rename")
        || v.state.title_draft.chars().count() > 120
        || v.state.history_search.len() > 200
        || v.state.target_agent_ids.len() > 8
        || v.state.attachment_ids.len() > 4
        || v.state.invite_username.len() > 100
        || v.state.invite_agent_id.len() > 80
        || v.state
            .permission_details
            .as_deref()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        || v.state.connection_draft.len() > 60_000
        || v.state.elicitation_drafts.len() > 24
        || serde_json::to_vec(&v.state.elicitation_drafts).map_or(true, |v| v.len() > 32768)
        || v.state
            .elicitation_drafts
            .keys()
            .any(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(ApiError::bad("Invalid view state"));
    }
    if !v.state.connection_draft.is_empty() {
        // A partially typed JSON document is fine; explicitly reject credential fields.
        let lower = v.state.connection_draft.to_ascii_lowercase();
        if [
            "\"env\"",
            "\"mcp_servers\"",
            "\"password\"",
            "\"token\"",
            "bearer ",
            "api_key",
        ]
        .iter()
        .any(|name| lower.contains(name))
        {
            return Err(ApiError::bad(
                "Keep credentials in protected connection configuration, not browser view state",
            ));
        }
    }
    if let Some(a) = &v.state.action
        && (a.id.is_empty()
            || a.id.len() > 100
            || !matches!(
                (a.kind.as_str(), a.target.as_str()),
                (
                    "focus",
                    "composer"
                        | "search"
                        | "rename_title"
                        | "history_search"
                        | "invite_username"
                        | "invite_agent_id"
                        | "approval"
                        | "elicitation"
                ) | ("scroll", "messages" | "permissions" | "elicitations")
            )
            || a.position.is_some_and(|p| !(-1..=10_000_000).contains(&p)))
    {
        return Err(ApiError::bad(
            "Use a focus or scroll view action with a unique id",
        ));
    }
    if let Some(cid) = &v.state.selected_conversation_id
        && !s.member(cid, &p.actor)?
    {
        return Err(ApiError::missing());
    }
    {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let other: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM views WHERE id=?1 AND owner<>?2)",
            params![id, p.actor],
            |r| r.get(0),
        )?;
        if other {
            return Err(ApiError::missing());
        }
        let current = tx
            .query_row(
                "SELECT revision FROM views WHERE id=?1 AND owner=?2",
                params![id, p.actor],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        if current != v.expected_revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "The view changed. Read it again before updating.".into(),
            ));
        }
        tx.execute("INSERT INTO views(id,value,revision,updated_at,owner) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET value=excluded.value,revision=excluded.revision,updated_at=excluded.updated_at",params![id,serde_json::to_string(&v.state).unwrap(),current+1,now_ms(),p.actor])?;
        tx.commit()?;
    }
    s.changed();
    Ok(Json(stored_view(&s, &id, &p.actor)?))
}
async fn events(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>> {
    let principal = auth::check(&s, &h, "observe", false)?;
    let rx = s.changes.subscribe();
    let stream = stream::unfold(
        (rx, s, h, principal, false),
        |(mut rx, s, h, p, first)| async move {
            if first
                && tokio::time::timeout(Duration::from_secs(20), rx.changed())
                    .await
                    .is_ok_and(|r| r.is_err())
            {
                return None;
            }
            // Streaming access expires and revokes just like ordinary reads.
            if auth::check(&s, &h, "observe", false).is_err() {
                return None;
            }
            let revision = *rx.borrow_and_update();
            Some((
                Ok(Event::default()
                    .event("change")
                    .json_data(json!({"revision":revision}))
                    .unwrap()),
                (rx, s, h, p, true),
            ))
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
pub async fn serve(home: std::path::PathBuf) -> anyhow::Result<()> {
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("service.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("This conversation service is already running"))?;
    let state = AppState::open(home)?;
    let listener = std::net::TcpListener::bind((state.config.bind.as_str(), state.config.port))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &state.config.tls_cert,
        &state.config.tls_key,
    )
    .await?;
    crate::config::private_write(
        &state.home.join("runtime.json"),
        &serde_json::to_vec_pretty(
            &json!({"port":port,"url":format!("https://127.0.0.1:{port}"),"public_url":state.config.public_url,"provider_id":state.config.provider_id()}),
        )?,
    )?;
    println!("Agent Conversations is listening on HTTPS port {port}");
    if !state.config.public_url.is_empty() {
        println!("Open {}", state.config.public_url);
    }
    axum_server::from_tcp_rustls(listener, tls)?
        .serve(router(state).into_make_service())
        .await?;
    Ok(())
}
