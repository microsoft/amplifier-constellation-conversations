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
    let cfg = match config(s) {
        Ok(cfg) if cfg.enabled => cfg,
        _ => return Ok(None),
    };
    let call:Option<String>=s.db.lock().unwrap().query_row("SELECT c.id FROM companion_calls c JOIN companion_tool_grants g ON g.call_id=c.id WHERE c.conversation_id=?1 AND c.owner=?2 AND c.agent_id=?3 AND c.state IN ('connecting','connected') AND g.port>0",params![room,actor,agent],|r|r.get(0)).optional()?;
    let Some(call) = call else { return Ok(None) };
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
    let permitted = match raw.tool.as_str() {
        "companion_view" | "companion_capture" | "companion_surface_status" => {
            crate::companion_surface::permission_valid(
                s,
                &call,
                actor,
                agent,
                room,
                &raw.tool,
                raw.arguments,
            )?
        }
        "companion_connections" => raw.arguments.as_object().is_some_and(|a| a.is_empty()),
        "companion_shared" => raw.arguments.as_object().is_some_and(|a| {
            a.is_empty()
                || (a.len() == 1
                    && a.get("awareness_id")
                        .and_then(Value::as_str)
                        .is_some_and(valid_awareness_id))
        }),
        "companion_action_status" => raw.arguments.as_object().is_some_and(|a| {
            a.len() == 1
                && a.get("request_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        }),
        "companion_show" => {
            serde_json::from_value::<Show>(raw.arguments).is_ok_and(|v| valid_show(&v))
        }
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
                return Ok(None);
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
    let row: Option<(String,String,String,String)> = s.db.lock().unwrap().query_row("SELECT c.owner,c.agent_id,c.conversation_id,g.token FROM companion_calls c JOIN companion_tool_grants g ON c.id=g.call_id WHERE c.id=?1 AND c.state IN ('connecting','connected')",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
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
    authorize(&s, &id, &h)?;
    if body.len() > 256 * 1024 {
        return Err(ApiError::bad("Tool request too large"));
    }
    let port: u16 = s.db.lock().unwrap().query_row(
        "SELECT port FROM companion_tool_grants WHERE call_id=?1",
        [&id],
        |r| r.get(0),
    )?;
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
        if data.len() + chunk.len() > 1024 * 1024 {
            return Err(ApiError::bad("Tool response too large"));
        }
        data.extend_from_slice(&chunk);
    }
    Ok((status, [("content-type", "application/json")], data).into_response())
}
pub async fn connections(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> ApiResult<Json<Value>> {
    let (_, agent, _) = authorize(&s, &id, &h)?;
    let c = config(&s)?;
    Ok(Json(
        json!({"connections":c.agents.get(&agent).into_iter().flatten().map(|x|json!({"id":x.id,"label":x.label,"description":x.description})).collect::<Vec<_>>(),"policy":"Use only for the caller's explicit request. Submission is not completion. Unknown sends must not be retried."}),
    ))
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
    Ok(Json(
        action_row(&s, &id, &rid)?.ok_or_else(ApiError::missing)?,
    ))
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
        let source = json!({"call_id":id,"conversation_id":room,"actor":owner,"audience":"private","delegation_id":did,"message_id":mid,"occurred_at":at,"original_text":text,"source_revision":auth::digest(&text)});
        tx.execute("INSERT INTO companion_actions(call_id,request_id,delegation_id,connection_id,instruction,source,state,result,created_at) VALUES(?1,?2,?3,?4,?5,?6,'submitting','{}',?7)",params![id,a.request_id,did,a.connection_id,a.instruction,source.to_string(),now_ms()])?;
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
    url: String,
    title: String,
    #[serde(default)]
    kind: Option<String>,
}
fn valid_show(v: &Show) -> bool {
    reqwest::Url::parse(&v.url).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
    }) && v.url.len() <= 4000
        && v.title.len() <= 300
        && !v.title.contains(['\n', '\r'])
        && v.kind
            .as_deref()
            .is_none_or(|kind| matches!(kind, "link" | "image" | "video" | "auto"))
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
    let data = json!({"title":v.title,"text":v.url,"url":v.url,"kind":v.kind.unwrap_or("auto".into()),"conversation_id":room});
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
        json!({"state":"available_on_screen","event_id":eid,"card_id":s.db.lock().unwrap().query_row("SELECT seq FROM companion_events WHERE call_id=?1 AND event_key=?2",params![id,eid],|r|r.get::<_,i64>(0))?.to_string(),"note":"Available does not prove opened, played or watched."}),
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
