//! Optional, installation-owned tools for a private Companion call. MCP framing
//! belongs to the official SDK in the per-call bridge. A tool never receives a
//! platform credential or chooses a host command. Unknown submissions stay unknown.
use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use anyhow::{Result, ensure};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "companion_huddle_tools.rs"]
pub mod huddles;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    enabled: bool,
    #[serde(default)]
    agents: BTreeMap<String, Vec<Connection>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    id: String,
    label: String,
    description: String,
    command: Vec<String>,
}
fn config(s: &AppState) -> Result<Config> {
    let path = s.home.join("companion-actions.json");
    if !path.exists() {
        return Ok(Config::default());
    }
    let data = std::fs::read(path)?;
    ensure!(data.len() <= 65536, "Action configuration too large");
    let c: Config = serde_json::from_slice(&data)?;
    ensure!(c.agents.len() <= 100, "Too many agent tool profiles");
    for connections in c.agents.values() {
        ensure!(connections.len() <= 16, "Too many connections");
        let mut ids = std::collections::HashSet::new();
        for item in connections {
            ensure!(
                !item.id.is_empty() && item.id.len() <= 120 && ids.insert(&item.id),
                "Invalid connection identity"
            );
            ensure!(
                item.label.len() <= 160 && item.description.len() <= 2000,
                "Invalid description"
            );
            ensure!(
                !item.command.is_empty()
                    && item.command.len() <= 20
                    && std::path::Path::new(&item.command[0]).is_absolute()
                    && item
                        .command
                        .iter()
                        .all(|x| x.len() <= 4096 && !x.contains('\0')),
                "Install a fixed action helper command"
            );
        }
    }
    Ok(c)
}
pub fn migrate(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS companion_tool_grants(call_id TEXT PRIMARY KEY REFERENCES companion_calls(id) ON DELETE CASCADE,token TEXT NOT NULL,port INTEGER); CREATE TABLE IF NOT EXISTS companion_actions(call_id TEXT NOT NULL REFERENCES companion_calls(id) ON DELETE CASCADE,request_id TEXT NOT NULL,delegation_id TEXT NOT NULL,connection_id TEXT NOT NULL,instruction TEXT NOT NULL,source TEXT NOT NULL,state TEXT NOT NULL,result TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(call_id,request_id),UNIQUE(call_id,delegation_id,connection_id));")?;
    db.execute("UPDATE companion_actions SET state='unknown',result=?1 WHERE state='submitting'", [json!({"state":"unknown","message":"The host stopped before a confirmed receipt. Do not resubmit automatically."}).to_string()])?;
    Ok(())
}
pub fn create_mcp_config(s: &AppState, id: &str) -> Result<Value> {
    if !config(s)?.enabled {
        return Ok(Value::Null);
    }
    let token = auth::random_token();
    s.db.lock().unwrap().execute(
        "INSERT INTO companion_tool_grants(call_id,token) VALUES(?1,?2)",
        params![id, token],
    )?;
    Ok(json!({"api_base":s.config.public_url,"call_id":id,"token":token}))
}
pub fn record_mcp_port(s: &AppState, id: &str, port: u16) -> Result<()> {
    ensure!(port > 0, "Invalid loopback tool listener");
    s.db.lock().unwrap().execute(
        "UPDATE companion_tool_grants SET port=?2 WHERE call_id=?1",
        params![id, port],
    )?;
    Ok(())
}
pub fn descriptor(s: &AppState, room: &str) -> Result<Option<Value>> {
    let value: Option<(String,String)> = s.db.lock().unwrap().query_row("SELECT c.id,g.token FROM companion_calls c JOIN companion_tool_grants g ON c.id=g.call_id WHERE c.conversation_id=?1 AND c.state IN ('connecting','connected') AND g.port IS NOT NULL",[room],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    Ok(value.map(|(id,token)|json!({"type":"http","name":"companion","url":format!("{}/api/v1/companion/calls/{id}/mcp",s.config.public_url.trim_end_matches('/')),"headers":[{"name":"Authorization","value":format!("Bearer {token}")}]})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeMcpPermission {
    server: String,
    tool: String,
    arguments: Value,
}
fn own_tool_shape_valid(tool: &str, args: &Value) -> bool {
    match tool {
        "companion_huddles"
        | "companion_huddle_ensure"
        | "companion_huddle_read"
        | "companion_huddle_post"
        | "companion_huddle_status" => huddles::permission_shape_valid(tool, args),
        "companion_view"
        | "companion_capture"
        | "companion_surface_status"
        | "companion_hangup"
        | "companion_feedback" => crate::companion_surface::permission_shape_valid(tool, args),
        "companion_connections" => args.as_object().is_some_and(|a| a.is_empty()),
        "companion_observations" => serde_json::from_value::<Observations>(args.clone())
            .is_ok_and(|v| v.connection_id.as_deref().is_none_or(valid_connection_id)),
        "companion_connection_status" => serde_json::from_value::<ConnectionStatus>(args.clone())
            .is_ok_and(|v| valid_connection_id(&v.connection_id)),
        "companion_shared" => args.as_object().is_some_and(|a| {
            a.is_empty()
                || (a.len() == 1
                    && a.get("awareness_id")
                        .and_then(Value::as_str)
                        .is_some_and(valid_awareness_id))
        }),
        "companion_action_status" => args.as_object().is_some_and(|a| {
            a.len() == 1
                && a.get("request_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        }),
        "companion_show" => {
            serde_json::from_value::<Show>(args.clone()).is_ok_and(|v| valid_show(&v))
        }
        "companion_request_action" => {
            serde_json::from_value::<Action>(args.clone()).is_ok_and(|v| valid_action(&v))
        }
        _ => false,
    }
}
/// A single native ACP permission may use the installation's existing call
/// authorization. This is not a general MCP, command, or filesystem policy.
/// The driver must have injected this call's own reserved `companion` server.
pub fn permission_once(
    s: &AppState,
    room: &str,
    actor: &str,
    agent: &str,
    descriptor_injected: bool,
    request: &Value,
) -> Result<Option<Value>> {
    if !descriptor_injected || request.to_string().len() > 65536 {
        return Ok(None);
    }
    let cfg = config(s).ok();
    // A disconnected listener does not erase this injected server's ownership.
    // Recognize its retained grant before deciding whether it can still run.
    let call:Option<(String,String,Option<i64>)>=s.db.lock().unwrap().query_row("SELECT c.id,c.state,g.port FROM companion_calls c JOIN companion_tool_grants g ON g.call_id=c.id WHERE c.conversation_id=?1 AND c.owner=?2 AND c.agent_id=?3",params![room,actor,agent],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((call, state, port)) = call else {
        return Ok(None);
    };
    if !crate::companion::room_identity_valid(s, room)?
        || s.agent(agent)?
            .is_none_or(|a| a.mcp_servers.iter().any(|m| m["name"] == "companion"))
    {
        return Ok(None);
    }
    let tool_call = &request["toolCall"];
    if tool_call["kind"] != "other"
        || tool_call["status"] != "pending"
        || tool_call["toolCallId"]
            .as_str()
            .is_none_or(|v| v.is_empty() || v.len() > 256)
    {
        return Ok(None);
    }
    let raw: NativeMcpPermission = match serde_json::from_value(tool_call["rawInput"].clone()) {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    if raw.server != "companion" || !raw.arguments.is_object() {
        return Ok(None);
    }
    // Native MCP titles are derived from the actual server and tool. Treat the
    // title only as a consistency check: built-ins carry model-authored raw
    // arguments, which could otherwise imitate the strict MCP wrapper.
    let expected_title = format!("companion / {}", raw.tool);
    if tool_call["title"].as_str() != Some(expected_title.as_str()) {
        return Ok(None);
    }
    if !own_tool_shape_valid(&raw.tool, &raw.arguments) {
        return Ok(None);
    }
    let Some(options) = request["options"].as_array() else {
        return Ok(None);
    };
    if options.is_empty()
        || options.len() > 16
        || options.iter().any(|o| {
            o["optionId"]
                .as_str()
                .is_none_or(|id| id.is_empty() || id.len() > 256)
                || o["name"]
                    .as_str()
                    .is_none_or(|name| name.is_empty() || name.len() > 512)
                || !["allow_once", "allow_always", "reject_once", "reject_always"]
                    .contains(&o["kind"].as_str().unwrap_or(""))
        })
        || options
            .iter()
            .map(|o| o["optionId"].as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
            != options.len()
    {
        return Ok(None);
    }
    let deny = |reason: &str| -> Result<Option<Value>> {
        // The descriptor was injected for this exact private call, but the
        // call's authority has ended. Do not leave a late native approval for
        // a browser that is no longer present; never extend effect authority.
        let rejects = options
            .iter()
            .filter(|o| o["kind"] == "reject_once")
            .collect::<Vec<_>>();
        let automatic_end = matches!(reason, "call_inactive" | "transport_unavailable");
        let outcome = if rejects.len() == 1 && !automatic_end {
            json!({"outcome":{"outcome":"selected","optionId":rejects[0]["optionId"]}})
        } else {
            crate::permissions::cancelled()
        };
        s.audit("permission.companion.denied", Some(room), json!({"agent_id":agent,"actor":actor,"call_id":call,"tool":raw.tool,"reason":reason,"kind":if rejects.len() == 1 && !automatic_end {"reject_once"} else {"cancelled"},"human_decision":false}))?;
        Ok(Some(outcome))
    };
    if port.is_none_or(|port| port <= 0) {
        return deny("transport_unavailable");
    }
    let inactive = !matches!(state.as_str(), "connecting" | "connected");
    let saved_receipt = (raw.tool == "companion_surface_status"
        && crate::companion_surface::terminal_status_shape_valid(&raw.arguments))
        || raw.tool == "companion_huddle_status";
    if (inactive && !saved_receipt) || cfg.as_ref().is_none_or(|cfg| !cfg.enabled) {
        return deny(if inactive {
            "call_inactive"
        } else {
            "tools_disabled"
        });
    }
    let cfg = cfg.unwrap();
    let permitted = match raw.tool.as_str() {
        "companion_huddles"
        | "companion_huddle_ensure"
        | "companion_huddle_read"
        | "companion_huddle_post"
        | "companion_huddle_status" => {
            huddles::permission_valid(s, &call, actor, agent, room, &raw.tool, &raw.arguments)?
        }
        "companion_view"
        | "companion_capture"
        | "companion_surface_status"
        | "companion_hangup"
        | "companion_feedback" => crate::companion_surface::permission_valid(
            s,
            &call,
            actor,
            agent,
            room,
            &raw.tool,
            raw.arguments,
        )?,
        "companion_connections"
        | "companion_observations"
        | "companion_connection_status"
        | "companion_shared"
        | "companion_action_status"
        | "companion_show" => true,
        "companion_request_action" => {
            let action = match serde_json::from_value::<Action>(raw.arguments) {
                Ok(a) if valid_action(&a) => a,
                _ => return Ok(None),
            };
            if !cfg
                .agents
                .get(agent)
                .is_some_and(|routes| routes.iter().any(|route| route.id == action.connection_id))
            {
                return deny("connection_not_configured");
            }
            let source:Option<String>=s.db.lock().unwrap().query_row("SELECT d.message_id FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id=?2 AND d.state='dispatched' AND m.role='user' AND m.actor=?3 AND m.conversation_id=?4 AND EXISTS(SELECT 1 FROM input_deliveries i WHERE i.message_id=m.id AND i.state IN ('submitted','received_remote','incorporated'))",params![call,action.delegation_id,actor,room],|r|r.get(0)).optional()?;
            match source {
                Some(source) => crate::delivery::authorized(s, &source, actor)?,
                None => false,
            }
        }
        _ => false,
    };
    if !permitted {
        return deny("call_scope_not_authorized");
    }
    let once = options
        .iter()
        .filter(|o| o["kind"] == "allow_once")
        .collect::<Vec<_>>();
    if once.len() != 1 {
        return Ok(None);
    }
    s.audit(
        "permission.companion.allowed",
        Some(room),
        json!({"agent_id":agent,"actor":actor,"call_id":call,"tool":raw.tool,"kind":"allow_once"}),
    )?;
    Ok(Some(
        json!({"outcome":{"outcome":"selected","optionId":once[0]["optionId"]}}),
    ))
}
pub(crate) fn enabled(s: &AppState) -> Result<bool> {
    Ok(config(s)?.enabled)
}
pub(crate) fn authorize(
    s: &AppState,
    id: &str,
    h: &HeaderMap,
) -> ApiResult<(String, String, String)> {
    authorize_scope(s, id, h, false)
}
/// Saved receipts retain their exact call credential and endpoint identity after
/// media ends. This does not authorize live observations or new effects.
pub(crate) fn authorize_receipt(
    s: &AppState,
    id: &str,
    h: &HeaderMap,
) -> ApiResult<(String, String, String)> {
    authorize_scope(s, id, h, true)
}
fn authorize_scope(
    s: &AppState,
    id: &str,
    h: &HeaderMap,
    saved_receipt: bool,
) -> ApiResult<(String, String, String)> {
    if !config(s)?.enabled {
        return Err(ApiError::forbidden());
    }
    if h.contains_key("origin") {
        return Err(ApiError::forbidden());
    }
    let supplied = h
        .get("authorization")
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?;
    let row: Option<(String,String,String,String)> = s.db.lock().unwrap().query_row("SELECT c.owner,c.agent_id,c.conversation_id,g.token FROM companion_calls c JOIN companion_tool_grants g ON c.id=g.call_id WHERE c.id=?1 AND (?2 OR c.state IN ('connecting','connected'))",params![id,saved_receipt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (owner, agent, room, token) = row.ok_or_else(ApiError::missing)?;
    if auth::digest(supplied) != auth::digest(&token)
        || !crate::companion::room_identity_valid(s, &room)?
    {
        return Err(ApiError::forbidden());
    }
    Ok((owner, agent, room))
}
pub async fn mcp(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    authorize_receipt(&s, &id, &h)?;
    if body.len() > 256 * 1024 {
        return Err(ApiError::bad("Tool request too large"));
    }
    // Saved images have the same 2 MiB decoded budget as ordinary uploads.
    // Only the existing, schema-checked surface read receives room for base64
    // and its receipt; all other tool responses keep the smaller budget.
    let image_read = serde_json::from_slice::<Value>(&body).is_ok_and(|value| {
        value["method"] == "tools/call"
            && value["params"]["name"] == "companion_surface_status"
            && crate::companion_surface::permission_shape_valid(
                "companion_surface_status",
                value["params"].get("arguments").unwrap_or(&json!({})),
            )
    });
    let response_limit = if image_read {
        3 * 1024 * 1024
    } else {
        1024 * 1024
    };
    let active: bool = s.db.lock().unwrap().query_row(
        "SELECT state IN ('connecting','connected') FROM companion_calls WHERE id=?1",
        [&id],
        |r| r.get(0),
    )?;
    if !active {
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| ApiError::bad("Invalid tool request"))?;
        let allowed = match value["method"].as_str() {
            Some("initialize" | "notifications/initialized" | "tools/list" | "ping") => true,
            Some("tools/call") => {
                (value["params"]["name"] == "companion_surface_status"
                    && crate::companion_surface::terminal_status_shape_valid(
                        &value["params"]
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    ))
                    || (value["params"]["name"] == "companion_huddle_status"
                        && huddles::permission_shape_valid(
                            "companion_huddle_status",
                            &value["params"]["arguments"],
                        ))
            }
            _ => false,
        };
        if !allowed {
            return Err(ApiError::forbidden());
        }
    }
    let port: Option<u16> = s.db.lock().unwrap().query_row(
        "SELECT port FROM companion_tool_grants WHERE call_id=?1",
        [&id],
        |r| r.get(0),
    )?;
    let port = port.ok_or_else(ApiError::missing)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .build()
        .map_err(|_| ApiError::bad("Tool transport unavailable"))?;
    let mut req = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("authorization", h.get("authorization").unwrap().clone());
    if let Some(v) = h.get("mcp-protocol-version") {
        req = req.header("mcp-protocol-version", v);
    }
    let mut r = req.body(body).send().await.map_err(|_| {
        ApiError::bad(
            "Tool connection interrupted; inspect the original request receipt before retrying",
        )
    })?;
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut data = Vec::new();
    while let Some(chunk) = r
        .chunk()
        .await
        .map_err(|_| ApiError::bad("Tool response interrupted"))?
    {
        if data.len() + chunk.len() > response_limit {
            return Err(ApiError::bad("Tool response too large"));
        }
        data.extend_from_slice(&chunk);
    }
    Ok((status, [("content-type", "application/json")], data).into_response())
}
/// Routing metadata for this call's installed tools, never a source observation.
/// The same filtered rows are returned by discovery; no helper is invoked here.
pub(crate) fn read_catalog(s: &AppState, id: &str) -> Result<Option<Value>> {
    if !config(s)?.enabled {
        return Ok(None);
    }
    let installed: bool = s.db.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_calls c JOIN companion_tool_grants g ON g.call_id=c.id WHERE c.id=?1 AND c.state IN ('connecting','connected') AND g.port IS NOT NULL)",
        [id],
        |r| r.get(0),
    )?;
    if !installed {
        return Ok(None);
    }
    let sources = crate::companion_awareness::status_sources(s, id)?;
    let coordination = huddles::catalog(s, id)?;
    if sources.is_empty() && coordination.is_none() {
        return Ok(None);
    }
    let mut catalog = json!({
        "observed_at":now_ms(),
        "observation_sources":sources
    });
    if let Some(coordination) = coordination {
        catalog["huddles"] = coordination;
    }
    Ok(Some(catalog))
}

pub async fn connections(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> ApiResult<Json<Value>> {
    let (_, agent, _) = authorize(&s, &id, &h)?;
    let c = config(&s)?;
    Ok(Json(
        json!({"connections":c.agents.get(&agent).into_iter().flatten().map(|x|json!({"id":x.id,"label":x.label,"description":x.description})).collect::<Vec<_>>(),"observation_sources":crate::companion_awareness::status_sources(&s,&id)?,"policy":"Configured observation sources support a fresh read with companion_connection_status. Configuration alone does not prove availability. Actions require the caller's explicit request. Submission is not completion. Unknown sends must not be retried."}),
    ))
}
/// Saved connection observations require no new platform action or native MCP
/// permission. The observer independently enforces current owner/audience policy.
pub async fn observations(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Query(query): Query<Observations>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &id, &h)?;
    if !query
        .connection_id
        .as_deref()
        .is_none_or(valid_connection_id)
    {
        return Err(ApiError::bad("Choose a configured observation source"));
    }
    Ok(Json(crate::companion_awareness::current_observations(
        &s,
        &id,
        query.connection_id.as_deref(),
    )?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    #[serde(default, deserialize_with = "optional_connection_id")]
    connection_id: Option<String>,
}
// An omitted selector means all authorized sources. Explicit null is not a
// selector and must not silently broaden a scoped read through native permission.
fn optional_connection_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionStatus {
    connection_id: String,
    #[serde(default)]
    view: crate::companion_awareness::StatusView,
}
fn valid_connection_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 120 && !id.chars().any(char::is_control)
}
pub async fn connection_status(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Query(query): Query<ConnectionStatus>,
) -> ApiResult<Json<Value>> {
    authorize(&s, &id, &h)?;
    if !valid_connection_id(&query.connection_id) {
        return Err(ApiError::bad("Choose a configured observation source"));
    }
    let read =
        crate::companion_awareness::connection_status(&s, &id, &query.connection_id, query.view)
            .await?;
    authorize(&s, &id, &h)?;
    // Preserve original source qualifiers before a backend can paraphrase them.
    // This queues context only; the ordinary floor and policy checks still apply.
    crate::companion_awareness::preserve_requested_status(&s, &id, &read)?;
    Ok(Json(read.response))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    connection_id: String,
    instruction: String,
    request_id: String,
    delegation_id: String,
}
fn valid_action(a: &Action) -> bool {
    uuid::Uuid::parse_str(&a.request_id).is_ok()
        && !a.instruction.trim().is_empty()
        && a.instruction.len() <= 8000
        && !a.delegation_id.is_empty()
        && a.delegation_id.len() <= 160
}
fn action_row(s: &AppState, id: &str, rid: &str) -> Result<Option<Value>> {
    let db = s.db.lock().unwrap();
    // Helpers have a 30-second deadline. A disconnected HTTP task may disappear
    // before recording that outcome; expiry establishes uncertainty, not failure.
    // A later matching receipt may still replace this provisional unknown state.
    db.execute("UPDATE companion_actions SET state='unknown',result=?3 WHERE call_id=?1 AND request_id=?2 AND state='submitting' AND created_at<?4",params![id,rid,json!({"request_id":rid,"state":"unknown","diagnostic":"submission_receipt_expired","message":"No terminal receipt was observed within the helper deadline. The effect may have occurred; do not retry automatically."}).to_string(),now_ms()-45_000])?;
    let mut value = db.query_row("SELECT request_id,connection_id,state,result,created_at FROM companion_actions WHERE call_id=?1 AND request_id=?2",params![id,rid],|r|{let result:String=r.get(3)?;Ok(json!({"request_id":r.get::<_,String>(0)?,"connection_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"result":serde_json::from_str::<Value>(&result).unwrap_or(Value::Null),"created_at":r.get::<_,i64>(4)?}))}).optional()?;
    drop(db);
    if let Some(row) = value.as_mut() {
        row["observation"] =
            crate::companion_awareness::action_observation(s, id, rid)?.unwrap_or(Value::Null);
    }
    Ok(value)
}
pub async fn status(
    State(s): State<Arc<AppState>>,
    Path((id, rid)): Path<(String, String)>,
    h: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&s, &id, &h)?;
    Ok(Json(match action_row(&s, &id, &rid)? {
        Some(value) => value,
        None => crate::companion_awareness::action_observation_for_call(&s, &id, &rid)?
            .ok_or_else(ApiError::missing)?,
    }))
}
pub async fn action(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(a): Json<Action>,
) -> ApiResult<Json<Value>> {
    let (owner, agent, room) = authorize(&s, &id, &h)?;
    if !valid_action(&a) {
        return Err(ApiError::bad(
            "Use a UUID request_id, the exact delegation_id and a bounded explicit instruction",
        ));
    }
    let c = config(&s)?;
    let conn = c
        .agents
        .get(&agent)
        .and_then(|xs| xs.iter().find(|x| x.id == a.connection_id))
        .ok_or_else(ApiError::forbidden)?;
    let authority_source: Option<String> =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT message_id FROM companion_delegations WHERE call_id=?1 AND id=?2",
                params![id, a.delegation_id],
                |r| r.get(0),
            )
            .optional()?;
    let authority_source =
        authority_source.ok_or_else(|| ApiError::bad("Use this call's exact delegation ID"))?;
    if !crate::delivery::authorized(&s, &authority_source, &owner)? {
        return Err(ApiError::forbidden());
    }
    let source = {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let previous:Option<(String,String,String)>=tx.query_row("SELECT connection_id,instruction,delegation_id FROM companion_actions WHERE call_id=?1 AND request_id=?2",params![id,a.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((connection, instruction, delegation)) = previous {
            if connection != a.connection_id
                || instruction != a.instruction
                || delegation != a.delegation_id
            {
                return Err(ApiError::busy());
            }
            drop(tx);
            drop(db);
            return Ok(Json(action_row(&s, &id, &a.request_id)?.unwrap()));
        }
        let source:Option<(String,String,String,i64)>=tx.query_row("SELECT d.id,d.message_id,m.text,d.created_at FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id=?4 AND d.state='dispatched' AND EXISTS(SELECT 1 FROM companion_calls c WHERE c.id=d.call_id AND c.state IN ('connecting','connected')) AND m.role='user' AND m.actor=?2 AND m.conversation_id=?3 AND EXISTS(SELECT 1 FROM input_deliveries i WHERE i.message_id=m.id AND i.state IN ('submitted','received_remote','incorporated')) LIMIT 1",params![id,owner,room,a.delegation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let (did, mid, text, at) =
            source.ok_or_else(|| ApiError::bad("An active caller request is required"))?;
        let existing:Option<(String,String)>=tx.query_row("SELECT request_id,instruction FROM companion_actions WHERE call_id=?1 AND delegation_id=?2 AND connection_id=?3",params![id,did,a.connection_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((rid, instruction)) = existing {
            if instruction != a.instruction {
                return Err(ApiError::busy());
            }
            drop(tx);
            drop(db);
            return Ok(Json(action_row(&s, &id, &rid)?.unwrap()));
        }
        let original_text: Option<String> = tx.query_row(
            "SELECT request_text FROM companion_delegations WHERE call_id=?1 AND id=?2",
            params![id, did],
            |r| r.get(0),
        )?;
        let original_text = original_text
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| text.clone());
        let original_at = crate::companion_awareness::original_human_source_time(&tx, &id, &did)
            .map_err(|_| ApiError::bad("The original human source cannot be verified"))?;
        let time_basis = if original_at.is_some() {
            "retained_human_transcript"
        } else {
            "delegation_created_at"
        };
        let source = json!({"call_id":id,"conversation_id":room,"actor":owner,"audience":"private","delegation_id":did,"intent_id":did,"message_id":mid,"occurred_at":original_at.unwrap_or(at),"time_basis":time_basis,"original_text":original_text,"source_revision":auth::digest(&original_text),"evidence_revision":auth::digest(&text)});
        tx.execute("INSERT INTO companion_actions(call_id,request_id,delegation_id,connection_id,instruction,source,state,result,created_at) VALUES(?1,?2,?3,?4,?5,?6,'submitting','{}',?7)",params![id,a.request_id,did,a.connection_id,a.instruction,source.to_string(),now_ms()])?;
        crate::companion_awareness::bind_action_observer(
            &s,
            &tx,
            &id,
            &a.request_id,
            &a.connection_id,
        )?;
        tx.commit()?;
        source
    };
    let input =
        json!({"version":1,"request_id":a.request_id,"instruction":a.instruction,"source":source});
    let receipt = invoke(&conn.command, &input).await;
    let result = match receipt {
        Ok(value)
            if value["request_id"] == a.request_id
                && value["state"].as_str().is_some_and(|state| {
                    matches!(
                        state,
                        "queued" | "accepted" | "completed" | "failed" | "unknown"
                    )
                }) =>
        {
            value
        }
        Ok(_) => {
            json!({"request_id":a.request_id,"state":"unknown","diagnostic":"invalid_receipt_identity_or_state","message":"The helper receipt did not match this request or a supported state. No trustworthy completion is confirmed. Do not retry automatically."})
        }
        Err(_) => {
            json!({"request_id":a.request_id,"state":"unknown","diagnostic":"helper_receipt_unavailable","message":"No trustworthy terminal receipt was received. Do not retry automatically."})
        }
    };
    let state = result["state"].as_str().unwrap_or("unknown");
    s.db.lock().unwrap().execute(
        "UPDATE companion_actions SET state=?3,result=?4 WHERE call_id=?1 AND request_id=?2",
        params![id, a.request_id, state, result.to_string()],
    )?;
    s.audit("companion.action",Some(&room),json!({"request_id":a.request_id,"connection_id":a.connection_id,"state":state,"actor":owner}))?;
    Ok(Json(action_row(&s, &id, &a.request_id)?.unwrap()))
}
async fn invoke(command: &[String], input: &Value) -> Result<Value> {
    let mut child = tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.to_string().as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(65537)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(bytes.len() <= 65536, "Action receipt exceeded limit");
        ensure!(
            child.wait().await?.success(),
            "Action helper did not settle"
        );
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Show {
    url: Option<String>,
    text: Option<String>,
    title: String,
    #[serde(default)]
    kind: Option<String>,
}
fn valid_show(v: &Show) -> bool {
    !v.title.trim().is_empty()
        && v.title.len() <= 300
        && !v.title.contains(['\n', '\r'])
        && match (&v.url, &v.text) {
            (Some(value), None) => {
                value.len() <= 4000
                    && reqwest::Url::parse(value).is_ok_and(|url| {
                        matches!(url.scheme(), "http" | "https")
                            && url.username().is_empty()
                            && url.password().is_none()
                    })
                    && v.kind
                        .as_deref()
                        .is_none_or(|kind| matches!(kind, "link" | "image" | "video" | "auto"))
            }
            (None, Some(text)) => {
                !text.trim().is_empty()
                    && text.len() <= 1200
                    && v.kind.as_deref().is_none_or(|kind| kind == "note")
            }
            _ => false,
        }
}

pub async fn show(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(v): Json<Show>,
) -> ApiResult<Json<Value>> {
    let (_, _, room) = authorize(&s, &id, &h)?;
    if !valid_show(&v) {
        return Err(ApiError::bad("Unsupported shared artifact"));
    }
    let eid = uuid::Uuid::new_v4().to_string();
    let note = v.text.is_some();
    let data = json!({"title":v.title,"text":v.text.or_else(||v.url.clone()),"url":v.url,"kind":v.kind.unwrap_or_else(||if note {"note".into()} else {"auto".into()}),"conversation_id":room,"presentation":"current"});
    let count: i64 = s.db.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='card'",
        [&id],
        |r| r.get(0),
    )?;
    if count >= 128 {
        return Err(ApiError::bad("Shared space reached its artifact limit"));
    }
    s.db.lock().unwrap().execute(
        "INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'card',?3,?4)",
        params![id, eid, data.to_string(), now_ms()],
    )?;
    s.changed();
    Ok(Json(
        json!({"state":"saved","presentation":"requested_current","event_id":eid,"card_id":s.db.lock().unwrap().query_row("SELECT seq FROM companion_events WHERE call_id=?1 AND event_key=?2",params![id,eid],|r|r.get::<_,i64>(0))?.to_string(),"note":"Saved in this call and offered to the limited whiteboard. Pinned or currently read items can keep it in history; companion_surface_status reports current board IDs. Saved does not prove displayed, opened, played or watched."}),
    ))
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SharedQuery {
    awareness_id: Option<String>,
}
fn valid_awareness_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub async fn shared(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
    Query(query): Query<SharedQuery>,
) -> ApiResult<Json<Value>> {
    let (_, _, room) = authorize(&s, &id, &h)?;
    let db = s.db.lock().unwrap();
    if let Some(awareness_id) = query.awareness_id {
        if !valid_awareness_id(&awareness_id) {
            return Err(ApiError::bad("Invalid awareness identity"));
        }
        let card: Option<(String, i64)> = db.query_row(
            "SELECT data,seq FROM companion_events WHERE call_id=?1 AND kind='card' AND event_key=?2",
            params![id, format!("awareness-card:{awareness_id}")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let (data, seq) = card.ok_or_else(ApiError::missing)?;
        let mut value: Value = serde_json::from_str(&data).map_err(anyhow::Error::from)?;
        if value["awareness_id"] != awareness_id {
            return Err(ApiError::missing());
        }
        value["card_id"] = json!(seq.to_string());
        return Ok(Json(
            json!({"items":[],"shown":[value],"note":"Exact saved source fact for this call; availability is not independent verification or action authority."}),
        ));
    }
    let items=db.prepare("SELECT id,name,mime_type,bytes FROM attachments WHERE conversation_id=?1 ORDER BY created_at DESC LIMIT 16")?.query_map([&room],|r|Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"mime_type":r.get::<_,String>(2)?,"bytes":r.get::<_,i64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let shown=db.prepare("SELECT data,seq FROM companion_events WHERE call_id=?1 AND kind='card' ORDER BY seq DESC LIMIT 16")?.query_map([&id],|r|{let data:String=r.get(0)?;let mut value=serde_json::from_str::<Value>(&data).unwrap_or(Value::Null);value["card_id"]=json!(r.get::<_,i64>(1)?.to_string());Ok(value)})?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(
        json!({"items":items,"shown":shown,"note":"Availability is not image understanding; ask through the existing attachment path."}),
    ))
}
