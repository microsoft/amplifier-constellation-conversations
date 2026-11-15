//! Optional Instance source receipts projected through the existing delivery row.
//! A receipt or work reference never commissions work or establishes fulfillment.
use crate::{
    delivery,
    state::{AppState, now_ms},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const META: &str = "com.microsoft.amplifier.source-delivery";
pub(crate) const METHOD: &str = "_amplifier/source_receipts";
const INSTANCE: &str = "io.github.bkrabach.agent-instance";

pub(crate) fn capability(init: &Value) -> Option<&Value> {
    let c = &init["_meta"][INSTANCE]["inputContinuity"]["sourceDelivery"];
    (c["version"] == 1
        && c["metadata"] == META
        && c["receiptsMethod"] == METHOD
        && c["ordinaryPrompts"] == true
        && c["promptRetry"] == "inspectOnly")
        .then_some(c)
}

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    let columns: Vec<String> = db
        .prepare("PRAGMA table_info(input_deliveries)")?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    for name in ["source_metadata", "source_binding", "source_receipt"] {
        if !columns.iter().any(|c| c == name) {
            db.execute_batch(&format!(
                "ALTER TABLE input_deliveries ADD COLUMN {name} TEXT"
            ))?;
        }
    }
    Ok(())
}

fn strings(db: &Connection, sql: &str, room: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare(sql)?
        .query_map([room], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn audience(db: &Connection, room: &str) -> Result<String> {
    let (owner, shared): (String, bool) = db.query_row(
        "SELECT owner,shared FROM conversations WHERE id=?1",
        [room],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let people = strings(
        db,
        "SELECT username FROM room_members WHERE conversation_id=?1 ORDER BY username",
        room,
    )?;
    let agents = strings(
        db,
        "SELECT agent_id FROM room_agents WHERE conversation_id=?1 ORDER BY agent_id",
        room,
    )?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([
            room, owner, shared, people, agents
        ]))?)
    ))
}

fn original(db: &Connection, message: &str) -> Result<Value> {
    let (room, actor, text, at): (String, String, String, i64) = db.query_row(
        "SELECT conversation_id,actor,text,created_at FROM messages WHERE id=?1 AND role='user'",
        [message],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let attachments: Vec<Value> = db.prepare("SELECT id,name,mime_type,data FROM attachments WHERE message_id=?1 ORDER BY created_at,id")?
        .query_map([message], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,format!("{:x}",Sha256::digest(r.get::<_,Vec<u8>>(3)?))])))?
        .collect::<rusqlite::Result<_>>()?;
    let revision = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([
            message,
            room,
            actor,
            text,
            at,
            attachments
        ]))?)
    );
    Ok(json!({"id":message,"revision":revision,"occurredAt":at}))
}

