//! Sanitized ACP failures. Never echo provider stderr, raw error data or auth URLs.
use serde_json::Value;

#[derive(Debug)]
pub struct AuthenticationRequired;
impl std::fmt::Display for AuthenticationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agent sign-in required. Sign in to the configured ACP agent on its host using the same service account, then reconnect. This experience does not run the agent's interactive sign-in flow; its advertised methods are available in the connection check.")
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
    serde_json::json!({"profile":"acp-preauthenticated/1","interactive":false,"methods":methods,"instructions":"Sign in to the configured ACP agent on its host using the same service account, then reconnect. No sign-in command or URL from the agent is executed automatically."})
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
