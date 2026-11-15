//! Transaction-owned update admission barrier. This is application lifecycle,
//! not authority to cancel, acknowledge, replay, or roll back conversation work.
use crate::error::{ApiError, ApiResult};
use anyhow::{Context, Result, ensure};
use axum::http::StatusCode;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS native_update_admission(
            transaction_id TEXT PRIMARY KEY,
            phase TEXT NOT NULL CHECK(phase IN ('prepared','restored')),
            acquired_at INTEGER NOT NULL,
            restored_at INTEGER);
         CREATE UNIQUE INDEX IF NOT EXISTS native_update_one_active
            ON native_update_admission((1)) WHERE phase='prepared';
         CREATE TABLE IF NOT EXISTS native_update_activity(
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_at INTEGER NOT NULL);",
    )?;
    Ok(())
}

fn validate_transaction(id: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok(),
        "Use an update transaction UUID"
    );
    Ok(())
}

/// Call within the same transaction that inserts new work. A separate check
/// before opening that transaction would race an external prepare operation.
pub fn ensure_open(db: &Connection) -> ApiResult<()> {
    let closed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_update_admission WHERE phase='prepared')",
        [],
        |r| r.get(0),
    )?;
    if closed {
        Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "This installation is updating. Your new request was not accepted; existing work is retained.".into(),
        ))
    } else {
        Ok(())
    }
}

pub fn closed(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_update_admission WHERE phase='prepared')",
        [],
        |r| r.get(0),
    )?)
}

/// A short local operation which is not already represented by an accepted
/// input/call row. On cancellation the owning future drops its child before this
/// guard. On process crash the row remains uncertain; startup never clears it.
pub struct ActivityGuard {
    state: std::sync::Arc<crate::state::AppState>,
    id: String,
}
impl Drop for ActivityGuard {
    fn drop(&mut self) {
        if let Ok(db) = self.state.db.lock() {
            let _ = db.execute("DELETE FROM native_update_activity WHERE id=?1", [&self.id]);
        }
    }
}
fn begin_activity(
    state: &std::sync::Arc<crate::state::AppState>,
    kind: &'static str,
) -> ApiResult<ActivityGuard> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut db = state.db.lock().unwrap();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_open(&tx)?;
    tx.execute(
        "INSERT INTO native_update_activity(id,kind,created_at) VALUES(?1,?2,?3)",
        params![id, kind, crate::state::now_ms()],
    )?;
    tx.commit()?;
    Ok(ActivityGuard {
        state: state.clone(),
        id,
    })
}
pub fn begin_probe(state: &std::sync::Arc<crate::state::AppState>) -> ApiResult<ActivityGuard> {
    begin_activity(state, "connection_probe")
}
pub fn begin_configuration(
    state: &std::sync::Arc<crate::state::AppState>,
) -> ApiResult<ActivityGuard> {
    begin_activity(state, "configuration")
}

/// The native CLI must not open AppState merely to protect a configuration
/// write: AppState startup performs recovery. This guard uses only the existing
/// database and the same admission journal as the running service.
pub struct HomeActivityGuard {
    db: Connection,
    id: String,
}
impl Drop for HomeActivityGuard {
    fn drop(&mut self) {
        let _ = self
            .db
            .execute("DELETE FROM native_update_activity WHERE id=?1", [&self.id]);
    }
}
pub fn begin_configuration_home(home: &Path) -> Result<HomeActivityGuard> {
    let mut db = open(home, false)?;
    let id = uuid::Uuid::new_v4().to_string();
    {
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            !closed(&tx)?,
            "This installation is updating; configuration was not changed"
        );
        tx.execute(
            "INSERT INTO native_update_activity(id,kind,created_at) VALUES(?1,'configuration',?2)",
            params![id, crate::state::now_ms()],
        )?;
        tx.commit()?;
    }
    Ok(HomeActivityGuard { db, id })
}

#[derive(Serialize)]
struct Busy {
    kind: &'static str,
    count: i64,
}

