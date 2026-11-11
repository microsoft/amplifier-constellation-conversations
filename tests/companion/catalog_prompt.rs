//! Inspect the actual ACP prompt without asking a model to obey test prose.
use super::*;

#[tokio::test]
async fn actual_delegation_contains_authorized_executable_read_catalog_and_exact_source() {
    let f = fixture("delegate");
    std::fs::write(
        f.home.path().join("companion-actions.json"),
        json!({"enabled":true,"agents":{"default":[{"id":"minecraft-home","label":"Home","description":"Configured Home action route.","command":["/not-executed/private-action-secret"]}],"another-agent":[{"id":"another-agent-action","label":"Other","description":"Hidden.","command":["/not-executed/private-action-secret"]}]}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        f.home.path().join("companion-awareness.json"),
        json!({
            "enabled":true,
            "decision_command":["/not-executed/private-decision-secret"],
            "agents":{
                "default":[
                    {"id":"minecraft-constellation","label":"Constellation Workshop","command":["/not-executed/private-observer-secret"],"owners":["owner"]},
                    {"id":"minecraft-home","label":"Home","command":["/not-executed/private-observer-secret"],"owners":["owner"]},
                    {"id":"other-owner-world","label":"Private Other","command":["/not-executed/private-observer-secret"],"owners":["other"]}
                ],
                "another-agent":[
                    {"id":"another-agent-world","label":"Private Agent","command":["/not-executed/private-observer-secret"],"owners":["owner"]}
                ]
            }
        }).to_string(),
    )
    .unwrap();
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    until(&f, id, "card").await;
    let saved: String = f.s.db.lock().unwrap().query_row(
        "SELECT m.text FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id='delegate-1'",
        [id], |r| r.get(0),
    ).unwrap();
    let marker = "Configured Companion connection catalog (configured routes; current facts require a read): ";
    assert_eq!(saved.matches(marker).count(), 1);
    let catalog: Value = serde_json::Deserializer::from_str(saved.split_once(marker).unwrap().1)
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(catalog["connections"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["connections"][0]["id"], "minecraft-home");
    assert_eq!(
        catalog["action_tools"]["request"]["tool"],
        "companion_request_action"
    );
    assert_eq!(
        catalog["action_tools"]["status"]["tool"],
        "companion_action_status"
    );
    let rows = catalog["observation_sources"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for connection in ["minecraft-constellation", "minecraft-home"] {
        let row = rows.iter().find(|row| row["id"] == connection).unwrap();
        assert_eq!(row["connection_id"], connection);
        assert!(row.get("read_only").is_none());
        assert_eq!(row["reads"]["current"]["read_only"], true);
        assert_eq!(
            row["reads"]["current"]["tool"],
            "companion_connection_status"
        );
        assert_eq!(
            row["reads"]["current"]["arguments"],
            json!({"connection_id":connection,"view":"current"})
        );
        assert_eq!(row["reads"]["saved"]["tool"], "companion_observations");
        assert_eq!(
            row["reads"]["saved"]["arguments"],
            json!({"connection_id":connection})
        );
        assert_eq!(
            row["reads"]["reports"]["arguments"],
            json!({"connection_id":connection,"view":"reports"})
        );
    }
    for private in [
        "other-owner-world",
        "another-agent-world",
        "private-observer-secret",
        "private-decision-secret",
        "private-action-secret",
        "another-agent-action",
    ] {
        assert!(
            !saved.contains(private),
            "private catalog configuration leaked"
        );
    }
    let segments = current_human_segments(&saved);
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0]["text"],
        "Please inspect the supplied test context and report it."
    );
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let prompts: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|v: &Value| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    let text = prompts[0]["params"]["prompt"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(&saved),
        "catalog must reach the actual ACP prompt, not only a local record"
    );
    assert_eq!(
        text.matches("You are the same agent speaking with the caller.")
            .count(),
        1,
        "the accepted production response instruction reaches the actual prompt once"
    );
    assert!(text.contains("Return concise useful facts, actual blockers and next steps."));
    assert!(text.contains(
        "Compare each allowance or deadline with its usage and observation time separately"
    ));
    assert!(!text.contains("For routine questions, give the requested conclusion"));
    assert!(!text.contains("You are the same agent continuing this spoken conversation"));
    end(&f, id).await;
}

#[tokio::test]
async fn disabled_call_tools_do_not_advertise_an_executable_catalog() {
    let f = fixture("delegate");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    until(&f, id, "card").await;
    let saved: String = f.s.db.lock().unwrap().query_row(
        "SELECT m.text FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id='delegate-1'",
        [id], |r| r.get(0),
    ).unwrap();
    assert!(!saved.contains("Configured Companion connection catalog (configured routes;"));
    assert_eq!(
        current_human_segments(&saved)[0]["text"],
        "Please inspect the supplied test context and report it."
    );
    end(&f, id).await;
}
