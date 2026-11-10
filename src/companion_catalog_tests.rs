//! Disposable catalog and host-route boundaries; no provider or live source.
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

fn installed_tools(f: &Fixture) -> String {
    std::fs::write(
        f.home.path().join("companion-actions.json"),
        r#"{"enabled":true,"agents":{}}"#,
    )
    .unwrap();
    let grant = crate::companion_tools::create_mcp_config(&f.s, &f.id).unwrap();
    crate::companion_tools::record_mcp_port(&f.s, &f.id, 34567).unwrap();
    grant["token"].as_str().unwrap().to_owned()
}

async fn get(f: &Fixture, token: &str, suffix: &str) -> (StatusCode, Value) {
    let response = crate::server::router(f.s.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/companion/calls/{}/tools/{suffix}", f.id))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn follow_read(f: &Fixture, token: &str, read: &Value) -> (StatusCode, Value) {
    let path = match read["tool"].as_str().unwrap() {
        "companion_observations" => "observations",
        "companion_connection_status" => "connection-status",
        name => panic!("Unexpected read tool {name}"),
    };
    let mut url = reqwest::Url::parse(&format!("https://fixture.invalid/{path}")).unwrap();
    for (key, value) in read["arguments"].as_object().unwrap() {
        url.query_pairs_mut()
            .append_pair(key, value.as_str().unwrap());
    }
    get(f, token, &format!("{path}?{}", url.query().unwrap())).await
}

#[tokio::test]
async fn catalog_and_discovery_share_exact_owner_scoped_read_arguments_without_queries() {
    let f = fixture();
    let token = installed_tools(&f);
    rewrite_policy(&f, |v| {
        let mut second = v["agents"]["default"][0].clone();
        second["id"] = json!("world:workshop / east");
        second["label"] = json!("Second workshop");
        let mut private = second.clone();
        private["id"] = json!("private-owner-source");
        private["owners"] = json!(["other"]);
        let mut other_agent = second.clone();
        other_agent["id"] = json!("other-agent-source");
        v["agents"]["default"]
            .as_array_mut()
            .unwrap()
            .extend([second, private]);
        v["agents"]["another"] = json!([other_agent]);
    });
    let catalog = crate::companion_tools::read_catalog(&f.s, &f.id)
        .unwrap()
        .unwrap();
    assert!(catalog["observed_at"].as_i64().unwrap() <= now_ms());
    let sources = catalog["observation_sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    for (source, expected_id) in sources.iter().zip(["minecraft", "world:workshop / east"]) {
        assert_eq!(source["connection_id"], expected_id);
        assert_eq!(source["id"], expected_id);
        assert_eq!(
            source["reads"]["saved"],
            json!({"tool":"companion_observations","arguments":{"connection_id":expected_id}})
        );
        assert_eq!(
            source["reads"]["current"],
            json!({"tool":"companion_connection_status","arguments":{"connection_id":expected_id,"view":"current"}})
        );
        assert_eq!(
            source["reads"]["reports"],
            json!({"tool":"companion_connection_status","arguments":{"connection_id":expected_id,"view":"reports"}})
        );
        assert!(source.get("availability").is_none());
        assert!(source.get("authority").is_none());
    }
    let (status, discovered) = get(&f, &token, "connections").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        discovered["observation_sources"],
        catalog["observation_sources"]
    );
    assert_eq!(discovered["connections"], json!([]));
    for private in [
        "private-owner-source",
        "other-agent-source",
        "observer.py",
        "decision.py",
        "owners",
        "audiences",
        &token,
    ] {
        assert!(!catalog.to_string().contains(private), "{private}");
        assert!(!discovered.to_string().contains(private), "{private}");
    }
    assert!(calls(&f, "observer").is_empty());
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn catalog_arguments_execute_scoped_reads_and_preserve_reports_default() {
    let f = fixture();
    let token = installed_tools(&f);
    let mut presence = source_event();
    presence["id"] = json!("current-position");
    presence["kind"] = json!("presence_observation");
    presence["actor"] = json!("Coach");
    presence["provenance"] = json!({"kind":"minecraft_public_presence"});
    presence["text"] =
        json!(r#"{"position":{"x":3,"y":64,"z":7},"ready":true,"busy":false,"turn_active":false}"#);
    let mut history = source_event();
    history["text"] = json!("An older chat report unrelated to current position.");
    let mut hidden = history.clone();
    hidden["id"] = json!("private-report");
    hidden["audience"] = json!({"kind":"private","owner":"other"});
    let canonical = fresh_result(&f, vec![presence.clone(), history.clone(), hidden]);
    let catalog = crate::companion_tools::read_catalog(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let reads = &catalog["observation_sources"][0]["reads"];
    let (status, saved) = follow_read(&f, &token, &reads["saved"]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["sources"][0]["connection_id"], "minecraft");
    assert!(calls(&f, "observer").is_empty());
    let (status, current) = follow_read(&f, &token, &reads["current"]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["reports"].as_array().unwrap().len(), 1);
    assert_eq!(current["reports"][0]["id"], presence["id"]);
    assert_eq!(current["reports"][0]["text"], presence["text"]);
    assert_eq!(
        current["reports"][0]["attribution"]["occurred_at"],
        presence["occurred_at"]
    );
    assert_eq!(current["selection"]["view"], "current");
    let (status, reports) = follow_read(&f, &token, &reads["reports"]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reports["reports"].as_array().unwrap().len(), 2);
    assert!(
        reports
            .to_string()
            .contains(history["text"].as_str().unwrap())
    );
    assert!(!reports.to_string().contains("private-report"));
    let (status, default) = get(&f, &token, "connection-status?connection_id=minecraft").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(default["reports"].as_array().unwrap().len(), 2);
    assert!(default.get("selection").is_none());
    let (status, wrong_namespace) = get(
        &f,
        &token,
        "connection-status?connection_id=minecraft%3Aprovider%3Afixture&view=current",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wrong_namespace["status"], "not_configured");
    assert_eq!(calls(&f, "observer").len(), 3);
    assert!(
        calls(&f, "observer")
            .iter()
            .all(|request| request == &json!({"version":1,"cursor":null,"actions":[]}))
    );
    assert!(calls(&f, "decision").is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(f.home.path().join("observer-result.json")).unwrap()
        )
        .unwrap(),
        canonical
    );
    rewrite_policy(&f, |v| {
        v["agents"]["default"][0]["owners"] = json!(["other"])
    });
    let (_, revoked) = follow_read(&f, &token, &reads["current"]).await;
    assert_eq!(revoked["status"], "not_configured");
    assert_eq!(calls(&f, "observer").len(), 3);
    assert!(
        crate::companion_tools::read_catalog(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn catalog_refreshes_configuration_and_requires_current_installed_private_scope() {
    for guard in [
        "disabled_tools",
        "missing_listener",
        "closed",
        "private_room",
        "stale_heartbeat",
        "disabled_sources",
    ] {
        let f = fixture();
        installed_tools(&f);
        let original = crate::companion_tools::read_catalog(&f.s, &f.id)
            .unwrap()
            .unwrap();
        rewrite_policy(&f, |v| {
            v["agents"]["default"][0]["id"] = json!("new-current-source")
        });
        let fresh = crate::companion_tools::read_catalog(&f.s, &f.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            original["observation_sources"][0]["connection_id"],
            "minecraft"
        );
        assert_eq!(
            fresh["observation_sources"][0]["reads"]["current"]["arguments"]["connection_id"],
            "new-current-source"
        );
        match guard {
            "disabled_tools" => std::fs::write(
                f.home.path().join("companion-actions.json"),
                r#"{"enabled":false}"#,
            )
            .unwrap(),
            "missing_listener" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE companion_tool_grants SET port=NULL", [])
                    .unwrap();
            }
            "closed" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE companion_calls SET state='closed'", [])
                    .unwrap();
            }
            "private_room" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "INSERT INTO room_members VALUES(?1,'other','member',?2)",
                        params![f.room, now_ms()],
                    )
                    .unwrap();
            }
            "stale_heartbeat" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE companion_calls SET heartbeat_at=0", [])
                    .unwrap();
            }
            "disabled_sources" => rewrite_policy(&f, |v| v["enabled"] = json!(false)),
            _ => unreachable!(),
        }
        assert!(
            crate::companion_tools::read_catalog(&f.s, &f.id)
                .unwrap()
                .is_none(),
            "{guard}"
        );
        assert!(calls(&f, "observer").is_empty());
        assert!(calls(&f, "decision").is_empty());
    }
}