fn busy(db: &Connection) -> Result<Vec<Busy>> {
    // Retained state is inspected, never normalized. In particular, an unknown
    // outcome cannot silently turn into completed, absent, or safe to retry.
    let checks = [
        (
            "active_or_uncertain_local_operations",
            "SELECT count(*) FROM native_update_activity",
        ),
        (
            "active_calls",
            "SELECT count(*) FROM companion_calls WHERE state IN ('starting','connecting','connected','closing')",
        ),
        (
            "retained_tool_servers",
            "SELECT count(*) FROM companion_tool_grants WHERE port IS NOT NULL",
        ),
        (
            "conversation_work",
            "SELECT count(*) FROM conversations WHERE status='working'",
        ),
        (
            "streaming_messages",
            "SELECT count(*) FROM messages WHERE status='streaming'",
        ),
        (
            "pending_or_uncertain_delegations",
            "SELECT count(*) FROM companion_delegations WHERE state IN ('waiting_transcript','dispatched','unknown')",
        ),
        (
            "unconfirmed_call_closure",
            "SELECT count(*) FROM companion_calls c WHERE c.state='interrupted' AND NOT EXISTS(SELECT 1 FROM companion_events e WHERE e.call_id=c.id AND e.kind='closed' AND json_extract(e.data,'$.confirmed')=1)",
        ),
    ];
    let mut busy: Vec<Busy> = checks
        .into_iter()
        .filter_map(
            |(kind, sql)| match db.query_row(sql, [], |r| r.get::<_, i64>(0)) {
                Ok(0) => None,
                Ok(count) => Some(Ok(Busy { kind, count })),
                Err(error) => Some(Err(error.into())),
            },
        )
        .collect::<Result<_>>()?;
    let retained = crate::native_update::retained_legacy_inputs(db)?;
    let sql = format!(
        "SELECT d.id,d.state FROM input_deliveries d LEFT JOIN messages m ON m.id=d.target_turn AND m.conversation_id=d.conversation_id AND m.agent_id=d.agent_id AND m.role='assistant' WHERE d.state IN ('received','promptRequired','preparing','offering','submitted','received_remote','remoteQueued','ack_pending','unknown') OR (d.state='notAccepted' AND NOT ({})) OR (d.state='incorporated' AND COALESCE(m.status,'')!='complete')",
        crate::delivery::PROVEN_NOT_STARTED
    );
    let mut inputs = db.prepare(&sql)?;
    let mut rows = inputs.query([])?;
    let mut pending = 0i64;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let state: String = row.get(1)?;
        if !(matches!(state.as_str(), "unknown" | "notAccepted") && retained.contains(&id)) {
            pending += 1;
        }
    }
    if pending != 0 {
        busy.push(Busy {
            kind: "pending_or_uncertain_inputs",
            count: pending,
        });
    }
    let mut actions = db.prepare("SELECT call_id,request_id,state FROM companion_actions WHERE state IN ('submitting','queued','accepted','unknown')")?;
    let mut rows = actions.query([])?;
    let mut pending = 0;
    while let Some(row) = rows.next()? {
        let state: String = row.get(2)?;
        let settled = matches!(state.as_str(), "queued" | "accepted")
            && crate::companion_awareness::action_settled_for_update(
                db,
                &row.get::<_, String>(0)?,
                &row.get::<_, String>(1)?,
            )?;
        pending += i64::from(!settled);
    }
    if pending != 0 {
        busy.push(Busy {
            kind: "pending_or_uncertain_actions",
            count: pending,
        });
    }
    Ok(busy)
}

