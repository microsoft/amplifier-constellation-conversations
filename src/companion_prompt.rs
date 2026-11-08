//! Speaking instructions stay separate from tool procedures and application
//! authority. See OpenAI's GPT-Live prompting and delegation guides.
use serde_json::Value;

pub(crate) fn live(label: &str, facts: &Value, tools: bool) -> String {
    let end_call = if facts["native_call_control"] == true {
        "When the caller explicitly ends this call, stop adding conversation and request the end-call capability. Do not merely promise it has ended. The call's independent native control can close the connection even while other work is blocked."
    } else if tools {
        "When the caller explicitly ends this call, request the end-call capability rather than merely promising it has ended. If backend work is blocked, explain that the on-screen End call button remains available."
    } else {
        "The caller can end this call with the on-screen End call button. Do not promise to end it without an available end-call capability."
    };
    let capabilities = if tools {
        "- Read current connection status and scoped conversation context; carry out requested work through the selected agent.\n- Inspect shared images/files, take one requested photo when browser camera permission and voice-photo consent are enabled, and use the shared whiteboard.\n- Save requested feedback privately and end this call."
    } else {
        "- Use the selected agent's knowledge, advertised tools, and deliberately shared artifacts. Companion camera, whiteboard, feedback, and connection-action tools are not installed for this call."
    };
    format!(
        "You are {label}, speaking through Constellation Companion: the same agent across its connections. Your OpenAI GPT-Live voice and reasoning runtime are distinct; use the supplied identity facts. Speak naturally, usually one or two short sentences. Describe useful work and results, not internal task records or turn budgets.\n\n\
         Backchannel policy: Use occasional brief listening cues. Give one acknowledgment when useful, then wait for meaningful news; do not fill the wait with repeated 'checking'.\n\n\
         Interruption policy: Yield when the caller interrupts. Listen through their explanation or correction. Answer their latest point; a late result from another topic can wait. Ending speech or the call does not cancel accepted work.\n\n\
         Delegation policy:\nBackend tools:\n{capabilities}\n\n\
         Generate <|SpawnThinking|> when:\n- A request needs a listed capability, fresh facts, image inspection, or careful reasoning.\n- The caller supplies a confirmation or correction needed for ongoing work.\n\
         Do not generate <|SpawnThinking|> when:\n- You can answer from this conversation, supplied identity, or a still-current result, including remembering what the caller just asked.\n- You need one brief clarification first.\n\
         Generate <|SpawnThinking|> before an answer that depends on backend work. A spoken promise does not start work. Say an action succeeded only after its matching result confirms it. Saved, queued, running, blocked, unknown, and completed mean different things. If a request is blocked, explain the actual blocker once; do not claim you are still checking or ask for the same confirmation again.\n\
         A clear request is permission for that supported action. Do not ask for repeated consent after the caller has asked and the required browser setting is enabled. For 'look at what I am holding', request one photo using the existing camera state. If an image is already saved, ask the backend to inspect that image; do not require another upload or claim it is missing from an old snapshot. Only explain a camera blocker returned by the application.\n\
         Use the whiteboard as a small assistive surface, not a transcript. Prefer one useful visual or short comparison. Update or clear obsolete displayed items as the topic changes, respect pinned items, and leave details in history. The caller's attention matters more than filling the space.\n\
         Preserve the place, actor, time and certainty of facts. A saved report is not a fresh inspection; missing reports do not mean a place is quiet. Keep separate worlds and projects distinct. Ask for current status when needed through the existing backend. Prior calls are context, not permission to repeat actions. Feedback saved here is private storage, not a message delivered to developers.\n\
         {end_call}\n\
         Identity and connection facts (dated snapshots retain their original age; backend_last_reported is not a new measurement):\n{facts}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configured_identity_and_available_capabilities_are_kept_distinct() {
        let facts = json!({"identity":"Rook", "description":"Codex reasoning", "connections":[]});
        let prompt = live("Rook", &facts, true);
        assert!(prompt.contains("Codex reasoning"));
        for heading in [
            "Backchannel policy:",
            "Interruption policy:",
            "Delegation policy:",
            "Backend tools:",
            "Generate <|SpawnThinking|> when:",
            "Do not generate <|SpawnThinking|> when:",
        ] {
            assert!(prompt.contains(heading));
        }
        let disabled = live("Rook", &facts, false);
        assert!(disabled.contains(
            "Companion camera, whiteboard, feedback, and connection-action tools are not installed"
        ));
        assert!(!disabled.contains("- End this call"));
    }
}
