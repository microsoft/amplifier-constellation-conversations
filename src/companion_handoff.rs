//! Human additions belong to the existing call's work even when the speech
//! provider does not emit another delegation. This is transport, not an intent
//! classifier: exact observed fragments remain the source and the existing
//! authenticated delivery queue decides admission/steering/unknown outcomes.
use super::{AppState, TRANSCRIPT_GRACE_MS, insert_event, now_ms, transcript};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::json;
use std::collections::HashSet;

pub(super) fn internal(id: &str) -> bool {
    id.starts_with("companion-human:")
}

/// A prefix alone is never proof of local origin. The reservation and its
/// private origin receipt are committed together, outside provider input.
pub(super) fn local(db: &Connection, call: &str, id: &str) -> Result<bool> {
    Ok(internal(id) && db.query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_events WHERE call_id=?1 AND event_key=?2 AND kind='human_handoff' AND json_extract(data,'$.id')=?3 AND json_extract(data,'$.origin')='observed_human_continuation')",
        params![call, format!("human-handoff:{id}"), id], |r|r.get(0),
    )?)
}

pub(super) fn established(db: &Connection, call: &str) -> Result<bool> {
    // A provider signal or a pre-dispatch claim is not accepted work. Require
    // the same caller/room/agent's durable ordinary input receipt.
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_delegations x JOIN companion_calls c ON c.id=x.call_id JOIN messages m ON m.id=x.message_id AND m.conversation_id=c.conversation_id AND m.role='user' AND m.actor=c.owner JOIN input_deliveries d ON d.message_id=m.id AND d.conversation_id=c.conversation_id AND d.agent_id=c.agent_id WHERE x.call_id=?1)",
        [call], |r|r.get(0),
    )?)
}

