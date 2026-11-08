//! Bounded browser-reported transport observations. Never authority, delivery
//! proof, or raw networking/media identifiers. Keep latest timing separately
//! from sparse transition history so diagnostics cannot fill the call log.
use crate::state::{AppState, now_ms};
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub(crate) fn validate(v: &Value) -> Result<()> {
    let object = v
        .as_object()
        .filter(|o| o.len() == 10)
        .ok_or_else(|| anyhow::anyhow!("Invalid browser diagnostics"))?;
    for (key, allowed) in [
        (
            "trigger",
            &[
                "heartbeat",
                "visibility_hidden",
                "visibility_visible",
                "focus",
                "blur",
                "pageshow",
                "resume",
                "online",
                "offline",
                "peer_state",
                "ice_state",
                "channel_state",
                "timer_resume",
                "file_picker_open",
                "file_picker_return",
                "page_exit",
                "call_close",
            ][..],
        ),
        ("visibility", &["visible", "hidden"][..]),
        (
            "peer_state",
            &[
                "new",
                "connecting",
                "connected",
                "disconnected",
                "failed",
                "closed",
                "unknown",
            ][..],
        ),
        (
            "ice_state",
            &[
                "new",
                "checking",
                "connected",
                "completed",
                "disconnected",
                "failed",
                "closed",
                "unknown",
            ][..],
        ),
        (
            "channel_state",
            &["connecting", "open", "closing", "closed", "unknown"][..],
        ),
        (
            "audio_state",
            &["running", "suspended", "closed", "interrupted", "unknown"][..],
        ),
        ("wake_lock", &["held", "released", "unavailable"][..]),
    ] {
        ensure!(
            object
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| allowed.contains(&s)),
            "Invalid browser diagnostics"
        );
    }
    for key in ["control_age_ms", "heartbeat_age_ms", "timer_gap_ms"] {
        ensure!(
            object
                .get(key)
                .and_then(Value::as_u64)
                .is_some_and(|n| n <= 3_600_000),
            "Invalid browser timing"
        );
    }
    // Exactly these ten fields; no device names, SDP, addresses or arbitrary text.
    ensure!(
        object.keys().all(|k| [
            "trigger",
            "visibility",
            "peer_state",
            "ice_state",
            "channel_state",
            "audio_state",
            "wake_lock",
            "control_age_ms",
            "heartbeat_age_ms",
            "timer_gap_ms"
        ]
        .contains(&k.as_str())),
        "Unexpected browser diagnostics"
    );
    Ok(())
}
fn structural(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        for key in ["control_age_ms", "heartbeat_age_ms", "timer_gap_ms"] {
            o.remove(key);
        }
    }
    v
}
pub(crate) fn record(s: &AppState, id: &str, v: &Value) -> Result<()> {
    validate(v)?;
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction()?;
    let previous: Option<(String, i64)> = tx
        .query_row(
            "SELECT data,transitions FROM companion_browser_diagnostics WHERE call_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut count = previous.as_ref().map(|r| r.1).unwrap_or(0);
    let changed = previous
        .as_ref()
        .and_then(|r| serde_json::from_str::<Value>(&r.0).ok())
        .is_none_or(|old| structural(&old) != structural(v));
    if changed && count < 120 {
        tx.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'browser_diagnostic',?3,?4)",params![id,format!("browser-diagnostic-{count}"),v.to_string(),now_ms()])?;
        count += 1;
    }
    tx.execute("INSERT INTO companion_browser_diagnostics(call_id,data,transitions,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(call_id) DO UPDATE SET data=excluded.data,transitions=excluded.transitions,updated_at=excluded.updated_at",params![id,v.to_string(),count,now_ms()])?;
    tx.commit()?;
    Ok(())
}
