//! Room facts for the optional Instance attention capability; no social evaluator here.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const META: &str = "com.microsoft.amplifier.attention";
pub const METHOD: &str = "_amplifier/attention";
pub fn supported(init: &Value) -> bool {
    init["_meta"][META]["version"] == 1
        && init["_meta"][META]["enabled"] == true
        && init["_meta"][META]["method"] == METHOD
}
pub fn evidence(
    room: &str,
    agent_id: &str,
    label: &str,
    trigger: &str,
    history: &[Value],
) -> Result<(Value, i64)> {
    let trigger_row = history
        .iter()
        .find(|m| m["id"] == trigger)
        .context("Trigger no longer present")?;
    let at = trigger_row["created_at"]
        .as_i64()
        .context("Missing source timestamp")?;
    let me = json!({"id":format!("agent:{agent_id}"),"name":label,"is_agent":true});
    let mut people = BTreeMap::from([(format!("agent:{agent_id}"), me.clone())]);
    let mut messages = Vec::new();
    for row in history
        .iter()
        .filter(|m| m["status"] == "complete")
        .rev()
        .take(24)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let agent = row["agent_id"].as_str();
        let actor = row["actor"].as_str().unwrap_or("unknown");
        let sender = agent
            .map(|id| format!("agent:{id}"))
            .unwrap_or_else(|| format!("human:{actor}"));
        people
            .entry(sender.clone())
            .or_insert(json!({"id":sender,"name":actor,"is_agent":agent.is_some()}));
        messages.push(json!({"id":row["id"],"sender":sender,"text":row["text"].as_str().unwrap_or("").chars().take(700).collect::<String>(),"recipients":row["target_agent_ids"].as_array().into_iter().flatten().filter_map(|id|id.as_str().map(|id|format!("agent:{id}"))).collect::<Vec<_>>()}));
    }
    Ok((
        json!({"conversation_id":room,"thread_id":"","agent":me,"participants":people.into_values().collect::<Vec<_>>(),"messages":messages,"trigger_id":trigger}),
        at,
    ))
}