// Advisory only: these reads explain whether retained blockers describe known
// work to wait for. They never remove a blocker, acknowledge an effect, or grant
// admission. An unavailable/malformed association stays needs-attention.
fn waiting_for_work(db: &Connection, blockers: &[Busy]) -> Result<bool> {
    if blockers.is_empty() {
        return Ok(false);
    }
    for blocker in blockers {
        let invalid = match blocker.kind {
            "active_calls" => {
                "SELECT EXISTS(SELECT 1 FROM companion_calls c WHERE c.state IN ('starting','connecting','connected','closing') AND NOT EXISTS(SELECT 1 FROM conversations r JOIN room_agents a ON a.conversation_id=r.id JOIN agent_endpoints e ON e.id=a.agent_id WHERE r.id=c.conversation_id AND a.agent_id=c.agent_id))"
            }
            "retained_tool_servers" => {
                "SELECT EXISTS(SELECT 1 FROM companion_tool_grants g WHERE g.port IS NOT NULL AND (g.port NOT BETWEEN 1 AND 65535 OR NOT EXISTS(SELECT 1 FROM companion_calls c JOIN conversations r ON r.id=c.conversation_id JOIN room_agents a ON a.conversation_id=r.id AND a.agent_id=c.agent_id JOIN agent_endpoints e ON e.id=a.agent_id WHERE c.id=g.call_id AND c.state IN ('starting','connecting','connected','closing'))))"
            }
            "conversation_work" => {
                "SELECT EXISTS(SELECT 1 FROM conversations c WHERE c.status='working' AND NOT EXISTS(SELECT 1 FROM room_agents a JOIN agent_endpoints e ON e.id=a.agent_id WHERE a.conversation_id=c.id))"
            }
            "streaming_messages" => {
                "SELECT EXISTS(SELECT 1 FROM messages m WHERE m.status='streaming' AND (COALESCE(m.role,'')!='assistant' OR NOT EXISTS(SELECT 1 FROM conversations c JOIN room_agents a ON a.conversation_id=c.id JOIN agent_endpoints e ON e.id=a.agent_id WHERE c.id=m.conversation_id AND a.agent_id=m.agent_id AND c.status='working')))"
            }
            "pending_or_uncertain_inputs" => {
                if !waiting_inputs(db)? {
                    return Ok(false);
                }
                continue;
            }
            "pending_or_uncertain_delegations" => {
                if !waiting_delegations(db)? {
                    return Ok(false);
                }
                continue;
            }
            "pending_or_uncertain_actions" => {
                if !waiting_actions(db)? {
                    return Ok(false);
                }
                continue;
            }
            // Local activity has no persisted owner-generation proof, and an
            // unconfirmed closure or an unrecognized category is not ordinary.
            _ => return Ok(false),
        };
        if db.query_row(invalid, [], |row| row.get::<_, bool>(0))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn waiting_input(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM input_deliveries d JOIN messages source ON source.id=d.message_id AND source.conversation_id=d.conversation_id JOIN conversations c ON c.id=d.conversation_id JOIN room_agents a ON a.conversation_id=c.id AND a.agent_id=d.agent_id JOIN agent_endpoints e ON e.id=a.agent_id LEFT JOIN messages target ON target.id=d.target_turn WHERE d.id=?1 AND (d.state IN ('received','promptRequired') OR (d.state IN ('preparing','offering','submitted','received_remote','remoteQueued','incorporated') AND target.conversation_id=d.conversation_id AND target.agent_id=d.agent_id AND target.role='assistant' AND target.status='streaming') OR (d.state='preparing' AND d.mode='prompt' AND COALESCE(d.target_turn,'')!='' AND target.id IS NULL AND COALESCE(d.session_id,'')='' AND COALESCE(d.endpoint,'')='' AND d.receipt IS NULL)))",
        [id], |row| row.get(0),
    )?)
}

