//! Public, observed ACP settings. Never infer a model from a launcher command,
//! endpoint name, profile description, or another conversation's private state.
use crate::state::{AppState, now_ms};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 240 && !s.chars().any(char::is_control))
}

pub(crate) fn project(initialize: &Value, setup: &Value) -> Value {
    let mut agent = json!({});
    for key in ["name", "title", "version"] {
        if let Some(value) = text(&initialize["agentInfo"][key]) {
            agent[key] = json!(value);
        }
    }
    let mut model = Value::Null;
    if let Some(id) = text(&setup["models"]["currentModelId"]) {
        let name = setup["models"]["availableModels"]
            .as_array()
            .into_iter()
            .flatten()
            .take(256)
            .find(|m| m["modelId"] == id)
            .and_then(|m| text(&m["name"]));
        model = json!({"id":id,"name":name.unwrap_or(id)});
    }
    let mut options = Vec::new();
    for option in setup["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .take(64)
    {
        let category = option["category"].as_str().unwrap_or("");
        if option["type"] != "select" || !["model", "thought_level"].contains(&category) {
            continue;
        }
        let (Some(id), Some(name), Some(current)) = (
            text(&option["id"]),
            text(&option["name"]),
            text(&option["currentValue"]),
        ) else {
            continue;
        };
        // Only an advertised selected value is a public setting. No arbitrary
        // metadata, credential fields, URLs, or free-form options are exposed.
        let selected = option["options"]
            .as_array()
            .into_iter()
            .flatten()
            .take(256)
            .find(|v| v["value"] == current)
            .and_then(|v| text(&v["name"]));
        let Some(selected) = selected else { continue };
        options.push(json!({"id":id,"name":name,"category":category,"value":current,"label":selected,"editable":false}));
        if category == "model" {
            model = json!({"id":current,"name":selected});
        }
    }
    json!({"agent":agent,"model":model,"options":options,"source":"acp_session_setup","observed_at":now_ms(),"capabilities":{"image_input":initialize["agentCapabilities"]["promptCapabilities"]["image"].as_bool(),"source":"acp_initialize","scope":"observed_session_snapshot"}})
}

/// Application-owned capability correction; no provider/user text is inserted.
pub(crate) fn image_capability_instruction(image: Option<bool>) -> &'static str {
    match image {
        Some(true) => {
            "This call's selected backend has now advertised image input support. Deliberately shared image bytes can be inspected through the backend. This does not mean any image has already been seen; use actual inspected findings for visual claims. This is a quiet capability update, not a request to speak."
        }
        Some(false) => {
            "This call's selected backend has now advertised that image input is unsupported. Do not promise to inspect photos or describe their pixels in this call. A file may still be saved or shown, which is not visual understanding. Offer help from the caller's description instead. This is a quiet capability update, not a request to speak."
        }
        None => {
            "Image input support for this call's selected backend remains unknown after its handshake. Do not infer support from installed tools, profile hints or another call. Do not promise image inspection until current capability and actual image delivery establish it. This is a quiet capability update, not a request to speak."
        }
    }
}

pub(crate) fn record(s: &AppState, room: &str, initialize: &Value, setup: &Value) -> Result<()> {
    let db = s.db.lock().unwrap();
    let companion: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM companion_calls WHERE conversation_id=?1)",
        [room],
        |r| r.get(0),
    )?;
    let mut info = initialize["agentInfo"].clone();
    if companion {
        if !info.is_object() {
            info = json!({});
        }
        info["companion_settings"] = project(initialize, setup);
    }
    db.execute(
        "UPDATE conversations SET agent_info=?2 WHERE id=?1",
        params![room, info.to_string()],
    )?;
    Ok(())
}

pub(crate) fn latest(s: &AppState, owner: &str, agent: &str) -> Result<Value> {
    let raw: Option<String> = s.db.lock().unwrap().query_row(
        "SELECT r.agent_info FROM companion_calls c JOIN conversations r ON r.id=c.conversation_id JOIN room_members m ON m.conversation_id=r.id AND m.username=c.owner JOIN room_agents a ON a.conversation_id=r.id AND a.agent_id=c.agent_id WHERE c.owner=?1 AND c.agent_id=?2 AND r.agent_info IS NOT NULL ORDER BY c.created_at DESC LIMIT 1",
        params![owner, agent], |r| r.get(0),
    ).optional()?;
    Ok(raw
        .and_then(|r| serde_json::from_str::<Value>(&r).ok())
        .and_then(|r| r.get("companion_settings").cloned())
        .unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_projection_is_typed_and_explicitly_a_session_snapshot() {
        for value in [json!(true), json!(false), Value::Null, json!("true")] {
            let projected = project(
                &json!({"agentCapabilities":{"promptCapabilities":{"image":value},"private":"not exposed"}}),
                &json!({}),
            );
            assert_eq!(
                projected["capabilities"]["image_input"],
                json!(value.as_bool())
            );
            assert_eq!(
                projected["capabilities"]["scope"],
                "observed_session_snapshot"
            );
            assert!(!projected.to_string().contains("not exposed"));
        }
        for supported in [None, Some(false), Some(true)] {
            assert!(image_capability_instruction(supported).len() <= 480);
        }
    }

    #[test]
    fn only_advertised_public_current_settings_are_reported() {
        let v = project(
            &json!({"agentInfo":{"name":"Native","version":"1","secret":"hidden"}}),
            &json!({
                "models":{"currentModelId":"model-a","availableModels":[{"modelId":"model-a","name":"Model A"}]},
                "configOptions":[
                    {"id":"effort","name":"Thinking","category":"thought_level","type":"select","currentValue":"low","options":[{"value":"low","name":"Low"}]},
                    {"id":"key","name":"Private key","type":"select","currentValue":"secret","options":[{"value":"secret","name":"secret"}]},
                    {"id":"model","name":"Model","category":"model","type":"select","currentValue":"unadvertised","options":[]}
                ]
            }),
        );
        assert_eq!(v["model"]["id"], "model-a");
        assert_eq!(v["options"].as_array().unwrap().len(), 1);
        assert_eq!(v["options"][0]["label"], "Low");
        assert!(!v.to_string().contains("hidden"));
        assert!(!v.to_string().contains("secret"));
        assert!(project(&json!({}), &json!({}))["model"].is_null());
    }
}
