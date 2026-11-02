//! Volatile, single-use ACP permission requests. No decision survives a process restart.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

#[derive(Clone, Default)]
pub struct Permissions(Arc<Mutex<HashMap<String, Pending>>>);
struct Pending {
    room: String,
    actor: String,
    expires_at: i64,
    value: Value,
    sender: oneshot::Sender<Value>,
}
pub struct Request {
    pub id: String,
    hub: Permissions,
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
    json!({"outcome":{"outcome":"cancelled"}})
}
impl Permissions {
    pub fn request(
        &self,
        room: &str,
        actor: &str,
        agent: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Request, &'static str> {
        let options = params["options"]
            .as_array()
            .ok_or("Missing permission options")?;
        if options.is_empty()
            || options.len() > 16
            || params.to_string().len() > 65536
            || options.iter().any(|o| {
                o["optionId"]
                    .as_str()
                    .is_none_or(|s| s.is_empty() || s.len() > 256)
                    || o["name"]
                        .as_str()
                        .is_none_or(|s| s.is_empty() || s.len() > 512)
                    || !["allow_once", "allow_always", "reject_once", "reject_always"]
                        .contains(&o["kind"].as_str().unwrap_or(""))
            })
            || options
                .iter()
                .filter_map(|o| o["optionId"].as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
                != options.len()
        {
            return Err("Invalid permission options");
        }
        let mut requests = self.0.lock().unwrap();
        requests.retain(|_, p| p.expires_at > crate::state::now_ms());
        if requests.len() >= 64 {
            return Err("Too many pending permission requests");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let expires_at = crate::state::now_ms() + timeout.as_millis() as i64;
        // Show the requested action only to its requester; never audit its arguments.
        let mut tool = json!({});
        for key in [
            "toolCallId",
            "title",
            "kind",
            "status",
            "rawInput",
            "locations",
            "content",
        ] {
            if let Some(v) = params["toolCall"].get(key) {
                tool[key] = v.clone();
            }
        }
        let value = json!({"id":id,"conversation_id":room,"agent":agent,"tool_call":tool,"options":options,"expires_at":expires_at});
        let (sender, receiver) = oneshot::channel();
        requests.insert(
            id.clone(),
            Pending {
                room: room.into(),
                actor: actor.into(),
                expires_at,
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
        requests.retain(|_, p| p.expires_at > crate::state::now_ms());
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
        option: Option<&str>,
    ) -> Result<Option<String>, &'static str> {
        let mut requests = self.0.lock().unwrap();
        let p = requests
            .get(id)
            .filter(|p| p.room == room && p.actor == actor)
            .ok_or("Permission request not found")?;
        if p.expires_at <= crate::state::now_ms() {
            requests.remove(id);
            return Err("Permission request expired");
        }
        if let Some(option) = option
            && !p.value["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["optionId"] == option)
        {
            return Err("Select an option offered by the agent");
        }
        let p = requests.remove(id).unwrap();
        let kind = option.and_then(|option| {
            p.value["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["optionId"] == option)
                .and_then(|o| o["kind"].as_str())
                .map(str::to_owned)
        });
        let outcome = option
            .map(|id| json!({"outcome":{"outcome":"selected","optionId":id}}))
            .unwrap_or_else(cancelled);
        p.sender
            .send(outcome)
            .map_err(|_| "Permission request is no longer active")?;
        Ok(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn params() -> Value {
        json!({"toolCall":{"title":"Write file","rawInput":{"path":"test.txt"}},"options":[{"optionId":"yes","name":"Allow once","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]})
    }
    #[tokio::test]
    async fn permissions_are_actor_and_room_bound_single_use_and_offered_options_only() {
        let hub = Permissions::default();
        let request = hub
            .request("room", "alice", "agent", &params(), Duration::from_secs(10))
            .unwrap();
        assert!(hub.list("room", "bob").is_empty());
        assert!(
            hub.resolve("other", "alice", &request.id, Some("yes"))
                .is_err()
        );
        assert!(
            hub.resolve("room", "bob", &request.id, Some("yes"))
                .is_err()
        );
        assert!(
            hub.resolve("room", "alice", &request.id, Some("invented"))
                .is_err()
        );
        hub.resolve("room", "alice", &request.id, Some("yes"))
            .unwrap();
        assert!(
            hub.resolve("room", "alice", &request.id, Some("yes"))
                .is_err()
        );
        assert_eq!(request.wait().await["outcome"]["optionId"], "yes");
    }
    #[tokio::test]
    async fn timeout_cancel_and_dropped_turn_all_fail_closed() {
        let hub = Permissions::default();
        let request = hub
            .request(
                "room",
                "alice",
                "agent",
                &params(),
                Duration::from_millis(1),
            )
            .unwrap();
        assert_eq!(request.wait().await, cancelled());
        assert!(hub.list("room", "alice").is_empty());
        let request = hub
            .request("room", "alice", "agent", &params(), Duration::from_secs(10))
            .unwrap();
        let id = request.id.clone();
        drop(request);
        assert!(hub.resolve("room", "alice", &id, Some("yes")).is_err());
        let request = hub
            .request("room", "alice", "agent", &params(), Duration::from_secs(10))
            .unwrap();
        hub.resolve("room", "alice", &request.id, None).unwrap();
        assert_eq!(request.wait().await, cancelled());
    }
}