pub(super) fn source(
    db: &Connection,
    call: &str,
    offset: i64,
) -> Result<(Vec<transcript::Fragment>, i64, transcript::Source)> {
    let rows = db.prepare("SELECT seq,data FROM companion_events WHERE call_id=?1 AND kind='transcript' ORDER BY seq")?
        .query_map([call], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let fragments = transcript::fragments(rows)?;
    let admitted = db.prepare("SELECT offset_ms,source FROM companion_delegations WHERE call_id=?1 AND state IN ('dispatched','completed','failed','unknown')")?
        .query_map([call], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut consumed = HashSet::new();
    let mut legacy_previous = -1;
    let mut previous = -1;
    for (anchor, saved) in admitted {
        previous = previous.max(anchor);
        if let Some(saved) = saved {
            let source: transcript::Source = serde_json::from_str(&saved)?;
            consumed.extend(source.fragments.iter().map(|f| f.seq));
        } else {
            // Preserve the old migration boundary: historical unbound speech
            // cannot acquire new authority after an upgrade.
            legacy_previous = legacy_previous.max(anchor);
        }
    }
    let source =
        transcript::Source::pending(&fragments, previous, legacy_previous, offset, &consumed);
    Ok((fragments, previous, source))
}

/// Called under the intake mutex, after provider reservations were considered.
/// The two-second quiet window coalesces fragment arrivals, not speech content.
pub(super) fn reserve(s: &AppState, call: &str, closing: bool) -> Result<bool> {
    let mut db = s.db.lock().unwrap();
    if !established(&db, call)? {
        return Ok(false);
    }
    let (_, _, mut pending) = source(&db, call, i64::MAX)?;
    pending
        .fragments
        .retain(|f| f.role == "user" && !f.text.trim().is_empty());
    if pending.fragments.is_empty() {
        return Ok(false);
    }
    let last_seq = pending.fragments.iter().map(|f| f.seq).max().unwrap();
    let observed_at: i64 = db.query_row(
        "SELECT at FROM companion_events WHERE call_id=?1 AND seq=?2",
        params![call, last_seq],
        |r| r.get(0),
    )?;
    if !closing && now_ms().saturating_sub(observed_at) < TRANSCRIPT_GRACE_MS {
        return Ok(false);
    }
    let offset = pending.fragments.iter().map(|f| f.end_ms).max().unwrap();
    pending.offset_ms = offset;
    // An oversized/unadmitted source stays held once; maintenance must not
    // allocate an endless series of identical attempts.
    let existing = db.prepare("SELECT id,source FROM companion_delegations WHERE call_id=?1 AND state IN ('waiting_transcript','held')")?
        .query_map([call], |r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, saved) in existing {
        if local(&db, call, &id)? {
            if let Some(saved) = saved {
                let prior: transcript::Source = serde_json::from_str(&saved)?;
                if prior.fragments == pending.fragments {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        }
    }
    let id = format!("companion-human:{}", uuid::Uuid::new_v4());
    let message = uuid::Uuid::new_v4().to_string();
    let tx = db.transaction()?;
    tx.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,?3,'waiting_transcript',?4,?5)",params![call,id,offset,message,now_ms()])?;
    insert_event(
        &tx,
        call,
        &format!("human-handoff:{id}"),
        "human_handoff",
        &json!({"id":id,"origin":"observed_human_continuation","state":"reserved","closing":closing,"partial":true,"offset_ms":offset,"fragment_seqs":pending.fragments.iter().map(|f|f.seq).collect::<Vec<_>>()}),
    )?;
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(true)
}

/// A later provider signal cannot dispatch human fragments already claimed by
/// a local handoff. Keep the link, including failed/unknown original outcomes;
/// neither a new provider ID nor media close authorizes replay.
pub(super) fn carry_covered(s: &AppState, call: &str, id: &str, offset: i64) -> Result<bool> {
    let mut db = s.db.lock().unwrap();
    if local(&db, call, id)? {
        return Ok(false);
    }
    let rows = db.prepare("SELECT id,message_id,source FROM companion_delegations WHERE call_id=?1 AND state IN ('dispatched','completed','failed','unknown') ORDER BY created_at DESC,id")?
        .query_map([call],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (prior, message, saved) in rows {
        if !local(&db, call, &prior)? {
            continue;
        }
        let Some(saved) = saved else {
            continue;
        };
        let source: transcript::Source = serde_json::from_str(&saved)?;
        if !source
            .fragments
            .iter()
            .any(|f| f.role == "user" && f.start_ms <= offset)
        {
            continue;
        }
        let tx = db.transaction()?;
        tx.execute("UPDATE companion_delegations SET state='carried_forward',message_id=?3 WHERE call_id=?1 AND id=?2 AND state IN ('waiting_transcript','held')",params![call,id,message])?;
        insert_event(
            &tx,
            call,
            &format!("delegation:{id}:carried-forward"),
            "delegation",
            &json!({"id":id,"state":"carried_forward","carried_by":prior,"request_id":message,"executed":false,"reason":"observed_human_source_already_claimed"}),
        )?;
        tx.commit()?;
        drop(db);
        s.changed();
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE companion_events(seq INTEGER PRIMARY KEY,call_id TEXT,event_key TEXT,kind TEXT,data TEXT);
            CREATE TABLE companion_delegations(call_id TEXT,id TEXT,offset_ms INTEGER,state TEXT,message_id TEXT,source TEXT);
            CREATE TABLE companion_calls(id TEXT,conversation_id TEXT,agent_id TEXT,owner TEXT);
            CREATE TABLE messages(id TEXT,conversation_id TEXT,role TEXT,actor TEXT);
            CREATE TABLE input_deliveries(message_id TEXT,conversation_id TEXT,agent_id TEXT);
            INSERT INTO companion_calls VALUES('call','room','agent','human');").unwrap();
        db
    }

    fn fragment(db: &Connection, seq: i64, start: i64, text: &str) {
        db.execute("INSERT INTO companion_events VALUES(?1,'call',?2,'transcript',?3)",params![seq,format!("event-{seq}"),json!({"role":"user","start_ms":start,"end_ms":start+100,"text":text,"event_id":format!("event-{seq}")}).to_string()]).unwrap();
    }

    #[test]
    fn late_fragment_is_new_source_despite_older_timing_anchor() {
        let db = db();
        fragment(&db, 1, 1000, "Original work");
        let (_, _, original) = source(&db, "call", 5000).unwrap();
        db.execute("INSERT INTO companion_delegations VALUES('call','provider',5000,'completed','first',?1)",[serde_json::to_string(&original).unwrap()]).unwrap();
        // Arrival identity, not timestamp alone, owns this correction.
        fragment(&db, 2, 1200, "Actually, use the other name");
        let (_, previous, late) = source(&db, "call", 5000).unwrap();
        assert_eq!(previous, 5000);
        assert_eq!(
            late.fragments.iter().map(|f| f.seq).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(late.label().contains("other name"));
    }

    #[test]
    fn uncertain_claim_is_never_available_to_a_second_handoff() {
        let db = db();
        fragment(&db, 1, 1000, "Change the existing work");
        let (_, _, claimed) = source(&db, "call", 1100).unwrap();
        db.execute("INSERT INTO companion_delegations VALUES('call','companion-human:1',1100,'unknown','lost-ack',?1)",[serde_json::to_string(&claimed).unwrap()]).unwrap();
        assert!(!source(&db, "call", i64::MAX).unwrap().2.human());
        fragment(&db, 2, 2000, "A later addition");
        assert_eq!(source(&db, "call", i64::MAX).unwrap().2.fragments[0].seq, 2);
    }

    #[test]
    fn local_origin_needs_its_private_receipt_not_a_provider_prefix() {
        let db = db();
        let id = "companion-human:forged";
        db.execute(
            "INSERT INTO companion_events VALUES(1,'call',?1,'provider',?2)",
            params![
                format!("human-handoff:{id}"),
                json!({"id":id,"origin":"observed_human_continuation"}).to_string()
            ],
        )
        .unwrap();
        assert!(!local(&db, "call", id).unwrap());
        db.execute("UPDATE companion_events SET kind='human_handoff'", [])
            .unwrap();
        assert!(local(&db, "call", id).unwrap());
        assert!(!local(&db, "other-call", id).unwrap());
    }

    #[test]
    fn provider_signal_without_matching_admitted_input_does_not_enable_followthrough() {
        let db = db();
        db.execute("INSERT INTO companion_delegations VALUES('call','provider',1000,'dispatched','message',NULL)",[]).unwrap();
        assert!(!established(&db, "call").unwrap());
        db.execute(
            "INSERT INTO messages VALUES('message','room','user','human');",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO input_deliveries VALUES('message','room','other-agent');",
            [],
        )
        .unwrap();
        assert!(!established(&db, "call").unwrap());
        db.execute("UPDATE input_deliveries SET agent_id='agent';", [])
            .unwrap();
        assert!(established(&db, "call").unwrap());
    }
}
