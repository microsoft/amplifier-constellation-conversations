//! Bounded ACP text assembly. Message identity is not a chunk boundary: chunks
//! from different messages can interleave. Older unidentified clients retain
//! append behavior. A delivered answer selects the latest non-commentary message.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Default, Serialize, Deserialize)]
pub struct Answer {
    messages: Vec<Part>,
    after_tool: bool,
    bytes: usize,
}
#[derive(Serialize, Deserialize)]
struct Part {
    id: String,
    text: String,
    commentary: bool,
}
impl Answer {
    pub fn observe(&mut self, update: &Value) -> Result<()> {
        match update["sessionUpdate"].as_str() {
            Some("tool_call" | "tool_call_update") => {
                self.after_tool = true;
                return Ok(());
            }
            Some("agent_message_chunk") => {}
            _ => return Ok(()),
        }
        let Some(text) = update["content"]["text"].as_str() else {
            return Ok(());
        };
        let id = update["messageId"].as_str().unwrap_or("");
        ensure!(
            id.len() <= 1024 && self.bytes + text.len() + 2 <= 2 * 1024 * 1024,
            "ACP answer exceeds bounds"
        );
        let i = if let Some(i) = self.messages.iter().position(|p| p.id == id) {
            i
        } else {
            ensure!(self.messages.len() < 1024, "Too many ACP messages");
            self.messages.push(Part {
                id: id.into(),
                text: String::new(),
                commentary: false,
            });
            self.messages.len() - 1
        };
        let part = &mut self.messages[i];
        if let Some(phase) = update["_meta"]["com.microsoft.amplifier.message"]["phase"].as_str() {
            match phase {
                "commentary" => part.commentary = true,
                "final_answer" => part.commentary = false,
                _ => {}
            }
        }
        if id.is_empty() && self.after_tool && !part.text.is_empty() && !part.text.ends_with("\n\n")
        {
            part.text.push_str("\n\n");
            self.bytes += 2;
        }
        part.text.push_str(text);
        self.bytes += text.len();
        self.after_tool = false;
        Ok(())
    }
    pub fn text(&self) -> &str {
        self.messages
            .iter()
            .rev()
            .find(|p| !p.commentary && !p.text.is_empty())
            .or_else(|| self.messages.iter().rev().find(|p| !p.text.is_empty()))
            .map(|p| p.text.as_str())
            .unwrap_or("")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn chunk(id: &str, text: &str) -> Value {
        json!({"sessionUpdate":"agent_message_chunk","messageId":id,"content":{"type":"text","text":text}})
    }
    #[test]
    fn revised_silence_is_one_valid_answer_even_with_late_old_chunks() {
        let mut a = Answer::default();
        a.observe(&chunk(
            "a",
            r#"{"decision":"speak","text":"old","reason":"old"}"#,
        ))
        .unwrap();
        a.observe(&chunk(
            "b",
            r#"{"decision":"pass","reason":"peer answered"}"#,
        ))
        .unwrap();
        a.observe(&chunk("a", " ")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(a.text()).unwrap()["decision"],
            "pass"
        );
    }
    #[test]
    fn commentary_does_not_replace_final_and_legacy_still_appends() {
        let mut a = Answer::default();
        a.observe(&chunk("final", "Done")).unwrap();
        let mut u = chunk("comment", "Looking");
        u["_meta"]["com.microsoft.amplifier.message"]["phase"] = "commentary".into();
        a.observe(&u).unwrap();
        assert_eq!(a.text(), "Done");
        let mut a = Answer::default();
        a.observe(&chunk("", "Hi")).unwrap();
        a.observe(&chunk("", " there")).unwrap();
        assert_eq!(a.text(), "Hi there");
    }
}