fn waiting_inputs(db: &Connection) -> Result<bool> {
    let retained = crate::native_update::retained_legacy_inputs(db)?;
    // Match busy()'s exact input selection and its existing historical exemption.
    let sql = format!(
        "SELECT d.id,d.state FROM input_deliveries d LEFT JOIN messages m ON m.id=d.target_turn AND m.conversation_id=d.conversation_id AND m.agent_id=d.agent_id AND m.role='assistant' WHERE d.state IN ('received','promptRequired','preparing','offering','submitted','received_remote','remoteQueued','ack_pending','unknown') OR (d.state='notAccepted' AND NOT ({})) OR (d.state='incorporated' AND COALESCE(m.status,'')!='complete')",
        crate::delivery::PROVEN_NOT_STARTED
    );
    let mut query = db.prepare(&sql)?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let state: String = row.get(1)?;
        if matches!(state.as_str(), "unknown" | "notAccepted") && retained.contains(&id) {
            continue;
        }
        if !waiting_input(db, &id)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn waiting_delegations(db: &Connection) -> Result<bool> {
    let mut query = db.prepare("SELECT d.state,d.message_id,c.state,c.conversation_id,c.agent_id FROM companion_delegations d LEFT JOIN companion_calls c ON c.id=d.call_id WHERE d.state IN ('waiting_transcript','dispatched','unknown')")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let state: String = row.get(0)?;
        let message: Option<String> = row.get(1)?;
        let call: Option<String> = row.get(2)?;
        let room: Option<String> = row.get(3)?;
        let agent: Option<String> = row.get(4)?;
        let (Some(message), Some(call), Some(room), Some(agent)) = (message, call, room, agent)
        else {
            return Ok(false);
        };
        if state == "unknown" || message.is_empty() {
            return Ok(false);
        }
        let associated: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM conversations c JOIN room_agents a ON a.conversation_id=c.id JOIN agent_endpoints e ON e.id=a.agent_id WHERE c.id=?1 AND a.agent_id=?2)", params![room,agent], |r| r.get(0))?;
        if !associated {
            return Ok(false);
        }
        // The call commits transcript/dispatch intent before send inserts its
        // input. Existing intent while that call is active is not an unknown
        // outcome. If a source row exists, it must still belong to this room.
        let source_room: Option<String> = db
            .query_row(
                "SELECT conversation_id FROM messages WHERE id=?1",
                [&message],
                |r| r.get(0),
            )
            .optional()?;
        if source_room.as_ref().is_some_and(|value| value != &room) {
            return Ok(false);
        }
        if matches!(
            call.as_str(),
            "starting" | "connecting" | "connected" | "closing"
        ) {
            continue;
        }
        if state != "dispatched" || call != "closed" {
            return Ok(false);
        }
        let input: Option<String> = db.query_row("SELECT id FROM input_deliveries WHERE message_id=?1 AND conversation_id=?2 AND agent_id=?3", params![message,room,agent], |r| r.get(0)).optional()?;
        if !input
            .as_deref()
            .is_some_and(|id| waiting_input(db, id).unwrap_or(false))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn waiting_actions(db: &Connection) -> Result<bool> {
    let mut query = db.prepare("SELECT call_id,request_id,state,result,source FROM companion_actions WHERE state IN ('submitting','queued','accepted','unknown')")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let call: String = row.get(0)?;
        let request: String = row.get(1)?;
        let state: String = row.get(2)?;
        if !matches!(state.as_str(), "queued" | "accepted") {
            return Ok(false);
        }
        if crate::companion_awareness::action_settled_for_update(db, &call, &request)? {
            continue;
        }
        let receipt: Value = serde_json::from_str(&row.get::<_, String>(3)?)?;
        let source: Value = serde_json::from_str(&row.get::<_, String>(4)?)?;
        if receipt["request_id"] != request
            || receipt["state"] != state
            || source["call_id"] != call
        {
            return Ok(false);
        }
        let associated: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM companion_actions a JOIN companion_calls c ON c.id=a.call_id JOIN companion_delegations d ON d.call_id=c.id AND d.id=a.delegation_id JOIN messages m ON m.id=d.message_id AND m.conversation_id=c.conversation_id JOIN conversations r ON r.id=c.conversation_id JOIN room_agents p ON p.conversation_id=r.id AND p.agent_id=c.agent_id JOIN agent_endpoints e ON e.id=p.agent_id WHERE a.call_id=?1 AND a.request_id=?2 AND COALESCE(a.connection_id,'')!='' AND d.state IN ('dispatched','completed') AND d.id=?3 AND m.id=?4 AND c.conversation_id=?5)", params![call,request,source["delegation_id"].as_str(),source["message_id"].as_str(),source["conversation_id"].as_str()], |r| r.get(0))?;
        if !associated {
            return Ok(false);
        }
        // A later explicit unknown/review/interrupted observation must not be
        // hidden by the earlier successful acceptance acknowledgement. Absence
        // of a later observation says only that accepted work remains pending.
        let observation: Option<String> = db
            .query_row(
                "SELECT data FROM companion_observations WHERE call_id=?1 AND request_id=?2",
                params![call, request],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(raw) = observation {
            let value: Value = serde_json::from_str(&raw)?;
            if value["request_id"] != request
                || !matches!(
                    value["state"].as_str(),
                    Some("accepted" | "running" | "waiting" | "blocked" | "admitted" | "replied")
                )
                || matches!(
                    value["delivery_state"].as_str(),
                    Some("unknown" | "interrupted")
                )
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn snapshot(db: &Connection) -> Result<Value> {
    let owner: Option<String> = db
        .query_row(
            "SELECT transaction_id FROM native_update_admission WHERE phase='prepared'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let busy = busy(db)?;
    // Failure of this supplementary explanation cannot alter admission.
    let waiting_for_work = waiting_for_work(db, &busy).unwrap_or(false);
    Ok(json!({
        "closed":owner.is_some(), "transaction_id":owner,
        "ready":owner.is_some() && busy.is_empty(), "busy":busy,
        "waiting_for_work":waiting_for_work
    }))
}

/// The write transaction serializes the gate with the actual accepted-input or
/// call insertion, including when the updater is a different process.
pub fn prepare_db(db: &mut Connection, transaction: &str) -> Result<Value> {
    validate_transaction(transaction)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior: Option<String> = tx
        .query_row(
            "SELECT phase FROM native_update_admission WHERE transaction_id=?1",
            [transaction],
            |r| r.get(0),
        )
        .optional()?;
    if prior.as_deref() == Some("restored") {
        return Ok(
            json!({"ready":false,"reason":"transaction_finished","transaction_id":transaction,"phase":"restored"}),
        );
    }
    let owner: Option<String> = tx
        .query_row(
            "SELECT transaction_id FROM native_update_admission WHERE phase='prepared'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if owner.as_deref().is_some_and(|owner| owner != transaction) {
        return Ok(json!({"ready":false,"reason":"another_update","transaction_id":owner}));
    }
    let acquired = owner.is_none();
    if acquired {
        tx.execute(
            "INSERT INTO native_update_admission(transaction_id,phase,acquired_at) VALUES(?1,'prepared',?2)",
            params![transaction, crate::state::now_ms()],
        )?;
    }
    let mut result = snapshot(&tx)?;
    if result["ready"] != true {
        result["reason"] = json!("busy");
        // A shared worker does not restore a participant whose prepare declined.
        // Only remove the new gate acquired by this invocation; an existing
        // prepared gate is retained after a lost reply or unexpected new fact.
        if acquired {
            tx.execute(
                "DELETE FROM native_update_admission WHERE transaction_id=?1 AND phase='prepared'",
                [transaction],
            )?;
            result["closed"] = json!(false);
            result["transaction_id"] = Value::Null;
        }
    }
    tx.commit()?;
    Ok(result)
}

pub fn status_db(db: &Connection) -> Result<Value> {
    let tx = db.unchecked_transaction()?;
    let result = snapshot(&tx)?;
    tx.commit()?;
    Ok(result)
}

/// Only the owner transaction releases its gate. A durable restored row makes
/// retry after a lost restore acknowledgement harmless, even after restart.
pub fn restore_db(db: &mut Connection, transaction: &str) -> Result<Value> {
    validate_transaction(transaction)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior: Option<String> = tx
        .query_row(
            "SELECT phase FROM native_update_admission WHERE transaction_id=?1",
            [transaction],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        prior.is_some(),
        "No prepared update transaction matches this restore"
    );
    if prior.as_deref() == Some("restored") {
        return Ok(json!({"restored":true,"transaction_id":transaction,"duplicate":true}));
    }
    tx.execute("UPDATE native_update_admission SET phase='restored',restored_at=?2 WHERE transaction_id=?1 AND phase='prepared'", params![transaction,crate::state::now_ms()])?;
    tx.commit()?;
    Ok(json!({"restored":true,"transaction_id":transaction,"duplicate":false}))
}

fn open(home: &Path, read_only: bool) -> Result<Connection> {
    let path = home.join("state.sqlite3");
    let metadata =
        std::fs::symlink_metadata(&path).context("An initialized installation is required")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Use the installation's regular database file"
    );
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(db)
}

pub fn prepare(home: &Path, transaction: &str) -> Result<Value> {
    validate_transaction(transaction)?;
    let mut db = open(home, false)?;
    // Bootstrap the additive gate journal only for an existing application
    // database. Opening it must not run AppState recovery or replace credentials.
    for table in [
        "companion_calls",
        "companion_tool_grants",
        "conversations",
        "messages",
        "input_deliveries",
        "companion_delegations",
        "companion_actions",
        "companion_events",
    ] {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        ensure!(exists, "The application database is incomplete");
    }
    migrate(&db)?;
    prepare_db(&mut db, transaction)
}
pub fn status(home: &Path) -> Result<Value> {
    status_db(&open(home, true)?)
}
pub fn restore(home: &Path, transaction: &str) -> Result<Value> {
    restore_db(&mut open(home, false)?, transaction)
}

/// Release this attempt only before any service stop/switch has been requested.
/// Unlike final restore, this permits the worker to poll prepare with the same
/// immutable transaction after it has deferred. Other owners are untouched.
pub(crate) fn defer(home: &Path, transaction: &str) -> Result<()> {
    validate_transaction(transaction)?;
    let mut db = open(home, false)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM native_update_admission WHERE transaction_id=?1 AND phase='prepared'",
        [transaction],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE companion_calls(id TEXT PRIMARY KEY,state TEXT);
             CREATE TABLE companion_tool_grants(call_id TEXT,port INTEGER);
             CREATE TABLE conversations(id TEXT PRIMARY KEY,status TEXT);
             CREATE TABLE messages(id TEXT PRIMARY KEY,status TEXT,conversation_id TEXT,agent_id TEXT,role TEXT,text TEXT);
             CREATE TABLE input_deliveries(id TEXT PRIMARY KEY,state TEXT,target_turn TEXT,conversation_id TEXT,agent_id TEXT,mode TEXT,session_id TEXT,endpoint TEXT,receipt TEXT);
             CREATE TABLE companion_delegations(id TEXT PRIMARY KEY,state TEXT);
             CREATE TABLE companion_actions(id TEXT PRIMARY KEY,state TEXT,call_id TEXT,request_id TEXT);
             CREATE TABLE companion_events(call_id TEXT,kind TEXT,data TEXT);",
        )
        .unwrap();
        migrate(&db).unwrap();
        (dir, db)
    }
    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn media_only_call_declines_and_releases_only_new_gate_without_changing_call() {
        let (_dir, mut db) = fixture();
        db.execute("INSERT INTO companion_calls VALUES('live','connected')", [])
            .unwrap();
        let transaction = id();
        let reply = prepare_db(&mut db, &transaction).unwrap();
        assert_eq!(reply["reason"], "busy");
        assert_eq!(reply["closed"], false);
        assert_eq!(reply["busy"][0], json!({"kind":"active_calls","count":1}));
        assert!(!closed(&db).unwrap());
        assert_eq!(
            db.query_row("SELECT state FROM companion_calls", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "connected"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM native_update_admission", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn post_hangup_work_and_unknown_outcomes_are_retained_and_block_update() {
        let (_dir, mut db) = fixture();
        db.execute_batch(
            "INSERT INTO companion_calls VALUES('closed','closed');
            INSERT INTO companion_tool_grants VALUES('closed',41235);
            INSERT INTO companion_delegations VALUES('accepted','dispatched');
            INSERT INTO input_deliveries(id,state,target_turn) VALUES('uncertain','unknown',NULL);
            INSERT INTO companion_actions(id,state) VALUES('world-effect','unknown');",
        )
        .unwrap();
        let before: Vec<String> = [
            "companion_calls",
            "companion_tool_grants",
            "companion_delegations",
            "input_deliveries",
            "companion_actions",
        ]
        .into_iter()
        .map(|table| {
            let mut q = db
                .prepare(&format!("SELECT quote(state) FROM {table}").replace(
                    "quote(state) FROM companion_tool_grants",
                    "quote(port) FROM companion_tool_grants",
                ))
                .unwrap();
            q.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join(",")
        })
        .collect();
        let reply = prepare_db(&mut db, &id()).unwrap();
        assert_eq!(reply["ready"], false);
        assert_eq!(reply["busy"].as_array().unwrap().len(), 4);
        let after: Vec<String> = [
            "companion_calls",
            "companion_tool_grants",
            "companion_delegations",
            "input_deliveries",
            "companion_actions",
        ]
        .into_iter()
        .map(|table| {
            let mut q = db
                .prepare(&format!("SELECT quote(state) FROM {table}").replace(
                    "quote(state) FROM companion_tool_grants",
                    "quote(port) FROM companion_tool_grants",
                ))
                .unwrap();
            q.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join(",")
        })
        .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn owner_identity_and_lost_restore_reply_survive_reopening_without_reclosing() {
        let (dir, mut db) = fixture();
        let first = id();
        let other = id();
        assert_eq!(prepare_db(&mut db, &first).unwrap()["ready"], true);
        assert_eq!(prepare_db(&mut db, &first).unwrap()["ready"], true);
        assert_eq!(
            prepare_db(&mut db, &other).unwrap()["reason"],
            "another_update"
        );
        assert!(restore_db(&mut db, &other).is_err());
        assert_eq!(status_db(&db).unwrap()["transaction_id"], first);
        drop(db);
        assert!(status(dir.path()).unwrap()["ready"].as_bool().unwrap());
        assert_eq!(restore(dir.path(), &first).unwrap()["duplicate"], false);
        assert_eq!(restore(dir.path(), &first).unwrap()["duplicate"], true);
        assert_eq!(
            prepare(dir.path(), &first).unwrap()["reason"],
            "transaction_finished"
        );
        assert_eq!(status(dir.path()).unwrap()["closed"], false);
        assert_eq!(prepare(dir.path(), &other).unwrap()["ready"], true);
        // An old restore retry cannot release the newer transaction's gate.
        assert_eq!(restore(dir.path(), &first).unwrap()["duplicate"], true);
        assert_eq!(status(dir.path()).unwrap()["transaction_id"], other);
    }

    #[test]
    fn atomic_admission_committed_before_prepare_is_observed_as_busy() {
        let (dir, mut db) = fixture();
        let barrier = Arc::new(Barrier::new(2));
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ensure_open(&tx).unwrap();
        tx.execute(
            "INSERT INTO companion_calls VALUES('racing-call','starting')",
            [],
        )
        .unwrap();
        let path = dir.path().to_owned();
        let other_barrier = barrier.clone();
        let join = std::thread::spawn(move || {
            other_barrier.wait();
            prepare(&path, &id()).unwrap()
        });
        barrier.wait();
        tx.commit().unwrap();
        let reply = join.join().unwrap();
        assert_eq!(reply["reason"], "busy");
        assert!(!closed(&db).unwrap());
        assert_eq!(
            db.query_row("SELECT count(*) FROM companion_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn prepare_committed_first_denies_new_work_transaction_without_inserting() {
        let (dir, mut db) = fixture();
        let transaction = id();
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
        let mut separate = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        let tx = separate
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let error = ensure_open(&tx).unwrap_err();
        assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
        drop(tx);
        assert_eq!(
            db.query_row("SELECT count(*) FROM companion_calls", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(status_db(&db).unwrap()["transaction_id"], transaction);
    }

    #[test]
    fn unexpected_busy_fact_does_not_clear_an_already_prepared_gate() {
        let (_dir, mut db) = fixture();
        let transaction = id();
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
        db.execute(
            "INSERT INTO companion_actions(id,state) VALUES('retained-late-result','unknown')",
            [],
        )
        .unwrap();
        let reply = prepare_db(&mut db, &transaction).unwrap();
        assert_eq!(reply["ready"], false);
        assert_eq!(reply["closed"], true);
        assert_eq!(reply["transaction_id"], transaction);
    }

    #[test]
    fn read_only_status_has_no_recovery_or_receipt_mutations() {
        let (dir, db) = fixture();
        db.execute_batch(
            "INSERT INTO input_deliveries(id,state,target_turn) VALUES('pending','submitted','target');
            INSERT INTO messages(id,status) VALUES('target','streaming');
            INSERT INTO companion_calls VALUES('active','connected');",
        )
        .unwrap();
        let before = db.total_changes();
        let snapshot = status(dir.path()).unwrap();
        assert_eq!(snapshot["closed"], false);
        assert!(!snapshot["busy"].as_array().unwrap().is_empty());
        assert_eq!(db.total_changes(), before);
        assert_eq!(
            db.query_row("SELECT state FROM input_deliveries", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "submitted"
        );
        assert_eq!(
            db.query_row("SELECT status FROM messages", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "streaming"
        );
        assert_eq!(
            db.query_row("SELECT state FROM companion_calls", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "connected"
        );
    }

    #[test]
    fn missing_installation_or_schema_does_not_create_or_normalize_state() {
        let dir = tempfile::tempdir().unwrap();
        assert!(prepare(dir.path(), &id()).is_err());
        assert!(!dir.path().join("state.sqlite3").exists());
        let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TABLE untouched(value TEXT); INSERT INTO untouched VALUES('retain');",
        )
        .unwrap();
        assert!(prepare(dir.path(), &id()).is_err());
        assert!(status(dir.path()).is_err());
        assert_eq!(
            db.query_row("SELECT value FROM untouched", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "retain"
        );
    }

    #[test]
    fn invalid_transaction_cannot_acquire_or_release_gate() {
        let (_dir, mut db) = fixture();
        for invalid in [
            "",
            "../transaction",
            "not-a-uuid",
            "transaction; DROP TABLE messages",
        ] {
            assert!(prepare_db(&mut db, invalid).is_err());
            assert!(restore_db(&mut db, invalid).is_err());
        }
        assert!(!closed(&db).unwrap());
    }

    #[test]
    fn settled_records_allow_update_but_explicit_unknown_closure_does_not() {
        let (_dir, mut db) = fixture();
        db.execute_batch(
            "INSERT INTO companion_calls VALUES('old','closed');
            INSERT INTO companion_delegations VALUES('finished','completed');
            INSERT INTO messages(id,status,conversation_id,agent_id,role) VALUES('answer','complete','room','agent','assistant');
            INSERT INTO input_deliveries(id,state,target_turn,conversation_id,agent_id) VALUES('done','incorporated','answer','room','agent');
            INSERT INTO companion_actions(id,state) VALUES('known','completed');",
        )
        .unwrap();
        let transaction = id();
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
        restore_db(&mut db, &transaction).unwrap();
        db.execute_batch(
            "INSERT INTO companion_calls VALUES('uncertain','interrupted');
            INSERT INTO companion_events VALUES('uncertain','closed','{\"confirmed\":false}');",
        )
        .unwrap();
        let reply = prepare_db(&mut db, &id()).unwrap();
        assert_eq!(reply["ready"], false);
        assert_eq!(
            reply["busy"],
            json!([{"kind":"unconfirmed_call_closure","count":1}])
        );
    }
    #[test]
    fn missing_provider_closure_is_unknown_not_a_terminal_assumption() {
        let (_dir, mut db) = fixture();
        db.execute(
            "INSERT INTO companion_calls VALUES('missing-close','interrupted')",
            [],
        )
        .unwrap();
        let reply = prepare_db(&mut db, &id()).unwrap();
        assert_eq!(reply["ready"], false);
        assert_eq!(
            reply["busy"],
            json!([{"kind":"unconfirmed_call_closure","count":1}])
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM companion_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn standalone_configuration_guard_uses_existing_database_without_recovery() {
        let (dir, mut db) = fixture();
        db.execute(
            "INSERT INTO input_deliveries(id,state,target_turn) VALUES('unknown','unknown',NULL)",
            [],
        )
        .unwrap();
        let guard = begin_configuration_home(dir.path()).unwrap();
        let reply = prepare_db(&mut db, &id()).unwrap();
        assert_eq!(reply["ready"], false);
        assert_eq!(
            reply["busy"][0]["kind"],
            "active_or_uncertain_local_operations"
        );
        drop(guard);
        assert_eq!(
            db.query_row("SELECT state FROM input_deliveries", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "unknown"
        );
        db.execute("DELETE FROM input_deliveries", []).unwrap();
        assert_eq!(prepare_db(&mut db, &id()).unwrap()["ready"], true);
        assert!(begin_configuration_home(dir.path()).is_err());
    }

    #[test]
    fn exact_proven_local_refusal_is_retained_without_blocking_update() {
        let (_dir, mut db) = fixture();
        db.execute_batch("INSERT INTO messages(id,status,conversation_id,agent_id,role,text) VALUES('answer','interrupted','room','agent','assistant',''); INSERT INTO input_deliveries(id,state,target_turn,conversation_id,agent_id,mode) VALUES('rejected','notAccepted','answer','room','agent','prompt');").unwrap();
        let transaction = id();
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
        restore_db(&mut db, &transaction).unwrap();
        for column in ["session_id", "endpoint", "receipt"] {
            db.execute(
                &format!("UPDATE input_deliveries SET {column}='retained evidence'"),
                [],
            )
            .unwrap();
            assert_eq!(
                prepare_db(&mut db, &id()).unwrap()["ready"],
                false,
                "{column}"
            );
            db.execute(&format!("UPDATE input_deliveries SET {column}=NULL"), [])
                .unwrap();
        }
        db.execute("UPDATE messages SET agent_id='different-agent'", [])
            .unwrap();
        assert_eq!(prepare_db(&mut db, &id()).unwrap()["ready"], false);
        assert_eq!(
            db.query_row("SELECT state FROM input_deliveries", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "notAccepted"
        );
        assert_eq!(
            db.query_row("SELECT status FROM messages", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "interrupted"
        );
    }

    #[test]
    fn same_worker_transaction_can_retry_busy_without_resubmitting_or_stopping_work() {
        let (_dir, mut db) = fixture();
        db.execute("INSERT INTO companion_calls VALUES('live','connected')", [])
            .unwrap();
        let transaction = id();
        for _ in 0..2 {
            assert_eq!(prepare_db(&mut db, &transaction).unwrap()["reason"], "busy");
            assert!(!closed(&db).unwrap());
            assert_eq!(
                db.query_row("SELECT state FROM companion_calls", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "connected"
            );
        }
        // The application's ordinary completion, not prepare, settles the call.
        db.execute("UPDATE companion_calls SET state='closed'", [])
            .unwrap();
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
    }

    #[test]
    fn pre_stop_defer_releases_only_own_gate_and_does_not_consume_transaction() {
        let (dir, mut db) = fixture();
        let transaction = id();
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
        defer(dir.path(), &id()).unwrap();
        assert_eq!(status_db(&db).unwrap()["transaction_id"], transaction);
        defer(dir.path(), &transaction).unwrap();
        assert!(!closed(&db).unwrap());
        assert_eq!(prepare_db(&mut db, &transaction).unwrap()["ready"], true);
        restore_db(&mut db, &transaction).unwrap();
        defer(dir.path(), &transaction).unwrap();
        assert_eq!(
            prepare_db(&mut db, &transaction).unwrap()["reason"],
            "transaction_finished"
        );
    }
}

#[cfg(test)]
mod waiting_tests;
