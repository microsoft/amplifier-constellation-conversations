//! Sanitized ACP failures. Never echo provider stderr, raw error data or auth URLs.
use serde_json::Value;

#[derive(Debug)]
pub struct AuthenticationRequired;
impl std::fmt::Display for AuthenticationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agent sign-in required. Sign in to the configured ACP agent on its host using the same service account, then reconnect. Supported agent login flows are offered during session setup. Terminal-only login must be completed on the agent host.")
    }
}
impl std::error::Error for AuthenticationRequired {}
pub fn rejected(error: &Value, method: &str) -> anyhow::Error {
    if error["code"] == -32000 {
        AuthenticationRequired.into()
    } else {
        anyhow::anyhow!(
            "ACP rejected {method} (code {}). Inspect the agent on its host before retrying work.",
            error["code"].as_i64().unwrap_or(-32603)
        )
    }
}
pub fn authentication_required(error: &anyhow::Error) -> bool {
    error.downcast_ref::<AuthenticationRequired>().is_some()
}
pub fn authentication(init: &Value) -> Value {
    let methods: Vec<Value> = init["authMethods"]
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .map(|m| {
            let text = |key: &str| {
                m[key]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect::<String>()
            };
            serde_json::json!({"id":text("id"),"name":text("name"),"type":text("type")})
        })
        .collect();
    serde_json::json!({"profile":"acp-interactive-auth/1","interactive":true,"methods":methods,"instructions":"Supported agent sign-in methods are offered when a session requires authentication. Choose a method, then explicitly open its provider page. Terminal-only methods require sign-in on the agent host; no agent-supplied command is executed."})
}
/// A client-owned choice, bound to the initiating person. Terminal invocations
/// are never accepted from an agent advertisement on this headless service.
pub fn choice(methods: &Value) -> anyhow::Result<Value> {
    let mut seen = std::collections::HashSet::new();
    let options: Vec<Value> = methods
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .filter_map(|m| {
            if m["type"].as_str().is_some_and(|t| t != "agent") {
                return None;
            }
            let id = m["id"].as_str()?;
            if id.is_empty()
                || id.len() > 200
                || id.chars().any(char::is_control)
                || !seen.insert(id.to_owned())
            {
                return None;
            }
            let name = m["name"]
                .as_str()
                .unwrap_or(id)
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect::<String>();
            Some(serde_json::json!({"const":id,"title":name}))
        })
        .collect();
    anyhow::ensure!(
        !options.is_empty(),
        "This agent needs host sign-in; it did not offer a supported ACP login method"
    );
    Ok(
        serde_json::json!({"mode":"form","message":"This agent requires sign-in. Choose how to continue. Credentials are entered only on the provider's own page.","requestedSchema":{"type":"object","properties":{"method_id":{"type":"string","title":"Sign-in method","oneOf":options}},"required":["method_id"]}}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_auth_required_becomes_actionable_without_echoing_payloads() {
        let err = rejected(
            &serde_json::json!({"code":-32000,"message":"secret-token","data":{"url":"https://evil.invalid"}}),
            "session/new",
        );
        assert!(authentication_required(&err));
        assert!(err.to_string().contains("same service account"));
        assert!(!err.to_string().contains("secret-token"));
        assert!(!authentication_required(&rejected(
            &serde_json::json!({"code":-32603}),
            "session/new"
        )));
        let info = authentication(
            &serde_json::json!({"authMethods":[{"id":"login","name":"Browser login","description":"secret","url":"https://example.invalid"}]}),
        );
        assert_eq!(info["methods"][0]["name"], "Browser login");
        assert!(!info.to_string().contains("secret"));
        assert!(info["methods"][0].get("url").is_none());
    }
}