/// A call's generated delegation prompt is evidence of transport, not the
/// original human speech. Keep those revisions separate while reusing the
/// original message/input identities and the negotiated source receipt profile.
fn source(
    db: &Connection,
    room: &str,
    message: &str,
    agent: &str,
) -> Result<Option<(Value, Option<Value>)>> {
    let call: Option<String> = db
        .query_row(
            "SELECT id FROM companion_calls WHERE conversation_id=?1",
            [room],
            |r| r.get(0),
        )
        .optional()?;
    let Some(call) = call else {
        return Ok(Some((original(db, message)?, None)));
    };
    let row: Option<(String, Option<String>, Option<String>)> = db.query_row(
        "SELECT id,source,request_text FROM companion_delegations WHERE call_id=?1 AND message_id=?2 AND state IN ('dispatched','completed','failed','unknown')",
        params![call,message], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?;
    let Some((delegation, Some(raw), Some(text))) = row else {
        // Legacy/no-speech and other call inputs retain their existing behavior;
        // do not invent source provenance from a generated prompt or label.
        return Ok(None);
    };
    ensure!(!text.trim().is_empty(), "No retained human request text");
    let identity = crate::companion_history::private_identity(db, &call)?
        .context("The original private call identity is unavailable")?;
    let actor: String = db.query_row(
        "SELECT actor FROM messages WHERE id=?1 AND conversation_id=?2 AND role='user'",
        params![message, room],
        |r| r.get(0),
    )?;
    ensure!(
        identity.room == room && identity.owner == actor && identity.agent == agent,
        "The original call source or recipient changed"
    );
    let at = crate::companion_awareness::original_human_source_time(db, &call, &delegation)?
        .context("The original human transcript is unavailable")?;
    let fragments: Value = serde_json::from_str(&raw)?;
    let revision = crate::auth::digest(&text);
    let evidence = original(db, message)?;
    let binding = json!({"kind":"companion","call_id":call,"delegation_id":delegation,
        "conversation_id":room,"message_id":message,"actor":actor,"agent_id":agent,
        "source_revision":revision,"occurred_at":at,
        "fragment_revision":crate::auth::digest(&serde_json::to_string(&fragments)?),
        "evidence_revision":evidence["revision"]});
    Ok(Some((
        json!({"id":message,"revision":revision,"occurredAt":at}),
        Some(binding),
    )))
}

pub(crate) fn prepare(
    state: &AppState,
    id: &str,
    sid: &str,
    endpoint: &str,
    init: &Value,
) -> Result<Option<Value>> {
    let db = state.db.lock().unwrap();
    let (room,message,agent,old_meta,old_binding): (String,String,String,Option<String>,Option<String>) = db.query_row(
        "SELECT conversation_id,message_id,agent_id,source_metadata,source_binding FROM input_deliveries WHERE id=?1", [id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let Some(cap) = capability(init) else {
        ensure!(
            old_meta.is_none(),
            "The original source receipt profile is no longer available"
        );
        return Ok(None);
    };
    let Some((original, companion)) = source(&db, &room, &message, &agent)? else {
        ensure!(
            old_meta.is_none(),
            "The original call source is unavailable"
        );
        return Ok(None);
    };
    let metadata = json!({"version":1,"inputId":id,"sources":[original]});
    let mut binding = json!({"provider_id":state.config.provider_id(),"audience":audience(&db,&room)?,"session_id":sid,"endpoint":endpoint,"work_references":cap["workReferences"]==true});
    if let Some(companion) = companion {
        binding["companion"] = companion;
    }
    if let Some(old) = old_meta {
        ensure!(
            serde_json::from_str::<Value>(&old)? == metadata,
            "The original source revision changed"
        );
        let old: Value = serde_json::from_str(
            old_binding
                .as_deref()
                .context("Missing original source binding")?,
        )?;
        ensure!(
            old["provider_id"] == binding["provider_id"]
                && old["audience"] == binding["audience"]
                && old["session_id"] == binding["session_id"]
                && old["endpoint"] == binding["endpoint"]
                && old["companion"] == binding["companion"],
            "The original source session or audience changed"
        );
    } else {
        db.execute(
            "UPDATE input_deliveries SET source_metadata=?2,source_binding=?3 WHERE id=?1",
            params![id, metadata.to_string(), binding.to_string()],
        )?;
    }
    Ok(Some(metadata))
}

#[derive(Clone)]
pub(crate) struct Saved {
    pub id: String,
    pub metadata: Value,
    pub session: String,
    pub endpoint: String,
    binding: Value,
}

pub(crate) fn saved(db: &Connection, id: &str) -> Result<Option<Saved>> {
    let row: Option<(Option<String>, Option<String>)> = db
        .query_row(
            "SELECT source_metadata,source_binding FROM input_deliveries WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(metadata), Some(binding))) = row else {
        return Ok(None);
    };
    let metadata: Value = serde_json::from_str(&metadata)?;
    let binding: Value = serde_json::from_str(&binding)?;
    Ok(Some(Saved {
        id: id.into(),
        metadata,
        session: binding["session_id"]
            .as_str()
            .context("Missing original session")?
            .into(),
        endpoint: binding["endpoint"]
            .as_str()
            .context("Missing original endpoint")?
            .into(),
        binding,
    }))
}

fn current(db: &Connection, item: &Saved, provider: &str) -> Result<bool> {
    type CurrentSource = (String, String, String, Option<String>, Option<String>);
    let row: Option<CurrentSource> = db.query_row(
        "SELECT d.conversation_id,d.message_id,d.agent_id,r.session_id,r.endpoint FROM input_deliveries d JOIN room_agents r ON r.conversation_id=d.conversation_id AND r.agent_id=d.agent_id WHERE d.id=?1", [&item.id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((room, message, agent, session, endpoint)) = row else {
        return Ok(false);
    };
    if item.binding.get("companion").is_some() {
        let Ok(Some((original, Some(binding)))) = source(db, &room, &message, &agent) else {
            return Ok(false);
        };
        if item.binding["companion"] != binding || item.metadata["sources"] != json!([original]) {
            return Ok(false);
        }
    }
    Ok(item.binding["provider_id"] == provider
        && session.as_deref() == Some(item.session.as_str())
        && endpoint.as_deref() == Some(item.endpoint.as_str())
        && item.binding["audience"] == audience(db, &room)?)
}

pub(crate) fn inspection(
    state: &AppState,
    id: &str,
    sid: &str,
    endpoint: &str,
    init: &Value,
) -> Result<Saved> {
    ensure!(
        capability(init).is_some(),
        "The original source receipt profile is unavailable"
    );
    let db = state.db.lock().unwrap();
    let item = saved(&db, id)?.context("This input has no negotiated source receipt")?;
    ensure!(
        item.session == sid
            && item.endpoint == endpoint
            && current(&db, &item, &state.config.provider_id())?,
        "The original source session, endpoint or audience is unavailable"
    );
    Ok(item)
}

pub(crate) fn for_turn(state: &AppState, turn: &str) -> Result<Vec<String>> {
    strings(
        &state.db.lock().unwrap(),
        "SELECT id FROM input_deliveries WHERE target_turn=?1 AND source_metadata IS NOT NULL ORDER BY sequence",
        turn,
    )
}

fn identifier(v: &Value) -> Option<&str> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
}

fn receipt(item: &Saved, reply: &Value, work_supported: bool) -> Result<Value> {
    let work_supported = work_supported && item.binding["work_references"] == true;
    ensure!(reply["version"] == 1, "Unsupported source receipt version");
    let rows = reply["receipts"]
        .as_array()
        .context("Missing source receipt list")?;
    ensure!(
        rows.len() <= 40,
        "Source receipt list exceeds negotiated bounds"
    );
    let sources = item.metadata["sources"]
        .as_array()
        .context("Missing original sources")?;
    let mut output = Vec::new();
    for row in rows {
        ensure!(
            sources.iter().any(|s| s["id"] == row["sourceId"]
                && s["revision"] == row["revision"]
                && s["occurredAt"] == row["occurredAt"]),
            "Source receipt does not match original revision and time"
        );
        ensure!(
            !output
                .iter()
                .any(|r: &Value| r["sourceId"] == row["sourceId"]
                    && r["revision"] == row["revision"]),
            "Duplicate source receipt"
        );
        let state = row["state"]
            .as_str()
            .context("Missing source delivery state")?;
        ensure!(
            [
                "received",
                "submitted",
                "remoteQueued",
                "incorporated",
                "notAccepted",
                "promptRequired",
                "unknown"
            ]
            .contains(&state),
            "Unsupported source delivery state"
        );
        let mut work = Vec::new();
        if work_supported {
            let refs = row["work"]
                .as_array()
                .context("Missing work reference coverage")?;
            ensure!(refs.len() <= 64, "Work reference bound exceeded");
            for w in refs {
                let work_state = w["state"].as_str().context("Missing work state")?;
                ensure!(
                    [
                        "proposed",
                        "accepted",
                        "in_progress",
                        "blocked",
                        "review",
                        "completed",
                        "cancelled"
                    ]
                    .contains(&work_state),
                    "Unsupported work state"
                );
                work.push(json!({"id":identifier(&w["id"]).context("Invalid work identity")?,"ownerAgentId":identifier(&w["ownerAgentId"]).context("Invalid work owner")?,
                    "revision":w["revision"].as_u64().context("Invalid work revision")?,"state":work_state,"inputId":identifier(&w["inputId"]).context("Missing work input identity")?}));
            }
        }
        let response = match row["response"]["kind"].as_str() {
            Some("error") => json!({"kind":"error","code":row["response"]["code"].as_i64()}),
            Some("result") => {
                json!({"kind":"result","stopReason":row["response"]["stopReason"].as_str().filter(|s|["end_turn","cancelled","max_tokens","max_turn_requests","refusal"].contains(s))})
            }
            _ => Value::Null,
        };
        output.push(json!({"sourceId":row["sourceId"],"revision":row["revision"],"occurredAt":row["occurredAt"],"inputId":identifier(&row["inputId"]),
            "state":state,"incorporated":row["incorporated"].as_bool(),"response":response,"observedAt":row["observedAt"].as_i64(),"completion":"not_established","work":work,
            "workCoverage":if work_supported {row["workCoverage"].as_str().filter(|s|["partial","retained_session_records"].contains(s)).unwrap_or("unknown")}else{"unsupported"}}));
    }
    Ok(
        json!({"version":1,"receipts":output,"coverage":{"delivery":"recent_session_records","work":if work_supported {"retained_session_records"} else {"unsupported"},"missing":"unknown"}}),
    )
}

pub(crate) fn record(state: &AppState, item: &Saved, reply: &Value, init: &Value) -> Result<()> {
    let supported =
        capability(init).context("Source profile disappeared")?["workReferences"] == true;
    let safe = receipt(item, reply, supported)?;
    let db = state.db.lock().unwrap();
    ensure!(
        current(&db, item, &state.config.provider_id())?,
        "The original source audience changed during inspection"
    );
    let still = saved(&db, &item.id)?.context("Original source was removed")?;
    ensure!(
        still.metadata == item.metadata && still.binding == item.binding,
        "Original source binding changed"
    );
    db.execute(
        "UPDATE input_deliveries SET source_receipt=?2 WHERE id=?1",
        params![
            item.id,
            json!({"receipt":safe,"observed_at":now_ms()}).to_string()
        ],
    )?;
    drop(db);
    state.changed();
    Ok(())
}

/// Failure to observe does not alter input delivery or erase older evidence.
/// Older cached facts are withheld until a new authorized observation succeeds.
pub(crate) fn unavailable(state: &AppState, id: &str) -> Result<()> {
    let db = state.db.lock().unwrap();
    let raw: Option<String> = db.query_row(
        "SELECT source_receipt FROM input_deliveries WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    let mut value = raw
        .map(|s| serde_json::from_str::<Value>(&s))
        .transpose()?
        .unwrap_or_else(|| json!({}));
    value["unavailable_at"] = json!(now_ms());
    db.execute(
        "UPDATE input_deliveries SET source_receipt=?2 WHERE id=?1",
        params![id, value.to_string()],
    )?;
    drop(db);
    state.changed();
    Ok(())
}

pub(crate) fn projection(db: &Connection, id: &str, provider: &str) -> Result<Option<Value>> {
    let Some(item) = saved(db, id)? else {
        return Ok(None);
    };
    if !current(db, &item, provider)? {
        return Ok(Some(
            json!({"version":1,"input_id":id,"sources":item.metadata["sources"],"status":"unavailable","receipt":null,"observed_at":null}),
        ));
    }
    let raw: Option<String> = db.query_row(
        "SELECT source_receipt FROM input_deliveries WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    let observed: Option<Value> = raw.map(|s| serde_json::from_str(&s)).transpose()?;
    if observed
        .as_ref()
        .is_some_and(|v| v.get("unavailable_at").is_some())
    {
        return Ok(Some(
            json!({"version":1,"input_id":id,"sources":item.metadata["sources"],"status":"unavailable","receipt":null,"observed_at":null}),
        ));
    }
    let mut value = json!({"version":1,"input_id":id,"sources":item.metadata["sources"],"status":if observed.is_some(){"observed"}else{"unobserved"},
        "receipt":observed.as_ref().map(|v|&v["receipt"]),"observed_at":observed.as_ref().map(|v|&v["observed_at"])});
    if let Some(origin) = item.binding.get("companion") {
        value["origin"] = origin.clone();
    }
    Ok(Some(value))
}

pub(crate) fn decorate(db: &Connection, items: &mut [Value], provider: &str) -> Result<()> {
    for row in items {
        if let Some(value) = projection(
            db,
            row["id"].as_str().context("Missing delivery identity")?,
            provider,
        )? {
            row["source_delivery"] = value;
        }
    }
    Ok(())
}

pub(crate) fn input(state: &AppState, room: &str, id: &str) -> Result<delivery::Input> {
    let db = state.db.lock().unwrap();
    let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM input_deliveries WHERE id=?1 AND conversation_id=?2 AND source_metadata IS NOT NULL)",params![id,room],|r|r.get(0))?;
    ensure!(valid, "No retained source receipt profile for this input");
    let item = saved(&db, id)?.context("Missing original source")?;
    ensure!(
        current(&db, &item, &state.config.provider_id())?,
        "Original source session, endpoint or audience changed"
    );
    delivery::input(&db, id)
}
