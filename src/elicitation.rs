//! Requester-private ACP forms. Values are returned only on an explicit decision.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

#[derive(Clone, Default)]
pub struct Elicitations(Arc<Mutex<HashMap<String, Pending>>>);
struct Pending {
    room: String,
    actor: String,
    value: Value,
    sender: oneshot::Sender<Value>,
}
pub struct Request {
    pub id: String,
    hub: Elicitations,
    receiver: oneshot::Receiver<Value>,
    timeout: Duration,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.hub.0.lock().unwrap().remove(&self.id);
    }
}
impl Request {
    pub async fn wait(mut self) -> Value {
        tokio::time::timeout(self.timeout, &mut self.receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_else(cancelled)
    }
}
pub fn cancelled() -> Value {
    json!({"action":"cancel"})
}
/// Keeps the transport readable while forms await a person. The agent can withdraw
/// a request, and dropping the enclosing turn removes every unanswered form.
#[derive(Default)]
pub struct Callbacks {
    tasks: tokio::task::JoinSet<(Value, Value)>,
    cancellations: HashMap<String, Option<oneshot::Sender<()>>>,
}
impl Callbacks {
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    pub fn start(&mut self, id: Value, request: Request) -> anyhow::Result<()> {
        anyhow::ensure!(
            id.is_string() || id.is_i64() || id.is_u64(),
            "Invalid callback identifier"
        );
        let key = id.to_string();
        anyhow::ensure!(
            self.tasks.len() < 16 && !self.cancellations.contains_key(&key),
            "Too many or duplicate form callbacks"
        );
        let (tx, rx) = oneshot::channel();
        self.cancellations.insert(key, Some(tx));
        self.tasks.spawn(async move {
            let result = tokio::select! {
                value = request.wait() => value,
                _ = rx => cancelled(),
            };
            (id, result)
        });
        Ok(())
    }
    pub fn cancel(&mut self, id: &Value) {
        if let Some(sender) = self
            .cancellations
            .get_mut(&id.to_string())
            .and_then(Option::take)
        {
            let _ = sender.send(());
        }
    }
    pub async fn completed(&mut self) -> anyhow::Result<(Value, Value)> {
        let (id, result) = self
            .tasks
            .join_next()
            .await
            .ok_or_else(|| anyhow::anyhow!("Missing form callback"))??;
        self.cancellations.remove(&id.to_string());
        Ok((id, result))
    }
}
fn fail(message: &'static str) -> Result<(), &'static str> {
    Err(message)
}
fn options(schema: &Value, key: &str) -> Option<Vec<String>> {
    schema[key].as_array().map(|a| {
        a.iter()
            .filter_map(|v| {
                if key == "enum" {
                    v.as_str()
                } else {
                    v["const"].as_str()
                }
            })
            .map(str::to_owned)
            .collect()
    })
}
fn enum_values(schema: &Value) -> Option<Vec<String>> {
    options(schema, "enum")
        .or_else(|| options(schema, "oneOf"))
        .or_else(|| options(schema, "anyOf"))
}
fn field_schema(schema: &Value, array_item: bool) -> Result<(), &'static str> {
    let object = schema.as_object().ok_or("Invalid form field")?;
    if object.keys().any(|k| {
        ![
            "type",
            "title",
            "description",
            "minLength",
            "maxLength",
            "format",
            "default",
            "minimum",
            "maximum",
            "enum",
            "enumNames",
            "oneOf",
            "anyOf",
            "items",
            "minItems",
            "maxItems",
        ]
        .contains(&k.as_str())
    }) {
        return fail("Unsupported form schema keyword");
    }
    let kind = schema["type"]
        .as_str()
        .unwrap_or(if array_item && schema["anyOf"].is_array() {
            "string"
        } else {
            ""
        });
    if !["string", "number", "integer", "boolean", "array"].contains(&kind)
        || (array_item && kind != "string")
    {
        return fail("Forms use flat primitive or enum fields");
    }
    for key in ["title", "description"] {
        if schema
            .get(key)
            .is_some_and(|v| !v.is_string() || v.as_str().unwrap().len() > 4096)
        {
            return fail("Invalid form field label");
        }
    }
    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if schema
            .get(key)
            .is_some_and(|v| v.as_u64().is_none() || v.as_u64().unwrap() > 16384)
        {
            return fail("Invalid form size limit");
        }
    }
    for key in ["minimum", "maximum"] {
        if schema.get(key).is_some_and(|v| v.as_f64().is_none()) {
            return fail("Invalid form number limit");
        }
    }
    if schema
        .get("format")
        .is_some_and(|v| !["email", "uri", "date", "date-time"].contains(&v.as_str().unwrap_or("")))
    {
        return fail("Unsupported form string format");
    }
    let choice_keys = ["enum", "oneOf", "anyOf"];
    if choice_keys
        .iter()
        .filter(|key| schema.get(**key).is_some())
        .count()
        > 1
        || choice_keys
            .iter()
            .any(|key| schema.get(*key).is_some_and(|v| !v.is_array()))
    {
        return fail("Invalid form choices");
    }
    if let Some(values) = enum_values(schema) {
        let count = schema["enum"]
            .as_array()
            .or_else(|| schema["oneOf"].as_array())
            .or_else(|| schema["anyOf"].as_array())
            .unwrap()
            .len();
        if values.is_empty()
            || values.len() > 100
            || values.len() != count
            || values.iter().any(|s| s.len() > 1000)
        {
            return fail("Invalid form choices");
        }
    }
    if kind == "array" {
        if array_item || enum_values(&schema["items"]).is_none() {
            return fail("Form arrays must contain enum choices");
        }
        field_schema(&schema["items"], true)?;
    }
    if let Some(default) = schema.get("default") {
        field_value(schema, default)?;
    }
    Ok(())
}
pub fn validate_request(params: &Value) -> Result<(), &'static str> {
    if params.to_string().len() > 65536
        || params["message"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 8192)
    {
        return fail("Invalid elicitation message");
    }
    if params["mode"] == "url" {
        let raw = params["url"].as_str().ok_or("Missing interaction URL")?;
        let url = reqwest::Url::parse(raw).map_err(|_| "Invalid interaction URL")?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if raw.len() > 8192
            || raw.chars().any(char::is_control)
            || !url.username().is_empty()
            || url.password().is_some()
            || !(url.scheme() == "https" || url.scheme() == "http" && local)
            || url.host_str().is_none()
            || params["elicitationId"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 1024)
            || params.get("requestedSchema").is_some()
        {
            return fail(
                "Use an HTTPS interaction URL without embedded credentials and a bounded request identifier",
            );
        }
        return Ok(());
    }
    if params["mode"] != "form" {
        return fail("Unsupported elicitation mode");
    }
    let schema = &params["requestedSchema"];
    if schema["type"] != "object" {
        return fail("Form schema must be an object");
    }
    if schema.as_object().unwrap().keys().any(|k| {
        ![
            "type",
            "properties",
            "required",
            "title",
            "description",
            "$schema",
            "additionalProperties",
        ]
        .contains(&k.as_str())
    }) || schema
        .get("additionalProperties")
        .is_some_and(|v| v != false)
    {
        return fail("Unsupported form schema keyword");
    }
    let properties = schema["properties"]
        .as_object()
        .ok_or("Missing form fields")?;
    if properties.len() > 24 {
        return fail("Too many form fields");
    }
    for (name, field) in properties {
        if name.is_empty()
            || name.len() > 100
            || ["__proto__", "constructor", "prototype"].contains(&name.as_str())
        {
            return fail("Invalid form field name");
        }
        let key = name.to_ascii_lowercase().replace(['_', '-', ' '], "");
        if [
            "password",
            "apikey",
            "accesstoken",
            "refreshtoken",
            "privatekey",
            "recoverycode",
            "cardnumber",
            "cvv",
            "secret",
        ]
        .iter()
        .any(|s| key.contains(s))
        {
            return fail("Secrets and credentials must not be collected in a form");
        }
        field_schema(field, false)?;
    }
    if let Some(required) = schema.get("required") {
        let required = required.as_array().ok_or("Invalid required fields")?;
        if required
            .iter()
            .any(|v| v.as_str().is_none_or(|k| !properties.contains_key(k)))
        {
            return fail("Unknown required form field");
        }
    }
    Ok(())
}
fn field_value(schema: &Value, value: &Value) -> Result<(), &'static str> {
    match schema["type"].as_str().unwrap_or("string") {
        "string" => {
            let value = value.as_str().ok_or("Expected text")?;
            let len = value.chars().count() as u64;
            if len < schema["minLength"].as_u64().unwrap_or(0)
                || len > schema["maxLength"].as_u64().unwrap_or(16384)
            {
                return fail("Text is outside the requested length");
            }
            if enum_values(schema).is_some_and(|options| !options.iter().any(|s| s == value)) {
                return fail("Select an offered choice");
            }
            let valid_format = match schema["format"].as_str() {
                Some("email") => value.rsplit_once('@').is_some_and(|(local, host)| {
                    !local.is_empty()
                        && !host.is_empty()
                        && !local.contains('@')
                        && !value.chars().any(char::is_whitespace)
                }),
                Some("uri") => reqwest::Url::parse(value).is_ok(),
                Some("date") => {
                    time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
                        .ok()
                        .is_some_and(|format| time::Date::parse(value, &format).is_ok())
                }
                Some("date-time") => time::OffsetDateTime::parse(
                    value,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok(),
                _ => true,
            };
            if !valid_format {
                return fail("Use the requested text format");
            }
        }
        "number" | "integer" => {
            let value = value.as_f64().ok_or("Expected a number")?;
            if (schema["type"] == "integer" && value.fract() != 0.0)
                || value < schema["minimum"].as_f64().unwrap_or(f64::NEG_INFINITY)
                || value > schema["maximum"].as_f64().unwrap_or(f64::INFINITY)
            {
                return fail("Number is outside the requested range");
            }
        }
        "boolean" => {
            if !value.is_boolean() {
                return fail("Expected true or false");
            }
        }
        "array" => {
            let values = value.as_array().ok_or("Expected selected choices")?;
            if (values.len() as u64) < schema["minItems"].as_u64().unwrap_or(0)
                || (values.len() as u64) > schema["maxItems"].as_u64().unwrap_or(100)
            {
                return fail("Select the requested number of choices");
            }
            for value in values {
                field_value(&schema["items"], value)?;
            }
        }
        _ => return fail("Unsupported form field"),
    }
    Ok(())
}
fn validate_content(schema: &Value, content: &Value) -> Result<(), &'static str> {
    if content.to_string().len() > 32768 {
        return fail("Form response is too large");
    }
    let content = content.as_object().ok_or("Expected form responses")?;
    let properties = schema["properties"].as_object().unwrap();
    for (name, value) in content {
        field_value(properties.get(name).ok_or("Unknown form response")?, value)?;
    }
    for name in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !content.contains_key(name) {
            return fail("Complete the required fields");
        }
    }
    Ok(())
}
impl Elicitations {
    pub fn request(
        &self,
        room: &str,
        actor: &str,
        agent: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Request, &'static str> {
        validate_request(params)?;
        let mut requests = self.0.lock().unwrap();
        requests
            .retain(|_, p| p.value["expires_at"].as_i64().unwrap_or(0) > crate::state::now_ms());
        if requests.len() >= 64 {
            return Err("Too many pending forms");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        let mut value = json!({"id":id,"conversation_id":room,"agent":agent,"mode":params["mode"],"message":params["message"],"schema":params["requestedSchema"],"expires_at":crate::state::now_ms()+timeout.as_millis() as i64});
        if params["mode"] == "url" {
            value["url"] = params["url"].clone();
            value["host"] = json!(
                reqwest::Url::parse(params["url"].as_str().unwrap())
                    .unwrap()
                    .host_str()
            );
            value["elicitation_id"] = params["elicitationId"].clone();
        }
        requests.insert(
            id.clone(),
            Pending {
                room: room.into(),
                actor: actor.into(),
                value,
                sender,
            },
        );
        Ok(Request {
            id,
            hub: self.clone(),
            receiver,
            timeout,
        })
    }
    pub fn list(&self, room: &str, actor: &str) -> Vec<Value> {
        let mut requests = self.0.lock().unwrap();
        requests
            .retain(|_, p| p.value["expires_at"].as_i64().unwrap_or(0) > crate::state::now_ms());
        requests
            .values()
            .filter(|p| p.room == room && p.actor == actor)
            .map(|p| p.value.clone())
            .collect()
    }
    pub fn resolve(
        &self,
        room: &str,
        actor: &str,
        id: &str,
        action: &str,
        content: Option<&Value>,
    ) -> Result<(), &'static str> {
        if !["accept", "decline", "cancel"].contains(&action) {
            return fail("Choose submit, decline or cancel");
        }
        let mut requests = self.0.lock().unwrap();
        let p = requests
            .get(id)
            .filter(|p| p.room == room && p.actor == actor)
            .ok_or("Form request not found")?;
        if p.value["expires_at"].as_i64().unwrap_or(0) <= crate::state::now_ms() {
            requests.remove(id);
            return fail("Form request expired");
        }
        if action == "accept" && p.value["mode"] != "url" {
            validate_content(&p.value["schema"], content.ok_or("Missing form response")?)?;
        }
        let p = requests.remove(id).unwrap();
        let mut result = json!({"action":action});
        if action == "accept" && p.value["mode"] != "url" {
            result["content"] = content.unwrap().clone();
        }
        p.sender
            .send(result)
            .map_err(|_| "Form is no longer active")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_values_check_formats_and_reject_unsupported_constraints() {
        for (format, good, bad) in [
            ("date", "2028-02-29", "2027-02-29"),
            ("date-time", "2026-11-02T09:00:00-08:00", "tomorrow morning"),
            ("email", "alex@example.test", "not an address"),
            ("uri", "https://example.test/path", "/relative"),
        ] {
            let field = json!({"type":"string","format":format});
            assert!(field_value(&field, &json!(good)).is_ok());
            assert!(field_value(&field, &json!(bad)).is_err());
        }
        let mut request = params();
        request["requestedSchema"]["allOf"] = json!([]);
        assert!(validate_request(&request).is_err());
        assert!(field_schema(&json!({"type":"string","enum":"invalid"}), false).is_err());
        assert!(field_schema(&json!({"type":"integer","minimum":1,"default":0}), false).is_err());
    }
    fn params() -> Value {
        json!({"mode":"form","message":"Choose approach","requestedSchema":{"type":"object","properties":{"approach":{"type":"string","enum":["small","large"]},"count":{"type":"integer","minimum":1,"maximum":3}},"required":["approach"]}})
    }
    #[test]
    fn external_interactions_require_safe_explicit_destinations() {
        let mut p = json!({"mode":"url","message":"Sign in","elicitationId":"login","url":"https://example.test/authorize"});
        for url in [
            "https://example.test/authorize?state=123",
            "http://127.0.0.1:8123/auth",
            "http://[::1]:8123/auth",
        ] {
            p["url"] = json!(url);
            assert!(validate_request(&p).is_ok());
        }
        for url in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,hi",
            "http://example.test",
            "https://user:password@example.test",
            "https://example.test/\npath",
        ] {
            p["url"] = json!(url);
            assert!(validate_request(&p).is_err(), "{url}");
        }
    }
    #[tokio::test]
    async fn private_forms_validate_and_require_an_explicit_single_use_decision() {
        let hub = Elicitations::default();
        let r = hub
            .request("room", "alice", "Agent", &params(), Duration::from_secs(30))
            .unwrap();
        assert!(hub.list("room", "bob").is_empty());
        assert!(
            hub.resolve(
                "room",
                "bob",
                &r.id,
                "accept",
                Some(&json!({"approach":"small"}))
            )
            .is_err()
        );
        assert!(
            hub.resolve("other", "alice", &r.id, "cancel", None)
                .is_err()
        );
        for content in [
            json!({}),
            json!({"approach":"unknown"}),
            json!({"approach":"small","count":4}),
            json!({"approach":"small","unexpected":"value"}),
        ] {
            assert!(
                hub.resolve("room", "alice", &r.id, "accept", Some(&content))
                    .is_err()
            );
        }
        hub.resolve(
            "room",
            "alice",
            &r.id,
            "accept",
            Some(&json!({"approach":"small","count":2})),
        )
        .unwrap();
        assert!(hub.resolve("room", "alice", &r.id, "cancel", None).is_err());
        assert_eq!(
            r.wait().await,
            json!({"action":"accept","content":{"approach":"small","count":2}})
        );
    }
    #[tokio::test]
    async fn decline_timeout_and_dropped_turn_never_return_form_data() {
        let hub = Elicitations::default();
        let r = hub
            .request("room", "alice", "Agent", &params(), Duration::from_secs(30))
            .unwrap();
        hub.resolve(
            "room",
            "alice",
            &r.id,
            "decline",
            Some(&json!({"secret":"ignored"})),
        )
        .unwrap();
        assert_eq!(r.wait().await, json!({"action":"decline"}));
        let r = hub
            .request(
                "room",
                "alice",
                "Agent",
                &params(),
                Duration::from_millis(1),
            )
            .unwrap();
        assert_eq!(r.wait().await, cancelled());
        let r = hub
            .request("room", "alice", "Agent", &params(), Duration::from_secs(30))
            .unwrap();
        let id = r.id.clone();
        drop(r);
        assert!(
            hub.resolve(
                "room",
                "alice",
                &id,
                "accept",
                Some(&json!({"approach":"small"}))
            )
            .is_err()
        );
    }
    #[test]
    fn form_schemas_reject_credentials_nested_objects_and_mixed_modes() {
        let mut p = params();
        p["mode"] = json!("url");
        assert!(validate_request(&p).is_err());
        p = params();
        p["requestedSchema"]["properties"]["api_key"] = json!({"type":"string"});
        assert!(validate_request(&p).is_err());
        p = params();
        p["requestedSchema"]["properties"]["nested"] = json!({"type":"object"});
        assert!(validate_request(&p).is_err());
    }
}
