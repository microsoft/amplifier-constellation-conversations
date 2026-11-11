//! Real worker/child-process boundaries with disposable source and model fixtures.
//! No API, agent engine, game operation, Slack message, or real credentials.
use super::*;
use crate::config::{Agent, Config as AppConfig};
use axum::http::HeaderMap;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    s: Arc<AppState>,
    id: String,
    room: String,
    headers: HeaderMap,
}

// Existing reader tests inspect the same public filtered tool response. The
// separate host binding never crosses the MCP response boundary.
async fn connection_status(s: &AppState, id: &str, connection: &str) -> Result<Value> {
    Ok(
        super::connection_status(s, id, connection, StatusView::Reports)
            .await?
            .response,
    )
}

fn timestamp(at: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(at / 1000).unwrap();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}
fn source_event() -> Value {
    json!({"id":"source-event-1","revision":"r1","kind":"work_report","text":"The roof was reported complete; independent verification remains pending.","occurred_at":timestamp(now_ms()),"audience":{"kind":"channel","id":"workshop-public"},"provenance":{"transport":"fixture","source_id":"source-event-1"}})
}
fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    AppConfig {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Awareness fixture".into(),
        owner: "owner".into(),
        allowed_users: vec!["other".into()],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://example.test:18445".into(),
        pam_service: "agent-conversations".into(),
        tls_cert: home.path().join("cert"),
        tls_key: home.path().join("key"),
        tls_ca: None,
        import_source: "fixture".into(),
        import_surface: "fixture".into(),
        agent: Agent {
            label: "Coach".into(),
            command: "/bin/true".into(),
            args: vec![],
            cwd: home.path().to_string_lossy().into(),
            session_cwd: None,
            env: BTreeMap::new(),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        },
    }
    .save(home.path())
    .unwrap();
    std::fs::write(
        home.path().join("observer.py"),
        r#"import json,pathlib,sys,time
p=pathlib.Path(__file__).parent
body=json.load(sys.stdin)
with (p/'observer-calls.jsonl').open('a') as f:f.write(json.dumps(body)+'\n')
(p/'observer-entered').write_text('entered')
while (p/'observer-block').exists():time.sleep(0.005)
print((p/'observer-result.json').read_text())
"#,
    )
    .unwrap();
    std::fs::write(home.path().join("decision.py"),r#"import json,pathlib,sys,time
p=pathlib.Path(__file__).parent
body=json.load(sys.stdin)
with (p/'decision-calls.jsonl').open('a') as f:f.write(json.dumps(body)+'\n')
(p/'decision-entered').write_text('entered')
while (p/'decision-block').exists():time.sleep(0.005)
mode=(p/'decision-mode').read_text() if (p/'decision-mode').exists() else 'quiet'
if mode=='fail':sys.exit(1)
result={'version':1,'request_id':body['request_id'],'event_id':body['event']['id'],'status':'decided','decision':'notify' if mode=='notify' else 'quiet','provider_choice':'notify' if mode=='notify' else 'quiet','confidence':0.95,'selected_probability':0.95,'model':'gpt-6-luna','reason':'provider_decision','model_requests':1}
if mode=='low-confidence':result.update(decision='later',provider_choice='quiet',confidence=0.19,selected_probability=0.39,reason='low_confidence')
if mode.startswith('predicate-'):
 for key in ['confidence','selected_probability','provider_choice']:result.pop(key)
 result.update(assessment={'kind':'relevance_and_proactive_v1','relevant_probability':0.95,'proactive_probability':0.5},decision='quiet',reason='relevant_context')
 if mode=='predicate-notify':result['assessment']['proactive_probability']=0.95;result.update(decision='notify',reason='relevant_proactive')
 if mode=='predicate-uncertain':result['assessment']['relevant_probability']=0.5;result['reason']='uncertain_relevance'
 if mode=='predicate-malformed':result['assessment']['relevant_probability']='yes'
 if mode=='predicate-mismatch':result.update(decision='notify',reason='relevant_proactive')
 if mode=='predicate-refused':result.update(status='fallback',assessment=None,reason='provider_refusal')
if mode=='wrong-request':result['request_id']='another-request'
if mode=='wrong-event':result['event_id']='another-event'
if mode=='wrong-model':result['model']='different-model'
if mode=='bad-confidence':result['confidence']=2.0;result['decision']='notify';result['provider_choice']='notify'
if mode=='refused':result['status']='fallback';result['decision']='later';result['reason']='provider_refusal'
print(json.dumps(result))
"#).unwrap();
    std::fs::write(home.path().join("companion-awareness.json"),json!({"enabled":true,"decision_command":["/usr/bin/python3",home.path().join("decision.py")],"guidance":"Quiet progress; do not interrupt a person.","agents":{"default":[{"id":"minecraft","label":"Workshop","command":["/usr/bin/python3",home.path().join("observer.py")],"owners":["owner"],"audiences":[{"kind":"channel","id":"workshop-public"}]}]}}).to_string()).unwrap();
    let s = AppState::open(home.path().into()).unwrap();
    let room = s.create("Private awareness fixture").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = uuid::Uuid::new_v4().to_string();
    let fingerprint =
        auth::digest(&serde_json::to_string(&s.agent("default").unwrap().unwrap()).unwrap());
    s.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?5,?6)",params![id,uuid::Uuid::new_v4().to_string(),room,now_ms()-5000,now_ms(),fingerprint]).unwrap();
    let mut headers = HeaderMap::new();
    let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    headers.insert(
        "authorization",
        format!("Bearer {}", token.trim()).parse().unwrap(),
    );
    let f = Fixture {
        home,
        s,
        id,
        room,
        headers,
    };
    observations(&f, vec![source_event()]);
    // Most worker fixtures explicitly model an authenticated quiet browser.
    // Unknown/stale/speaking cases remove or change this observation themselves.
    f.s.db.lock().unwrap().execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at) VALUES(?1,'browser','fixture',0,0,0,0,?2)",params![f.id,now_ms()]).unwrap();
    f
}
fn observations(f: &Fixture, events: Vec<Value>) {
    std::fs::write(f.home.path().join("observer-result.json"),json!({"version":1,"connection_id":"minecraft","availability":"available","events":events,"actions":[],"cursor":{"sequence":1},"coverage":{"scope":"fixture","complete":false}}).to_string()).unwrap();
}
fn fresh_result(f: &Fixture, events: Vec<Value>) -> Value {
    let result = json!({"version":1,"connection_id":"minecraft","availability":"available","observed_at":timestamp(now_ms()),"events":events,"actions":[],"cursor":{"secret":"private-cursor"},"coverage":{"bounded":true,"gap":false,"baseline":true,"private_token":"private-coverage-token"},"presence":{"ready":true,"busy":false,"turn_active":false,"token":"private-presence-token"},"token":"private-top-level-token"});
    std::fs::write(
        f.home.path().join("observer-result.json"),
        result.to_string(),
    )
    .unwrap();
    result
}

#[tokio::test]
async fn fresh_status_reads_only_fixed_source_and_projects_current_access_without_hidden_work() {
    let f = fixture();
    let mut old = source_event();
    old["actor"] = json!("Rook");
    old["occurred_at"] = json!(timestamp(now_ms() - 180_000));
    old["provenance"]["token"] = json!("private-provenance-token");
    old["raw_secret"] = json!("private-event-token");
    let mut private = source_event();
    private["audience"] = json!({"kind":"private","owner":"another-user"});
    private["text"] = json!("Unrelated private report");
    fresh_result(&f, vec![old.clone(), private]);
    let response = connection_status(&f.s, &f.id, "minecraft").await.unwrap();
    assert_eq!(response["status"], "available");
    assert_eq!(response["read_only"], true);
    assert_eq!(response["authority"], false);
    assert_eq!(response["reports"].as_array().unwrap().len(), 1);
    assert_eq!(response["reports"][0]["text"], old["text"]);
    assert_eq!(
        response["reports"][0]["attribution"]["occurred_at"],
        old["occurred_at"]
    );
    assert_eq!(response["reports"][0]["attribution"]["actor"], "Rook");
    assert!(response["reports"][0]["source_age_ms"].as_i64().unwrap() >= 180_000);
    assert_eq!(
        response["presence"],
        json!({"ready":true,"busy":false,"turn_active":false})
    );
    assert!(!response.to_string().contains("private-"));
    assert!(!response.to_string().contains("Unrelated private report"));
    assert_eq!(
        calls(&f, "observer"),
        vec![json!({"version":1,"cursor":null,"actions":[]})]
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(states(&f).is_empty());
    assert!(cards(&f).is_empty());
    assert_eq!(
        connection_status(&f.s, &f.id, "unenrolled").await.unwrap()["status"],
        "not_configured"
    );
    assert_eq!(calls(&f, "observer").len(), 1);
    assert_eq!(
        status_sources(&f.s, &f.id)
            .unwrap()
            .iter()
            .map(|source| json!({"id":source["id"],"label":source["label"],"read_only":source["read_only"]}))
            .collect::<Vec<_>>(),
        vec![json!({"id":"minecraft","label":"Workshop","read_only":true})]
    );
    rewrite_policy(&f, |v| {
        v["agents"]["default"][0]["owners"] = json!(["other"])
    });
    assert!(status_sources(&f.s, &f.id).unwrap().is_empty());
    assert_eq!(
        connection_status(&f.s, &f.id, "minecraft").await.unwrap()["status"],
        "not_configured"
    );
    assert_eq!(calls(&f, "observer").len(), 1);
}

#[path = "companion_catalog_tests.rs"]
mod catalog;

#[tokio::test]
async fn fresh_status_withholds_inflight_results_after_policy_identity_or_call_revocation() {
    for (mode, view) in ["policy", "identity", "closed"]
        .into_iter()
        .flat_map(|mode| [StatusView::Reports, StatusView::Current].map(|view| (mode, view)))
    {
        let f = fixture();
        fresh_result(&f, vec![source_event()]);
        std::fs::write(f.home.path().join("observer-block"), "hold").unwrap();
        let state = f.s.clone();
        let id = f.id.clone();
        let pending =
            tokio::spawn(
                async move { super::connection_status(&state, &id, "minecraft", view).await },
            );
        entered(&f, "observer").await;
        match mode {
            "policy" => rewrite_policy(&f, |v| v["agents"]["default"][0]["audiences"] = json!([])),
            "identity" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET endpoint_hash='changed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            _ => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET state='closed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
        }
        std::fs::remove_file(f.home.path().join("observer-block")).unwrap();
        assert!(pending.await.unwrap().is_err(), "{mode}");
        assert!(calls(&f, "decision").is_empty());
        assert!(states(&f).is_empty());
    }
}

#[tokio::test]
async fn fresh_status_rejects_stale_wrong_source_or_action_payloads_without_echoing_helper_secrets()
{
    let f = fixture();
    for mode in ["stale", "identity", "actions", "unavailable", "malformed"] {
        let mut result = fresh_result(&f, vec![source_event()]);
        match mode {
            "stale" => result["observed_at"] = json!(timestamp(now_ms() - 60_000)),
            "identity" => result["connection_id"] = json!("another-connection"),
            "actions" => {
                result["actions"] =
                    json!([{"request_id":"unrequested","token":"private-action-token"}])
            }
            "unavailable" => result["availability"] = json!("unavailable"),
            _ => result["events"] = json!([{"token":"private-malformed-token"}]),
        }
        std::fs::write(
            f.home.path().join("observer-result.json"),
            result.to_string(),
        )
        .unwrap();
        let response = connection_status(&f.s, &f.id, "minecraft").await.unwrap();
        assert_eq!(response["status"], "unavailable", "{mode}");
        assert_eq!(response["reports"], json!([]));
        assert!(!response.to_string().contains("private-"));
    }
    assert!(calls(&f, "decision").is_empty());
    assert!(states(&f).is_empty());
}
fn calls(f: &Fixture, name: &str) -> Vec<Value> {
    std::fs::read_to_string(f.home.path().join(format!("{name}-calls.jsonl")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn cards(f: &Fixture) -> Vec<Value> {
    let db = f.s.db.lock().unwrap();
    let mut st = db
        .prepare("SELECT data FROM companion_events WHERE call_id=?1 AND kind='card' ORDER BY seq")
        .unwrap();
    st.query_map([&f.id], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}
fn states(f: &Fixture) -> Vec<String> {
    let db = f.s.db.lock().unwrap();
    let mut st = db
        .prepare(
            "SELECT state FROM companion_awareness WHERE call_id=?1 ORDER BY created_at,event_key",
        )
        .unwrap();
    st.query_map([&f.id], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}
fn rewrite_policy(f: &Fixture, change: impl FnOnce(&mut Value)) {
    let p = f.home.path().join("companion-awareness.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    change(&mut v);
    std::fs::write(p, v.to_string()).unwrap();
}
fn quiet_browser(f: &Fixture) -> tokio::sync::mpsc::Receiver<Value> {
    f.s.db.lock().unwrap().execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at) VALUES(?1,'browser','fixture',0,0,0,0,?2) ON CONFLICT(call_id) DO UPDATE SET user_speaking=0,agent_speaking=0,updated_at=excluded.updated_at",params![f.id,now_ms()]).unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    crate::companion::test_control(&f.s, &f.id, tx);
    rx
}
async fn entered(f: &Fixture, name: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f.home.path().join(format!("{name}-entered")).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn run(f: &Fixture) -> Result<()> {
    tick(&f.s, &f.id, &f.headers).await?;
    let sender = f.s.companion.controls.lock().unwrap().get(&f.id).cloned();
    if let Some(sender) = sender {
        while let Some(batch) = crate::companion_delivery::take_ready(&f.s, &f.id)? {
            for command in batch["commands"].as_array().unwrap() {
                let _ = sender.try_send(command.clone());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn worker_deduplicates_before_paid_subprocess_and_refuses_changed_immutable_source() {
    let f = fixture();
    run(&f).await.unwrap();
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "observer").len(), 2);
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(cards(&f).len(), 1);
    let paid = calls(&f, "decision")[0].clone();
    assert_eq!(paid["event"]["revision"], "r1");
    assert_eq!(paid["context"]["user_speaking"], false);
    let mut event = source_event();
    event["text"] = json!("Changed content under unchanged revision");
    observations(&f, vec![event.clone()]);
    let _ = run(&f).await;
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(cards(&f).len(), 1);
    event["revision"] = json!("r2");
    observations(&f, vec![event]);
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 2);
}

#[tokio::test]
async fn unavailable_source_does_not_starve_other_connections_or_ready_context() {
    let f = fixture();
    rewrite_policy(&f, |v| {
        let mut unavailable = v["agents"]["default"][0].clone();
        unavailable["id"] = json!("offline");
        unavailable["label"] = json!("Offline connector");
        unavailable["command"] = json!(["/bin/false"]);
        v["agents"]["default"]
            .as_array_mut()
            .unwrap()
            .insert(0, unavailable);
    });
    let mut rx = quiet_browser(&f);
    let _ = run(&f).await;
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(cards(&f).len(), 1);
    assert_eq!(rx.try_recv().unwrap()["channel"], "thinking");
}

#[tokio::test]
async fn worker_failures_and_mismatched_decision_identity_are_retained_without_retry() {
    for mode in [
        "fail",
        "wrong-request",
        "wrong-event",
        "wrong-model",
        "bad-confidence",
        "refused",
    ] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        run(&f).await.unwrap();
        assert_eq!(calls(&f, "decision").len(), 1, "{mode}");
        assert_eq!(states(&f), vec!["retained"], "{mode}");
        assert!(rx.try_recv().is_err(), "{mode}");
        assert_eq!(cards(&f).len(), 1, "{mode}");
    }
}

#[tokio::test]
async fn ready_source_removed_from_policy_never_appends_or_reevaluates() {
    for removed in ["audience", "source", "owner"] {
        let f = fixture();
        run(&f).await.unwrap();
        assert_eq!(states(&f), vec!["context_queued"]);
        observations(&f, vec![]);
        rewrite_policy(&f, |v| match removed {
            "audience" => v["agents"]["default"][0]["audiences"] = json!([]),
            "source" => v["agents"]["default"] = json!([]),
            _ => v["agents"]["default"][0]["owners"] = json!(["other"]),
        });
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        assert!(rx.try_recv().is_err(), "{removed}");
        assert_eq!(calls(&f, "decision").len(), 1);
        assert!(!states(&f).iter().any(|v| v == "ready"), "{removed}");
    }
}

#[tokio::test]
async fn revocation_while_reading_source_prevents_paid_evaluation() {
    for revoke in ["call", "audience", "credential", "room"] {
        let f = fixture();
        std::fs::write(f.home.path().join("observer-block"), "").unwrap();
        let s = f.s.clone();
        let id = f.id.clone();
        let h = f.headers.clone();
        let task = tokio::spawn(async move { tick(&s, &id, &h).await });
        entered(&f, "observer").await;
        match revoke {
            "call" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET state='closed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            "audience" => {
                rewrite_policy(&f, |v| v["agents"]["default"][0]["audiences"] = json!([]))
            }
            "credential" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM tokens", [])
                    .unwrap();
            }
            _ => {
                f.s.db.lock().unwrap().execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'other','member',?2)",params![f.room,now_ms()]).unwrap();
            }
        }
        std::fs::remove_file(f.home.path().join("observer-block")).unwrap();
        let _ = task.await.unwrap();
        assert!(calls(&f, "decision").is_empty(), "{revoke}");
        assert!(cards(&f).is_empty(), "{revoke}");
    }
}

#[tokio::test]
async fn revocation_while_deciding_preserves_attempt_but_never_cards_or_appends() {
    for revoke in ["call", "policy", "credential"] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-block"), "").unwrap();
        let mut rx = quiet_browser(&f);
        let s = f.s.clone();
        let id = f.id.clone();
        let h = f.headers.clone();
        let task = tokio::spawn(async move { tick(&s, &id, &h).await });
        entered(&f, "decision").await;
        match revoke {
            "call" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET state='closed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            "policy" => rewrite_policy(&f, |v| v["enabled"] = json!(false)),
            _ => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM tokens", [])
                    .unwrap();
            }
        }
        std::fs::remove_file(f.home.path().join("decision-block")).unwrap();
        let _ = task.await.unwrap();
        assert_eq!(calls(&f, "decision").len(), 1);
        assert!(cards(&f).is_empty(), "{revoke}");
        assert!(rx.try_recv().is_err(), "{revoke}");
    }
}

#[tokio::test]
async fn crash_after_paid_intent_is_unknown_and_never_replayed() {
    let f = fixture();
    std::fs::write(f.home.path().join("decision-block"), "").unwrap();
    let s = f.s.clone();
    let id = f.id.clone();
    let h = f.headers.clone();
    let task = tokio::spawn(async move { tick(&s, &id, &h).await });
    entered(&f, "decision").await;
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(states(&f), vec!["deciding"]);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    std::fs::remove_file(f.home.path().join("decision-block")).unwrap();
    migrate(&f.s.db.lock().unwrap()).unwrap();
    assert_eq!(states(&f), vec!["unknown"]);
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 1);
    assert!(cards(&f).is_empty());
    assert_eq!(states(&f), vec!["unknown"]);
}

#[tokio::test]
async fn oversized_whole_report_stays_saved_without_partial_voice_claim() {
    let f = fixture();
    let mut event = source_event();
    event["text"] = json!(format!(
        "{} The final check FAILED; do not claim completion.",
        "Detailed source observation. ".repeat(40)
    ));
    observations(&f, vec![event.clone()]);
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    assert_eq!(cards(&f)[0]["text"], event["text"]);
    let command = rx.try_recv().unwrap();
    let text = command["content"].as_str().unwrap();
    assert!(text.len() <= 480);
    assert!(text.contains("complete text does not fit"));
    assert!(text.contains("do not infer an answer or completion"));
    assert!(!text.contains("Detailed source observation"));
    assert!(rx.try_recv().is_err());
    provider_event(
        &f.s,
        &f.id,
        &json!({"type":"session.thinking.appended","client_event_id":command["event_id"]}),
    )
    .unwrap();
    assert_eq!(states(&f), vec!["context_accepted"]);
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 1);
}

#[tokio::test]
async fn append_settlement_requires_matching_call_id_and_channel_without_claiming_playback() {
    let f = fixture();
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    let command = rx.try_recv().unwrap();
    let event_id = command["event_id"].as_str().unwrap();
    assert!(event_id.starts_with("awareness-"));
    assert_eq!(states(&f), vec!["context_queued"]);
    let mut acknowledgment = json!({"type":"session.thinking.appended","client_event_id":event_id,"start_ms":100,"end_ms":120});
    provider_event(&f.s, "another-call", &acknowledgment).unwrap();
    assert_eq!(states(&f), vec!["context_queued"]);
    acknowledgment["client_event_id"] = json!(format!("awareness-{}", "a".repeat(64)));
    provider_event(&f.s, &f.id, &acknowledgment).unwrap();
    assert_eq!(states(&f), vec!["context_queued"]);
    acknowledgment["client_event_id"] = json!(event_id);
    acknowledgment["type"] = json!("session.commentary.appended");
    provider_event(&f.s, &f.id, &acknowledgment).unwrap();
    assert_eq!(states(&f), vec!["context_queued"]);
    acknowledgment["type"] = json!("session.thinking.appended");
    provider_event(&f.s, &f.id, &acknowledgment).unwrap();
    assert_eq!(states(&f), vec!["context_accepted"]);
    provider_event(&f.s, &f.id, &acknowledgment).unwrap();
    acknowledgment["type"] = json!("error");
    provider_event(&f.s, &f.id, &acknowledgment).unwrap();
    assert_eq!(states(&f), vec!["context_accepted"]);
    assert!(rx.try_recv().is_err());
    assert_eq!(calls(&f, "decision").len(), 1);
    assert!(
        !states(&f)
            .iter()
            .any(|state| state.contains("spoken") || state.contains("played"))
    );
}

#[tokio::test]
async fn official_nested_append_error_requires_unambiguous_identity_and_never_retries() {
    for (error_type, expected) in [
        ("invalid_request_error", "context_rejected"),
        ("server_error", "unknown"),
        ("rate_limit_error", "unknown"),
    ] {
        let f = fixture();
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        let command = rx.try_recv().unwrap();
        let event_id = command["event_id"].as_str().unwrap();
        let nested = json!({"type":"error","error":{"type":error_type,"client_event_id":event_id}});
        provider_event(&f.s, "another-call", &nested).unwrap();
        assert_eq!(states(&f), vec!["context_queued"]);
        for outer in [Value::Null, json!(false), json!("awareness-unrelated")] {
            let mut ambiguous = nested.clone();
            ambiguous["client_event_id"] = outer;
            provider_event(&f.s, &f.id, &ambiguous).unwrap();
            assert_eq!(states(&f), vec!["context_queued"]);
        }
        let mut wrong_nested = nested.clone();
        wrong_nested["client_event_id"] = json!(event_id);
        wrong_nested["error"]["client_event_id"] = json!("awareness-unrelated");
        provider_event(&f.s, &f.id, &wrong_nested).unwrap();
        assert_eq!(states(&f), vec!["context_queued"]);
        let mut matching = nested.clone();
        if error_type == "rate_limit_error" {
            matching["client_event_id"] = json!(event_id);
        }
        provider_event(&f.s, &f.id, &matching).unwrap();
        assert_eq!(states(&f), vec![expected]);
        provider_event(
            &f.s,
            &f.id,
            &json!({"type":"session.thinking.appended","client_event_id":event_id}),
        )
        .unwrap();
        run(&f).await.unwrap();
        assert_eq!(states(&f), vec![expected]);
        assert!(rx.try_recv().is_err());
        assert_eq!(calls(&f, "decision").len(), 1);
    }
}

#[tokio::test]
async fn speaking_recent_transcript_or_unknown_floor_defers_even_high_confidence_notice() {
    for blocked in ["speaking", "transcript", "stale-browser", "unknown-browser"] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), "notify").unwrap();
        let mut rx = quiet_browser(&f);
        {
            let db = f.s.db.lock().unwrap();
            match blocked {
                "speaking" => {
                    db.execute(
                        "UPDATE companion_surfaces SET user_speaking=1 WHERE call_id=?1",
                        [&f.id],
                    )
                    .unwrap();
                }
                "transcript" => {
                    db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'fragment','transcript',?2,?3)",params![f.id,json!({"role":"user","text":"Also wait because"}).to_string(),now_ms()]).unwrap();
                }
                "unknown-browser" => {
                    db.execute("DELETE FROM companion_surfaces WHERE call_id=?1", [&f.id])
                        .unwrap();
                }
                _ => {
                    db.execute(
                        "UPDATE companion_surfaces SET updated_at=?2 WHERE call_id=?1",
                        params![f.id, now_ms() - 6000],
                    )
                    .unwrap();
                }
            }
        }
        run(&f).await.unwrap();
        assert!(rx.try_recv().is_err(), "{blocked}");
        assert_eq!(states(&f), vec!["pending"], "{blocked}");
        assert!(cards(&f).is_empty(), "{blocked}");
        assert!(calls(&f, "decision").is_empty(), "{blocked}");
    }
}

#[tokio::test]
async fn quiet_facts_remain_available_during_new_requests_without_spoken_notice() {
    let f = fixture();
    let mut rx = quiet_browser(&f);
    f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'long-running',0,'dispatched','retained-work',?2)",params![f.id,now_ms()-60000]).unwrap();
    run(&f).await.unwrap();
    assert_eq!(rx.try_recv().unwrap()["channel"], "thinking");
    let mut second = source_event();
    second["id"] = json!("another-fact");
    observations(&f, vec![second]);
    f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'fresh-request',100,'waiting_transcript','new-work',?2)",params![f.id,now_ms()]).unwrap();
    run(&f).await.unwrap();
    assert_eq!(rx.try_recv().unwrap()["channel"], "thinking");
    assert!(states(&f).iter().all(|v| v == "context_queued"));
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_delegations SET created_at=?2 WHERE call_id=?1",
            params![f.id, now_ms() - 7000],
        )
        .unwrap();
    run(&f).await.unwrap();
    assert!(rx.try_recv().is_err());
    assert_eq!(calls(&f, "decision").len(), 2);
    let work: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM companion_delegations WHERE call_id=?1 AND id='long-running'",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(work, "dispatched");
}

#[tokio::test]
async fn peer_source_attribution_reaches_decisions_cards_and_voice_without_actor_substitution() {
    let f = fixture();
    let mut event = source_event();
    event["actor"] = json!("Rook");
    event["kind"] = json!("message");
    event["text"] = json!("I am checking the roof; the result is not verified.");
    observations(&f, vec![event.clone()]);
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    let request = &calls(&f, "decision")[0]["event"];
    assert_eq!(request["actor"], "Rook");
    assert_eq!(request["connection_id"], "minecraft");
    let card = &cards(&f)[0];
    assert!(card["title"].as_str().unwrap().contains("Rook"));
    assert_eq!(card["text"], event["text"]);
    assert_eq!(card["attribution"]["actor"], "Rook");
    assert_eq!(card["attribution"]["scope"], "message");
    assert_eq!(card["source"], event);
    let append = rx.try_recv().unwrap();
    let content = append["content"].as_str().unwrap();
    for required in ["Rook", "Workshop", "not verified"] {
        assert!(content.contains(required), "{content}");
    }
    assert!(!content.contains("Coach"));
    let current = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(current["observations"][0]["source"], event);
    assert_eq!(current["observations"][0]["attribution"]["actor"], "Rook");
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(calls(&f, "observer").len(), 1);
}

#[test]
fn observation_read_is_cached_bounded_current_policy_and_honest_about_coverage() {
    let f = fixture();
    let empty = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(empty["status"], "no_saved_facts");
    assert_eq!(empty["sources"][0]["status"], "no_observation");
    assert!(
        empty["note"]
            .as_str()
            .unwrap()
            .contains("do not establish inactivity")
    );
    assert!(current_observations(&f.s, "another-call", None).is_err());
    let (cfg, fingerprint) = config(&f.s).unwrap().unwrap();
    let source = &cfg.agents["default"][0];
    for index in 0..20 {
        let mut event = source_event();
        event["id"] = json!(format!("event-{index}"));
        event["actor"] = json!("Rook");
        record_event(&f.s, &f.id, source, &event, &fingerprint).unwrap();
    }
    let mut updated = source_event();
    updated["id"] = json!("event-19");
    updated["revision"] = json!("new-revision");
    updated["text"] = json!("Corrected report: the last check failed.");
    record_event(&f.s, &f.id, source, &updated, &fingerprint).unwrap();
    let mut private = source_event();
    private["id"] = json!("private-unrelated");
    private["audience"] = json!({"kind":"private","owner":"other"});
    record_event(&f.s, &f.id, source, &private, &fingerprint).unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_awareness SET state='ignored' WHERE call_id=?1",
            [&f.id],
        )
        .unwrap();
    let current = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(current["observations"].as_array().unwrap().len(), 16);
    assert_eq!(current["omitted"], 4);
    assert_eq!(current["observations"][0]["source"], updated);
    assert_eq!(
        current["observations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["source"]["id"] == "event-19")
            .count(),
        1
    );
    assert!(
        current["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["source"]["id"] != "private-unrelated")
    );
    rewrite_policy(&f, |c| c["agents"]["default"][0]["audiences"] = json!([]));
    assert!(
        current_observations(&f.s, &f.id, None).unwrap()["observations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(calls(&f, "observer").is_empty());
}

#[test]
fn observation_read_balances_connections_and_prioritizes_current_tasks_and_recent_reports() {
    let f = fixture();
    rewrite_policy(&f, |c| {
        let mut slack = c["agents"]["default"][0].clone();
        slack["id"] = json!("slack");
        slack["label"] = json!("Team chat");
        c["agents"]["default"].as_array_mut().unwrap().push(slack);
    });
    let (cfg, fingerprint) = config(&f.s).unwrap().unwrap();
    let minecraft = &cfg.agents["default"][0];
    let slack = &cfg.agents["default"][1];
    let now = now_ms();
    let mut task = source_event();
    task["id"] = json!("current-game-task");
    task["kind"] = json!("task_observation");
    task["occurred_at"] = json!(timestamp(now - 3_600_000));
    task["text"] = json!("Current Minecraft task: reviewing the stocked supermarket.");
    record_source_event(&f.s, &f.id, minecraft, &task, &fingerprint, false).unwrap();
    // Backfill arrives in reverse event order; retention/poll order must not
    // outrank the actual source time within either connection.
    for index in (0..20).rev() {
        for source in [minecraft, slack] {
            let mut event = source_event();
            event["id"] = json!(format!("{}-message-{index}", source.id));
            event["kind"] = json!("message");
            event["occurred_at"] = json!(timestamp(now - 60_000 + index * 1000));
            record_event(&f.s, &f.id, source, &event, &fingerprint).unwrap();
        }
    }
    let mut revised = source_event();
    revised["id"] = json!("minecraft-message-19");
    revised["kind"] = json!("message");
    revised["revision"] = json!("corrected");
    revised["occurred_at"] = json!(timestamp(now - 41_000));
    revised["text"] = json!("Rook reports the inspection is still in progress.");
    record_event(&f.s, &f.id, minecraft, &revised, &fingerprint).unwrap();
    let mut unrelated = source_event();
    unrelated["id"] = json!("unrelated-private-fact");
    unrelated["audience"] = json!({"kind":"private","owner":"someone-else"});
    record_event(&f.s, &f.id, minecraft, &unrelated, &fingerprint).unwrap();
    f.s.db.lock().unwrap().execute(
        "UPDATE companion_awareness SET last_observed_at=?2 WHERE call_id=?1 AND connection_id='slack'",
        params![f.id, now + 1000],
    ).unwrap();
    let reply = current_observations(&f.s, &f.id, None).unwrap();
    let facts = reply["observations"].as_array().unwrap();
    assert_eq!(facts.len(), 16);
    assert_eq!(reply["omitted"], 25);
    assert_eq!(facts[0]["source"], task);
    for connection in ["minecraft", "slack"] {
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact["attribution"]["connection_id"] == connection)
                .count(),
            8
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact["source"]["id"] == format!("{connection}-message-19"))
        );
        assert!(
            !facts
                .iter()
                .any(|fact| fact["source"]["id"] == format!("{connection}-message-0"))
        );
    }
    assert_eq!(
        facts
            .iter()
            .find(|fact| fact["source"]["id"] == "minecraft-message-19")
            .unwrap()["source"],
        revised
    );
    assert!(
        !facts
            .iter()
            .any(|fact| fact["source"]["id"] == "unrelated-private-fact")
    );
    assert!(serde_json::to_vec(&reply).unwrap().len() <= 256 * 1024);
    rewrite_policy(&f, |c| c["agents"]["default"][0]["audiences"] = json!([]));
    let revoked = current_observations(&f.s, &f.id, None).unwrap();
    assert!(revoked["observations"].as_array().unwrap().is_empty());
    assert!(calls(&f, "decision").is_empty());
    assert!(calls(&f, "observer").is_empty());
}

#[test]
fn observation_read_bounds_serialized_bytes_without_clipping_any_included_fact() {
    let f = fixture();
    rewrite_policy(&f, |c| {
        let template = c["agents"]["default"][0].clone();
        c["agents"]["default"] = json!(
            (0..8)
                .map(|index| {
                    let mut source = template.clone();
                    source["id"] = json!(format!("connection-{index}"));
                    source["label"] = json!("\u{1}".repeat(80));
                    source
                })
                .collect::<Vec<_>>()
        );
    });
    let (cfg, fingerprint) = config(&f.s).unwrap().unwrap();
    let mut original = BTreeMap::new();
    let coverage = json!({"detail":"\\".repeat(2000)}).to_string();
    assert!(coverage.len() <= 4096);
    for source in &cfg.agents["default"] {
        for index in 0..2 {
            let mut event = source_event();
            event["id"] = json!(format!("{}-{index}", source.id));
            event["text"] = json!(format!("Reported: {}", "\n".repeat(3500)));
            event["actor"] = json!("R".repeat(160));
            assert!(event_valid(&event));
            original.insert(event["id"].as_str().unwrap().to_owned(), event.clone());
            record_event(&f.s, &f.id, source, &event, &fingerprint).unwrap();
        }
        f.s.db.lock().unwrap().execute(
            "INSERT INTO companion_observers(call_id,connection_id,cursor,coverage,updated_at,policy_hash) VALUES(?1,?2,'null',?3,?4,?5)",
            params![f.id,source.id,coverage,now_ms(),fingerprint],
        ).unwrap();
    }
    let reply = current_observations(&f.s, &f.id, None).unwrap();
    assert!(serde_json::to_vec(&reply).unwrap().len() <= 256 * 1024);
    let facts = reply["observations"].as_array().unwrap();
    assert!(!facts.is_empty());
    assert!(
        facts.len() < 16,
        "Serialized-byte bound, not the count bound, must apply"
    );
    assert_eq!(
        facts.len() + reply["omitted"].as_u64().unwrap() as usize,
        16
    );
    for fact in facts {
        let expected = &original[fact["source"]["id"].as_str().unwrap()];
        assert_eq!(fact["source"], *expected);
        assert_eq!(fact["text"], expected["text"]);
        assert_eq!(fact["attribution"]["actor"], expected["actor"]);
    }
    assert_eq!(reply["sources"].as_array().unwrap().len(), 8);
    assert!(calls(&f, "decision").is_empty());
    assert!(calls(&f, "observer").is_empty());
}

#[test]
fn observation_read_preserves_stale_time_and_revokes_closed_or_changed_call_identity() {
    let f = fixture();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET created_at=?2 WHERE id=?1",
            params![f.id, now_ms() - 300000],
        )
        .unwrap();
    let (cfg, fingerprint) = config(&f.s).unwrap().unwrap();
    let mut event = source_event();
    event["occurred_at"] = json!(timestamp(now_ms() - 180000));
    record_event(&f.s, &f.id, &cfg.agents["default"][0], &event, &fingerprint).unwrap();
    let saved = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(saved["observations"][0]["fresh"], true);
    assert_eq!(saved["observations"][0]["source_fresh"], false);
    assert_eq!(
        saved["observations"][0]["source"]["occurred_at"],
        event["occurred_at"]
    );
    assert!(saved["observations"][0]["source_age_ms"].as_i64().unwrap() >= 180000);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_awareness SET last_observed_at=?2 WHERE call_id=?1",
            params![f.id, now_ms() - 180000],
        )
        .unwrap();
    assert_eq!(
        current_observations(&f.s, &f.id, None).unwrap()["observations"][0]["fresh"],
        false
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    assert!(current_observations(&f.s, &f.id, None).is_err());
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='connected',endpoint_hash='changed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    assert!(current_observations(&f.s, &f.id, None).is_err());
    assert!(calls(&f, "decision").is_empty());
    assert!(calls(&f, "observer").is_empty());
}

#[tokio::test]
async fn baseline_old_tasks_and_chat_are_available_only_to_explicit_reads_without_replay() {
    let f = fixture();
    let mut task = source_event();
    task["id"] = json!("existing-task");
    task["kind"] = json!("task_observation");
    task["occurred_at"] = json!(timestamp(now_ms() - 3600000));
    task["actor"] = json!("Rook");
    task["text"] = json!("Task status was running; last reported progress was an hour ago.");
    let mut chat = task.clone();
    chat["id"] = json!("old-chat");
    chat["kind"] = json!("message");
    chat["text"] = json!("I started the roof earlier.");
    observations(&f, vec![task.clone(), chat.clone()]);
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    assert_eq!(states(&f), vec!["observed", "observed"]);
    assert!(cards(&f).is_empty());
    assert!(calls(&f, "decision").is_empty());
    assert!(rx.try_recv().is_err());
    let saved = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(saved["observations"].as_array().unwrap().len(), 2);
    for fact in saved["observations"].as_array().unwrap() {
        assert_eq!(fact["source"]["occurred_at"], task["occurred_at"]);
        assert_eq!(fact["attribution"]["actor"], "Rook");
        assert_eq!(fact["fresh"], true);
        assert_eq!(fact["source_fresh"], false);
        assert!(fact["source_age_ms"].as_i64().unwrap() >= 3600000);
    }
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_awareness SET last_observed_at=1 WHERE call_id=?1",
            [&f.id],
        )
        .unwrap();
    run(&f).await.unwrap();
    assert!(
        current_observations(&f.s, &f.id, None).unwrap()["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["fresh"] == true)
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(rx.try_recv().is_err());
    rewrite_policy(&f, |c| c["agents"]["default"][0]["audiences"] = json!([]));
    let revoked = current_observations(&f.s, &f.id, None).unwrap();
    assert!(revoked["observations"].as_array().unwrap().is_empty());
    assert_eq!(revoked["sources"][0]["status"], "no_observation");
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "observer").last().unwrap()["cursor"], Value::Null);
    assert!(
        current_observations(&f.s, &f.id, None).unwrap()["observations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(cards(&f).is_empty());
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn pre_call_stale_future_and_wrong_audience_events_do_not_spend_decisions() {
    for invalid in ["pre-call", "stale", "future", "audience"] {
        let f = fixture();
        let mut event = source_event();
        match invalid {
            "pre-call" => event["occurred_at"] = json!(timestamp(now_ms() - 6000)),
            "stale" => event["occurred_at"] = json!(timestamp(now_ms() - 180000)),
            "future" => event["occurred_at"] = json!(timestamp(now_ms() + 10000)),
            _ => event["audience"] = json!({"kind":"private","owner":"other"}),
        };
        observations(&f, vec![event]);
        run(&f).await.unwrap();
        assert!(calls(&f, "decision").is_empty(), "{invalid}");
        assert!(cards(&f).is_empty());
    }
}

#[tokio::test]
async fn action_observation_revisions_are_immutable_ordered_and_never_replace_submission_receipt() {
    let f = fixture();
    observations(&f, vec![]);
    let (request, _) = retained_action(&f);
    let original: Value = serde_json::from_str(
        &f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                params![f.id, request],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    let at = now_ms();
    let initial = json!({"request_id":request,"connection_id":"minecraft","revision":"a1","state":"accepted","scope":"task","verified":false,"observed_at":timestamp(at),"provenance":{"transport":"fixture","task_id":"task-1"}});
    let send = |update: Value| {
        let p = f.home.path().join("observer-result.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        v["actions"] = json!([update]);
        std::fs::write(p, v.to_string()).unwrap();
    };
    send(initial.clone());
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(initial.clone())
    );
    assert!(calls(&f, "decision").is_empty());
    let mut seen_again = initial.clone();
    seen_again["observed_at"] = json!(timestamp(at + 1000));
    send(seen_again);
    run(&f).await.unwrap();
    assert!(calls(&f, "decision").is_empty());
    let mut rewritten = initial.clone();
    rewritten["state"] = json!("blocked");
    send(rewritten);
    let _ = run(&f).await;
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(initial.clone())
    );
    assert!(calls(&f, "decision").is_empty());
    let mut older = initial.clone();
    older["revision"] = json!("a0");
    older["observed_at"] = json!(timestamp(at - 2000));
    older["state"] = json!("running");
    send(older);
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(initial.clone())
    );
    assert!(calls(&f, "decision").is_empty());
    let mut newest = initial.clone();
    newest["revision"] = json!("a2");
    newest["observed_at"] = json!(timestamp(at + 1000));
    newest["state"] = json!("reported_complete");
    send(newest.clone());
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(newest)
    );
    assert!(calls(&f, "decision").is_empty());
    assert_eq!(cards(&f).len(), 2);
    assert!(
        cards(&f).last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("not independent verification")
    );
    let saved: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                params![f.id, request],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), original);
}

#[tokio::test]
async fn observation_copy_omits_internal_request_ids_without_losing_receipts_or_overstating_outcomes()
 {
    for (scope, state, verified, phrase) in [
        ("message_delivery", "completed", true, "message delivered"),
        (
            "message_delivery",
            "unknown",
            false,
            "message delivery is unconfirmed",
        ),
        (
            "message_delivery",
            "completed",
            false,
            "message delivery is unconfirmed",
        ),
        ("task", "reported_complete", false, "work reported complete"),
        ("task", "unknown", false, "task status is unknown"),
    ] {
        let f = fixture();
        let (request_id, _) = retained_action(&f);
        let receipt: Value = serde_json::from_str(
            &f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                    params![f.id, request_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        let update = json!({"request_id":request_id,"connection_id":"minecraft","revision":"copy-v1","state":state,"scope":scope,"verified":verified,"observed_at":timestamp(now_ms()),"receipt":{"channel":"C0123456789","ts":"1794050000.000001"},"provenance":{"transport":"fixture"}});
        std::fs::write(f.home.path().join("observer-result.json"), json!({"version":1,"connection_id":"minecraft","availability":"available","events":[],"actions":[update],"cursor":null,"coverage":{}}).to_string()).unwrap();
        run(&f).await.unwrap();
        let rendered = cards(&f);
        assert_eq!(rendered.len(), 1);
        let text = rendered[0]["text"].as_str().unwrap();
        assert!(text.starts_with("Workshop: "));
        assert!(text.contains(phrase), "{text}");
        assert!(!text.contains(&request_id));
        assert_eq!(rendered[0]["source"]["id"], format!("action:{request_id}"));
        assert!(calls(&f, "decision").is_empty());
        assert_eq!(rendered[0]["presentation"], "quiet");
        assert_eq!(
            action_observation(&f.s, &f.id, &request_id).unwrap(),
            Some(update)
        );
        if scope == "task" {
            assert!(text.contains("not independent verification"));
        } else {
            assert!(
                text.contains("further work is not verified")
                    || text.contains("Further work is not verified")
            );
            assert_eq!(text.contains("confirms the message arrived"), verified);
        }
        let saved: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                    params![f.id, request_id],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), receipt);
    }
}

#[tokio::test]
async fn pending_source_policy_change_withdraws_before_any_paid_invocation() {
    let f = fixture();
    observations(&f, vec![]);
    let (c, hash) = config(&f.s).unwrap().unwrap();
    let source = &c.agents["default"][0];
    record_event(&f.s, &f.id, source, &source_event(), &hash).unwrap();
    rewrite_policy(&f, |v| v["agents"]["default"][0]["audiences"] = json!([]));
    run(&f).await.unwrap();
    assert!(calls(&f, "decision").is_empty());
    assert_eq!(states(&f), vec!["withdrawn"]);
    assert!(cards(&f).is_empty());
}

#[test]
fn long_call_does_not_silently_exhaust_its_source_history() {
    let f = fixture();
    let (config, policy) = config(&f.s).unwrap().unwrap();
    let source = &config.agents["default"][0];
    for index in 0..520 {
        let mut event = source_event();
        event["id"] = json!(format!("long-call-{index}"));
        assert!(record_source_event(&f.s, &f.id, source, &event, &policy, true).unwrap());
    }
    let count: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_awareness WHERE call_id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 520);
}

#[tokio::test]
async fn unchanged_action_receipts_rotate_without_starving_older_requests() {
    let f = fixture();
    observations(&f, vec![]);
    std::fs::write(
        f.home.path().join("observer.py"),
        r#"import json,pathlib,sys
p=pathlib.Path(__file__).parent
body=json.load(sys.stdin)
with (p/'observer-calls.jsonl').open('a') as stream:stream.write(json.dumps(body)+'\n')
result=json.loads((p/'observer-result.json').read_text())
by_id={item['request_id']:item for item in result['actions']}
result['actions']=[by_id[item['request_id']] for item in body['actions']]
print(json.dumps(result))
"#,
    )
    .unwrap();
    let observed_at = timestamp(now_ms());
    let mut originals = BTreeMap::new();
    let mut outcomes = Vec::new();
    {
        let db = f.s.db.lock().unwrap();
        for index in 0..6 {
            let request = format!("rotation-request-{index}");
            let receipt = json!({"request_id":request,"state":"queued","receipt":{"request_id":format!("domain-{index}"),"queued":true}});
            let update = json!({"request_id":request,"connection_id":"minecraft","revision":"unchanged-v1","state":"accepted","scope":"task","verified":false,"observed_at":observed_at,"provenance":{"transport":"fixture","task_id":format!("task-{index}")}});
            db.execute("INSERT INTO companion_actions(call_id,request_id,delegation_id,connection_id,instruction,source,state,result,created_at) VALUES(?1,?2,?3,'minecraft','Original request.','{}','queued',?4,?5)",params![f.id,request,format!("delegation-{index}"),receipt.to_string(),index]).unwrap();
            // Every request already has a stable observation. Repeated identical
            // outcomes still have to advance polling fairness, not create events.
            db.execute("INSERT INTO companion_observations(call_id,request_id,revision,data,updated_at) VALUES(?1,?2,'unchanged-v1',?3,0)",params![f.id,request,update.to_string()]).unwrap();
            let mut semantic = update.clone();
            semantic.as_object_mut().unwrap().remove("observed_at");
            db.execute("INSERT INTO companion_observation_revisions(call_id,request_id,revision,data) VALUES(?1,?2,'unchanged-v1',?3)",params![f.id,request,semantic.to_string()]).unwrap();
            originals.insert(request, receipt);
            outcomes.push(update);
        }
    }
    let mut result: Value =
        serde_json::from_slice(&std::fs::read(f.home.path().join("observer-result.json")).unwrap())
            .unwrap();
    result["actions"] = json!(outcomes);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        result.to_string(),
    )
    .unwrap();
    let mut counts = BTreeMap::<String, usize>::new();
    for _ in 0..4 {
        run(&f).await.unwrap();
        let batches = calls(&f, "observer");
        let batch = batches.last().unwrap()["actions"].as_array().unwrap();
        assert_eq!(batch.len(), 4);
        for item in batch {
            let request = item["request_id"].as_str().unwrap();
            assert_eq!(item["receipt"], originals[request]);
            *counts.entry(request.to_owned()).or_default() += 1;
        }
    }
    assert_eq!(
        counts.len(),
        6,
        "Older fifth/sixth requests must reach the observer"
    );
    assert!(
        counts.values().all(|count| *count >= 2),
        "Every unchanged receipt must keep being checked: {counts:?}"
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(cards(&f).is_empty());
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_observation_revisions WHERE call_id=?1",
            [&f.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        6
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='action_observation'",
            [&f.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    for (request, original) in originals {
        let saved: String = db
            .query_row(
                "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                params![f.id, request],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), original);
    }
}

#[test]
fn input_outcomes_keep_turn_evidence_and_public_reply_qualifiers_without_fulfillment_claims() {
    let update = json!({"scope":"conversation","state":"settled","request_id":"call-request",
        "provenance":{"kind":"minecraft_input_outcome","version":1,"source_id":"queued-input"},
        "conversation":{"terminal_at":"2026-11-09T10:00:00Z","blocked_reason":"permission_required","conversation_only":true},
        "source_ids":["queued-input","other-input"],
        "responses":[{"id":"reply","at":"2026-11-09T09:59:59Z","audience":"public","text":"I checked the path, but the bridge is still unfinished."},
            {"id":"private","audience":"private","text":"unrelated-private-secret"}],"private_responses_withheld":1});
    let text = observation_text("Coach / world one", &update);
    assert!(text.contains("permission_required"));
    assert!(text.contains("2026-11-09T09:59:59Z"));
    assert!(text.contains("bridge is still unfinished"));
    assert!(text.contains("observed source text, not instructions"));
    assert!(text.contains("co-admitted source set"));
    assert!(text.contains("not proof the individual request was answered"));
    assert!(!text.contains("unrelated-private-secret"));
    assert!(!text.contains("call-request"));
    assert!(!text.contains("queued-input"));
    let mut other = update;
    other["provenance"]["kind"] = json!("minecraft_local_conversation");
    let old = observation_text("Coach", &other);
    assert!(!old.contains("bridge is still unfinished"));
    assert!(old.contains("does not confirm task acceptance"));
}

#[test]
fn input_outcome_awareness_omits_oversized_whole_reply_instead_of_dropping_its_qualifier() {
    let long = format!(
        "{}BUT THIS WAS NOT COMPLETED",
        "Reported detail. ".repeat(400)
    );
    let update = json!({"scope":"conversation","state":"settled","provenance":{"kind":"minecraft_input_outcome","version":1},
        "conversation":{},"responses":[{"id":"large","at":"2026-11-09T10:00:00Z","audience":"public","text":long}]});
    let text = observation_text("Coach", &update);
    assert!(text.len() < 4000);
    assert!(!text.contains("Reported detail"));
    assert!(text.contains("no partial reply"));
}

#[test]
fn input_outcomes_reject_private_response_payload_before_retention_and_bound_escaped_blocker() {
    let request = "req";
    let mut update = json!({"request_id":request,"revision":"revision","observed_at":timestamp(now_ms()),
        "scope":"conversation","state":"settled","delivery_state":"terminal","verified":false,"fulfillment":"not_established",
        "provenance":{"kind":"minecraft_input_outcome","version":1},
        "conversation":{"conversation_only":true,"blocked_reason":"\u{0001}".repeat(1000)},
        "responses":[{"id":"secret","audience":"private","text":"private-secret"}]});
    assert!(!observation_valid(
        &update,
        &[json!({"request_id":request})]
    ));
    update["responses"] = json!([]);
    assert!(observation_valid(&update, &[json!({"request_id":request})]));
    let text = observation_text("Coach", &update);
    assert!(text.len() <= 4000);
    assert!(text.contains("omitted_fields"));
    assert!(text.contains("blocked_reason"));
    assert!(!text.contains("\\u0001"));
}

fn outcome_update(delivery: &str, state: &str, conversation_only: Value) -> Value {
    json!({"request_id":"original-call-request","revision":"outcome-revision","observed_at":timestamp(now_ms()),
        "scope":"conversation","state":state,"delivery_state":delivery,"verified":false,"fulfillment":"not_established",
        "provenance":{"kind":"minecraft_input_outcome","version":1,"source_id":"original-domain-input"},
        "conversation":{"conversation_only":conversation_only,"admitted_at":null,"terminal_at":null,"blocked_reason":null},
        "responses":[],"responses_omitted":0,"private_responses_withheld":0})
}

#[test]
fn queued_and_unknown_outcomes_preserve_unset_conversation_scope() {
    for delivery in ["queued", "unknown"] {
        let update = outcome_update(delivery, "unknown", Value::Null);
        assert!(observation_valid(
            &update,
            &[json!({"request_id":"original-call-request"})]
        ));
        let text = observation_text("Coach", &update);
        assert!(!text.contains("Recorded turn scope:"));
        assert!(!text.contains("not limited to conversation"));
        assert!(!text.contains("handling ended"));
        if delivery == "queued" {
            assert!(text.contains("Input queued"));
        } else {
            assert!(text.contains("outcome remains unknown"));
        }
    }
    for (delivery, state) in [
        ("admitted", "admitted"),
        ("terminal", "settled"),
        ("interrupted", "interrupted"),
    ] {
        let mut update = outcome_update(delivery, state, Value::Null);
        assert!(!observation_valid(
            &update,
            &[json!({"request_id":"original-call-request"})]
        ));
        update["conversation"]["conversation_only"] = json!(false);
        assert!(observation_valid(
            &update,
            &[json!({"request_id":"original-call-request"})]
        ));
    }
}

#[test]
fn domain_omission_flags_are_not_counts_or_retention_promises() {
    let mut update = outcome_update("terminal", "settled", json!(true));
    update["domain_private_responses_withheld"] = json!(true);
    update["domain_responses_omitted"] = json!(true);
    update["source_ids"] = json!(["original-domain-input", "another-co-admitted-source"]);
    update["responses"] = json!([{"id":"public","at":timestamp(now_ms()),"audience":"public","text":"The public reply that is available."}]);
    assert!(observation_valid(
        &update,
        &[json!({"request_id":"original-call-request"})]
    ));
    let text = observation_text("Coach", &update);
    assert!(text.contains("Private response content was withheld"));
    assert!(text.contains("available reply record is incomplete"));
    assert!(text.contains("omitted replies may not be retained"));
    assert!(!text.contains("Additional reply records are retained"));
    assert!(text.contains("not proof the individual request was answered"));
    assert!(text.contains("co-admitted source set"));
    assert!(!text.contains("another-co-admitted-source"));
    assert_eq!(update["private_responses_withheld"], 0);
    assert_eq!(update["responses_omitted"], 0);
    for key in [
        "domain_private_responses_withheld",
        "domain_responses_omitted",
    ] {
        for invalid in [json!(1), json!("true"), Value::Null, json!([])] {
            let mut wrong = update.clone();
            wrong[key] = invalid;
            assert!(
                !observation_valid(&wrong, &[json!({"request_id":"original-call-request"})]),
                "{key}: {wrong}"
            );
        }
    }
    for key in ["private_responses_withheld", "responses_omitted"] {
        for invalid in [json!(true), json!("1"), json!(-1), json!(1.5)] {
            let mut wrong = update.clone();
            wrong[key] = invalid;
            assert!(
                !observation_valid(&wrong, &[json!({"request_id":"original-call-request"})]),
                "{key}: {wrong}"
            );
        }
    }
    update["domain_private_responses_withheld"] = json!(false);
    update["domain_responses_omitted"] = json!(false);
    let text = observation_text("Coach", &update);
    assert!(!text.contains("Private response content was withheld"));
    assert!(!text.contains("available reply record is incomplete"));
}

#[test]
fn outcome_raw_validation_rejects_mismatched_delivery_and_malformed_public_reply() {
    let known = [json!({"request_id":"original-call-request"})];
    let update = outcome_update("terminal", "settled", json!(true));
    for change in [
        json!({"state":"completed"}),
        json!({"state":"admitted"}),
        json!({"delivery_state":"queued"}),
        json!({"delivery_state":"invented"}),
        json!({"conversation":{"conversation_only":"false"}}),
    ] {
        let mut wrong = update.clone();
        wrong
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        assert!(!observation_valid(&wrong, &known), "{wrong}");
    }
    let public = json!({"id":"public-reply","at":timestamp(now_ms()),"text":"The path is clear, but the shop is unfinished.","audience":"public"});
    let mut valid = update.clone();
    valid["responses"] = json!([public.clone()]);
    assert!(observation_valid(&valid, &known));
    for change in [
        json!({"id":null}),
        json!({"text":null}),
        json!({"text":""}),
        json!({"at":"not-a-time"}),
        json!({"audience":"private"}),
    ] {
        let mut reply = public.clone();
        reply
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let mut wrong = update.clone();
        wrong["responses"] = json!([reply]);
        assert!(!observation_valid(&wrong, &known), "{wrong}");
    }
}

fn retained_action(f: &Fixture) -> (String, String) {
    let request = uuid::Uuid::new_v4().to_string();
    let queue = uuid::Uuid::new_v4().to_string();
    let input = uuid::Uuid::new_v4().to_string();
    let delegation = uuid::Uuid::new_v4().to_string();
    let at = now_ms();
    let text = "Find why this stopped; preserve the existing request.";
    let source = json!({"call_id":f.id,"conversation_id":f.room,"actor":"owner","audience":"private","delegation_id":delegation,"message_id":input,"occurred_at":at,"original_text":text,"source_revision":auth::digest(text)});
    let receipt = json!({"request_id":request,"state":"queued","scope":"private_operator_instruction","receipt":{"request_id":queue,"queued":true}});
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')",params![input,f.room,text,at]).unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,100,'completed',?3,?4)",params![f.id,delegation,input,at]).unwrap();
    db.execute(
        "INSERT INTO companion_actions VALUES(?1,?2,?3,'minecraft',?4,?5,'queued',?6,?7)",
        params![
            f.id,
            request,
            delegation,
            text,
            source.to_string(),
            receipt.to_string(),
            at
        ],
    )
    .unwrap();
    bind_action_observer(&f.s, &db, &f.id, &request, "minecraft").unwrap();
    (request, queue)
}
fn another_call(f: &Fixture, owner: &str) -> String {
    let room =
        f.s.create_room("Next call", owner, &["default".into()])
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
    let id = uuid::Uuid::new_v4().to_string();
    let hash =
        auth::digest(&serde_json::to_string(&f.s.agent("default").unwrap().unwrap()).unwrap());
    f.s.db.lock().unwrap().execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,?2,?3,'test','default',?4,'connected',?5,?5,?6)",params![id,owner,uuid::Uuid::new_v4().to_string(),room,now_ms(),hash]).unwrap();
    id
}
fn returned_outcome(f: &Fixture, request: &str, queue: &str, terminal: bool) -> Value {
    let mut outcome = outcome_update(
        if terminal { "terminal" } else { "admitted" },
        if terminal { "settled" } else { "admitted" },
        json!(true),
    );
    outcome["request_id"] = json!(request);
    outcome["connection_id"] = json!("minecraft");
    outcome["revision"] = json!(if terminal {
        "terminal-v2"
    } else {
        "admitted-v1"
    });
    outcome["provenance"]["source_id"] = json!(queue);
    outcome["source_ids"] = json!([queue]);
    outcome["conversation"]["blocked_reason"] = json!("unresolved_activity_effects");
    outcome["conversation"]["admitted_at"] = json!(timestamp(now_ms() - 1000));
    outcome["conversation"]["terminal_at"] = if terminal {
        json!(timestamp(now_ms()))
    } else {
        Value::Null
    };
    outcome["task_authorizations"] =
        json!([{"operation":"amend","task_id":"original-task","authority_revision":2}]);
    outcome["responses"] = if terminal {
        json!([{"id":"public-report","audience":"public","at":timestamp(now_ms()),"text":"Construction has not resumed; the existing action receipt needs review."}])
    } else {
        json!([])
    };
    let mut response = fresh_result(f, vec![]);
    response["actions"] = json!([outcome.clone()]);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        response.to_string(),
    )
    .unwrap();
    outcome
}

#[tokio::test]
async fn bound_requested_outcome_reaches_quiet_context_without_optional_model_judgment() {
    for mode in ["low-confidence", "fail"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let expected = returned_outcome(&f, &request, &queue, true);
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        let original: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                    params![f.id, request],
                    |r| r.get(0),
                )
                .unwrap();
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        let mut commands = Vec::new();
        while let Ok(command) = rx.try_recv() {
            commands.push(command);
        }
        assert!(!commands.is_empty(), "{mode}");
        assert!(
            commands[0]["content"]
                .as_str()
                .unwrap()
                .contains("Construction has not resumed")
        );
        assert!(
            commands
                .iter()
                .all(|v| v["channel"] == "thinking" && v["content"].as_str().unwrap().len() <= 480)
        );
        let text = commands
            .iter()
            .map(|v| v["content"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Construction has not resumed"));
        assert!(text.contains("association with an individual answer is not established"));
        assert!(text.contains("resumed execution is not established"));
        assert!(calls(&f, "decision").is_empty());
        let rendered = cards(&f);
        assert_eq!(rendered.len(), 1);
        assert_eq!(
            rendered[0]["source"]["audience"],
            json!({"kind":"private","owner":"owner"})
        );
        assert_eq!(rendered[0]["source"]["provenance"]["source_call_id"], f.id);
        assert_eq!(rendered[0]["presentation"], "quiet");
        assert_eq!(rendered[0]["observed_only"], true);
        assert_eq!(
            action_observation(&f.s, &f.id, &request).unwrap(),
            Some(expected)
        );
        let db = f.s.db.lock().unwrap();
        let decision: String = db
            .query_row(
                "SELECT decision FROM companion_awareness WHERE call_id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&decision).unwrap(),
            json!({"status":"not_requested","reason":"bound_requested_outcome"})
        );
        assert_eq!(
            db.query_row(
                "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                params![f.id, request],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            original
        );
    }
}

#[tokio::test]
async fn outcome_kind_alone_or_unenrolled_action_cannot_bypass_decisions() {
    for mode in ["unenrolled", "changed_source", "forged_event"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let update = returned_outcome(&f, &request, &queue, true);
        let (configuration, fingerprint) = config(&f.s).unwrap().unwrap();
        let source = &configuration.agents["default"][0];
        match mode {
            "unenrolled" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM companion_action_observers", [])
                    .unwrap();
            }
            "changed_source" => {
                f.s.db.lock().unwrap().execute("UPDATE companion_actions SET source=json_set(source,'$.actor','another-person')",[]).unwrap();
            }
            _ => {
                let mut fake = requested_outcome_event("owner", source, &f.id, &update);
                fake["audience"] = json!({"kind":"channel","id":"workshop-public"});
                fake["text"] = json!("A helper said this was requested.");
                observations(&f, vec![fake]);
            }
        }
        run(&f).await.unwrap();
        assert!(states(&f).is_empty(), "{mode}");
        assert!(calls(&f, "decision").is_empty(), "{mode}");
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.id)
                .unwrap()
                .is_none()
        );
        // Even a caller of the internal producer cannot assert missing binding.
        if mode != "forged_event" {
            assert!(
                !preserve_requested_outcome(&f.s, &f.id, &f.id, source, &update, &fingerprint)
                    .unwrap()
            );
        }
    }
}

#[tokio::test]
async fn bound_result_waiting_for_floor_does_not_expire_or_refresh_original_time() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut update = returned_outcome(&f, &request, &queue, true);
    // A known old outcome, including its older public reply, must survive a
    // long explanation. No sleeps or refreshed source timestamps hide its age.
    let old = timestamp(now_ms() - 180_000);
    update["observed_at"] = json!(old);
    update["conversation"]["admitted_at"] = json!(timestamp(now_ms() - 181_000));
    update["conversation"]["terminal_at"] = json!(old);
    update["responses"][0]["at"] = json!(old);
    let mut response = fresh_result(&f, vec![]);
    response["actions"] = json!([update.clone()]);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        response.to_string(),
    )
    .unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_surfaces SET user_speaking=1 WHERE call_id=?1",
            [&f.id],
        )
        .unwrap();
    run(&f).await.unwrap();
    assert_eq!(states(&f), vec!["context_queued"]);
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    assert!(calls(&f, "decision").is_empty());
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    assert!(
        batch["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["channel"] == "thinking")
    );
    assert!(batch.to_string().contains(&old));
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap().unwrap()["observed_at"],
        old
    );
    let data: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT data FROM companion_awareness WHERE call_id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&data).unwrap()["occurred_at"],
        old
    );
}

#[tokio::test]
async fn queued_bound_outcome_rechecks_privacy_original_source_and_current_revision() {
    for change in [
        "policy",
        "owner",
        "endpoint",
        "source",
        "enrollment",
        "duplicate",
        "revision",
        "closed",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        returned_outcome(&f, &request, &queue, false);
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_surfaces SET user_speaking=1 WHERE call_id=?1",
                [&f.id],
            )
            .unwrap();
        run(&f).await.unwrap();
        assert_eq!(states(&f), vec!["context_queued"], "{change}");
        let original = action_observation(&f.s, &f.id, &request).unwrap().unwrap();
        match change {
            "policy" => rewrite_policy(&f, |v| {
                v["agents"]["default"][0]["owners"] = json!(["other"])
            }),
            "owner" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET owner='other' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            "endpoint" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET endpoint_hash='other' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            "source" => {
                f.s.db.lock().unwrap().execute("UPDATE companion_actions SET source=json_set(source,'$.original_text','altered request')",[]).unwrap();
            }
            "enrollment" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM companion_action_observers", [])
                    .unwrap();
            }
            "duplicate" => {
                f.s.db.lock().unwrap().execute("INSERT INTO companion_actions SELECT call_id,'duplicate-request','duplicate-delegation',connection_id,instruction,source,state,result,created_at FROM companion_actions WHERE request_id=?1",[&request]).unwrap();
            }
            "revision" => {
                let next = returned_outcome(&f, &request, &queue, true);
                let (c, _) = config(&f.s).unwrap().unwrap();
                save_observation(&f.s, &f.id, &c.agents["default"][0], &next).unwrap();
            }
            _ => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET state='closed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
        }
        let _rx = quiet_browser(&f);
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.id)
                .unwrap()
                .is_none(),
            "{change}"
        );
        assert!(calls(&f, "decision").is_empty(), "{change}");
        if change != "revision" {
            assert_eq!(
                action_observation(&f.s, &f.id, &request).unwrap(),
                Some(original)
            );
        }
    }
}

#[tokio::test]
async fn late_bound_result_enters_same_private_call_without_new_action_or_model() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    returned_outcome(&f, &request, &queue, false);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    followthrough_tick(&f.s, &f.id, &f.headers, false)
        .await
        .unwrap();
    let new = another_call(&f, "owner");
    f.s.db.lock().unwrap().execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at) VALUES(?1,'next-browser','fixture',0,0,0,0,?2)",params![new,now_ms()]).unwrap();
    returned_outcome(&f, &request, &queue, true);
    tick(&f.s, &new, &f.headers).await.unwrap();
    let batch = crate::companion_delivery::take_ready(&f.s, &new)
        .unwrap()
        .unwrap();
    assert!(batch.to_string().contains("Construction has not resumed"));
    assert!(
        batch["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["channel"] == "thinking")
    );
    assert!(calls(&f, "decision").is_empty());
    let db = f.s.db.lock().unwrap();
    let raw: String = db
        .query_row(
            "SELECT data FROM companion_awareness WHERE call_id=?1",
            [&new],
            |r| r.get(0),
        )
        .unwrap();
    let event: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(event["provenance"]["source_call_id"], f.id);
    assert_eq!(event["provenance"]["source_id"], queue);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        db.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM companion_actions WHERE call_id=?1)",
            [&new],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}

#[tokio::test]
async fn uncertain_requested_result_append_is_not_retried_or_reclassified() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    returned_outcome(&f, &request, &queue, true);
    run(&f).await.unwrap();
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let parts = batch["commands"].as_array().unwrap();
    provider_event(&f.s,&f.id,&json!({"type":"error","error":{"type":"server_error","client_event_id":parts[0]["event_id"]}})).unwrap();
    assert_eq!(states(&f), vec!["unknown"]);
    run(&f).await.unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(states(&f), vec!["unknown"]);
    assert!(calls(&f, "decision").is_empty());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_awareness_appends",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        parts.len() as i64
    );
}

#[tokio::test]
async fn closed_call_collects_exact_terminal_without_decision_or_replay_and_next_call_reads_it() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    let expected = returned_outcome(&f, &request, &queue, true);
    assert!(
        followthrough_tick(&f.s, &f.id, &f.headers, false)
            .await
            .unwrap()
    );
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(expected)
    );
    assert!(
        !followthrough_tick(&f.s, &f.id, &f.headers, false)
            .await
            .unwrap()
    );
    assert_eq!(calls(&f, "observer").len(), 1);
    assert_eq!(
        calls(&f, "observer")[0]["actions"][0]["receipt"]["receipt"]["request_id"],
        queue
    );
    assert!(calls(&f, "decision").is_empty());
    let new = another_call(&f, "owner");
    let slot = action_observation_for_call(&f.s, &new, &request)
        .unwrap()
        .unwrap();
    assert_eq!(slot["source_call_id"], f.id);
    assert_eq!(slot["observation"]["delivery_state"], "terminal");
    assert_eq!(slot["fulfillment"], "not_established");
    assert_eq!(slot["automatic_replay_allowed"], false);
    assert!(
        slot["latest_fact"]
            .as_str()
            .unwrap()
            .contains("amendment to the existing assignment")
    );
    assert!(
        slot["latest_fact"]
            .as_str()
            .unwrap()
            .contains("Construction has not resumed")
    );
    let history = crate::companion_history::snapshot(&f.s, &new)
        .unwrap()
        .unwrap();
    assert_eq!(history["action_outcomes"][0]["request_id"], request);
    assert!(history["action_outcomes"][0].get("observation").is_none());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(cards(&f).is_empty());
}

#[tokio::test]
async fn later_call_refreshes_original_pending_receipt_without_copying_action_authority() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    returned_outcome(&f, &request, &queue, false);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    followthrough_tick(&f.s, &f.id, &f.headers, false)
        .await
        .unwrap();
    let new = another_call(&f, "owner");
    returned_outcome(&f, &request, &queue, true);
    followthrough_tick(&f.s, &new, &f.headers, true)
        .await
        .unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap().unwrap()["state"],
        "settled"
    );
    assert!(action_observation(&f.s, &new, &request).unwrap().is_none());
    let facts = current_observations(&f.s, &new, None).unwrap();
    assert!(facts.to_string().contains("Construction has not resumed"));
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_actions WHERE call_id=?1",
                [&new],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn cross_call_outcomes_refuse_changed_actor_endpoint_policy_and_duplicate_queue() {
    for change in [
        "owner",
        "endpoint",
        "source",
        "message",
        "audience",
        "duplicate",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_calls SET state='closed' WHERE id=?1",
                [&f.id],
            )
            .unwrap();
        let destination = another_call(&f, if change == "owner" { "other" } else { "owner" });
        match change {
            "endpoint" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET endpoint_hash='different' WHERE id=?1",
                        [&destination],
                    )
                    .unwrap();
            }
            "source" => rewrite_policy(&f, |value| {
                value["agents"]["default"][0]["audiences"] = json!([])
            }),
            "message" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE messages SET text='Changed original source' WHERE role='user'",
                        [],
                    )
                    .unwrap();
            }
            "audience" => {
                f.s.db.lock().unwrap().execute("UPDATE companion_actions SET source=json_set(source,'$.audience','public')",[]).unwrap();
            }
            "duplicate" => {
                f.s.db.lock().unwrap().execute("INSERT INTO companion_actions SELECT call_id,'duplicate','another-delegation',connection_id,instruction,source,state,result,created_at FROM companion_actions",[]).unwrap();
            }
            _ => (),
        }
        returned_outcome(&f, &request, &queue, true);
        assert!(
            !followthrough_tick(&f.s, &destination, &f.headers, true)
                .await
                .unwrap(),
            "{change}"
        );
        assert!(
            action_observation_for_call(&f.s, &destination, &request)
                .unwrap()
                .is_none(),
            "{change}"
        );
        assert!(calls(&f, "observer").is_empty(), "{change}");
    }
}

#[tokio::test]
async fn unknown_receipt_remains_unknown_and_missing_enrollment_is_not_recreated() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    let mut update = returned_outcome(&f, &request, &queue, false);
    update["delivery_state"] = json!("unknown");
    update["state"] = json!("unknown");
    update["conversation"]["conversation_only"] = Value::Null;
    update["revision"] = json!("unknown-v1");
    let mut response = fresh_result(&f, vec![]);
    response["actions"] = json!([update]);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        response.to_string(),
    )
    .unwrap();
    followthrough_tick(&f.s, &f.id, &f.headers, false)
        .await
        .unwrap();
    let new = another_call(&f, "owner");
    assert_eq!(
        action_observation_for_call(&f.s, &new, &request)
            .unwrap()
            .unwrap()["observation"]["state"],
        "unknown"
    );
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM companion_action_observers", [])
        .unwrap();
    assert!(
        !followthrough_tick(&f.s, &new, &f.headers, true)
            .await
            .unwrap()
    );
    assert!(
        action_observation_for_call(&f.s, &new, &request)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn current_snapshot_is_presented_without_forging_old_progress_time() {
    let f = fixture();
    let old = timestamp(now_ms() - 14 * 3600 * 1000);
    let mut event = source_event();
    event["kind"] = json!("task_observation");
    event["occurred_at"] = json!(old);
    event["observed_at"] = json!(timestamp(now_ms()));
    event["provenance"] = json!({"kind":"minecraft_current_task","current_state_snapshot":true});
    event["text"] = json!(json!({"status":"running","progress":{"at":old}}).to_string());
    observations(&f, vec![event.clone()]);
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 1);
    let command = rx.try_recv().unwrap();
    let text = command["content"].as_str().unwrap();
    assert!(text.contains("status running"));
    assert!(text.contains(&old));
    assert!(text.contains("does not prove a game action"));
    let facts = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(facts["observations"][0]["source"]["occurred_at"], old);
    event["observed_at"] = json!(timestamp(now_ms() + 1000));
    observations(&f, vec![event]);
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 1);
}

#[test]
fn retained_request_binds_human_text_and_wrapper_evidence_separately() {
    let f = fixture();
    let (request, _) = retained_action(&f);
    let human = "Please diagnose and resume the same work.";
    let wrapper = "Private prior context, instructions, current source, and tool references.";
    {
        let db = f.s.db.lock().unwrap();
        db.execute("UPDATE messages SET text=?1 WHERE role='user'", [wrapper])
            .unwrap();
        db.execute(
            "UPDATE companion_delegations SET request_text=?1 WHERE call_id=?2",
            params![human, f.id],
        )
        .unwrap();
        db.execute("UPDATE companion_actions SET source=json_set(source,'$.original_text',?1,'$.source_revision',?2,'$.evidence_revision',?3) WHERE request_id=?4",params![human,auth::digest(human),auth::digest(wrapper),request]).unwrap();
        assert!(original_source_valid(&db, &f.id, &request).unwrap());
        db.execute("UPDATE companion_delegations SET request_text='Changed human instruction' WHERE call_id=?1",[&f.id]).unwrap();
        assert!(!original_source_valid(&db, &f.id, &request).unwrap());
    }
}

#[tokio::test]
async fn revoked_read_in_flight_retains_no_domain_response() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='closed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    returned_outcome(&f, &request, &queue, true);
    std::fs::write(f.home.path().join("observer-block"), "").unwrap();
    let (state, id, headers) = (f.s.clone(), f.id.clone(), f.headers.clone());
    let pending =
        tokio::spawn(async move { followthrough_tick(&state, &id, &headers, false).await });
    entered(&f, "observer").await;
    rewrite_policy(&f, |value| {
        value["agents"]["default"][0]["owners"] = json!(["other"])
    });
    std::fs::remove_file(f.home.path().join("observer-block")).unwrap();
    assert!(!pending.await.unwrap().unwrap());
    assert!(action_observation(&f.s, &f.id, &request).unwrap().is_none());
    assert!(cards(&f).is_empty());
    assert!(calls(&f, "decision").is_empty());
}

#[test]
fn mismatched_domain_queue_and_malformed_amendment_cannot_become_facts() {
    let known = [
        json!({"request_id":"original-call-request","receipt":{"receipt":{"request_id":"expected-domain-input"}}}),
    ];
    let mut update = outcome_update("terminal", "settled", json!(true));
    assert!(!observation_valid(&update, &known));
    update["provenance"]["source_id"] = json!("expected-domain-input");
    assert!(observation_valid(&update, &known));
    for wrong in [
        json!({"operation":"amend","task_id":"task","authority_revision":false}),
        json!({"operation":"execute","task_id":"task","authority_revision":1}),
    ] {
        update["task_authorizations"] = json!([wrong]);
        assert!(!observation_valid(&update, &known));
    }
}

#[test]
fn scoped_outcome_requires_exact_original_actor_audience_and_source_receipt() {
    let request = uuid::Uuid::new_v4().to_string();
    let queue = uuid::Uuid::new_v4().to_string();
    let source = json!({"version":1,"request_id":request,"call_id":"original-call","intent_id":"original-intent",
        "revision":"original-human-text-revision","occurred_at":timestamp(now_ms()),
        "actor":{"id":"authenticated-human","label":"authenticated-human"},
        "audience":{"kind":"private","experience":"companion","id":"original-room"}});
    let action = json!({"request_id":request,"receipt":{"request_id":request,"state":"queued","scope":"private_installation_input",
        "protocol":"installation_inputs.v1","caller_id":"companion","source":source,
        "receipt":{"queued":true,"request_id":queue,"source_request_id":request}}});
    let mut update = outcome_update("terminal", "settled", json!(true));
    update["request_id"] = json!(request);
    update["provenance"]["source_id"] = json!(queue);
    update["installation_source"] = source;
    update["installation_source"]["caller"] = json!({"id":"companion","label":"Companion"});
    update["domain_response_association"] = json!("shared_turn");
    update["private_game_responses_included"] = json!(false);
    update["responses"] = json!([{"id":"reply","at":"2026-11-09T20:34:51.066Z","audience":"public","text":"Automated Companion QA: hello from Constellation."}]);
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    update["domain_response_association"] = json!("single_source");
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let single = observation_text("Constellation Workshop", &update);
    assert!(single.contains("The domain links this public reply record to this input"));
    assert!(!single.contains("co-admitted source set"));
    assert!(
        single.find("Automated Companion QA: hello").unwrap()
            < single.find("handling ended").unwrap()
    );
    assert!(single.contains("broader task completion or resumed execution is not established"));
    assert_eq!(update["fulfillment"], "not_established");
    update["state"] = json!("admitted");
    update["delivery_state"] = json!("admitted");
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let ongoing = observation_text("Constellation Workshop", &update);
    assert!(ongoing.contains("handling continues"));
    assert!(!ongoing.contains("response is pending"));
    update["state"] = json!("settled");
    update["delivery_state"] = json!("terminal");
    update["domain_response_association"] = json!("not_established");
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let unknown = observation_text("Constellation Workshop", &update);
    assert!(unknown.contains("public reply occurrence is recorded"));
    assert!(!unknown.contains("links this public reply record to this input"));
    update["domain_response_association"] = json!("shared_turn");
    for (key, replacement) in [
        ("actor", json!({"id":"other-human","label":"other-human"})),
        (
            "audience",
            json!({"kind":"private","experience":"companion","id":"other-room"}),
        ),
        ("caller", json!({"id":"other-caller","label":"Other"})),
        ("call_id", json!("other-call")),
        ("intent_id", json!("other-intent")),
        ("revision", json!("other-revision")),
        ("occurred_at", json!(timestamp(now_ms() - 1000))),
        ("request_id", json!(uuid::Uuid::new_v4().to_string())),
    ] {
        let mut changed = update.clone();
        changed["installation_source"][key] = replacement;
        assert!(
            !observation_valid(&changed, std::slice::from_ref(&action)),
            "{key}"
        );
    }
    for (key, replacement) in [
        ("private_game_responses_included", json!(true)),
        ("private_game_responses_included", json!(0)),
        (
            "domain_response_association",
            json!("individual_fulfillment"),
        ),
        ("domain_response_association", Value::Null),
        ("domain_response_association", json!(true)),
        ("source_ids", json!([queue, "other-private-source"])),
    ] {
        let mut changed = update.clone();
        changed[key] = replacement;
        assert!(
            !observation_valid(&changed, std::slice::from_ref(&action)),
            "{key}"
        );
    }
    let text = observation_text("Coach", &update);
    assert!(text.contains("not proof the individual request was answered"));
    assert!(!text.contains("authenticated-human"));
    assert!(!text.contains("original-room"));
    let mut legacy = action.clone();
    legacy["receipt"]["scope"] = json!("private_operator_instruction");
    legacy["receipt"]
        .as_object_mut()
        .unwrap()
        .remove("protocol");
    assert!(!observation_valid(&update, &[legacy]));
    update["provenance"]["kind"] = json!("minecraft_observation");
    update["state"] = json!("unknown");
    for key in [
        "installation_source",
        "domain_response_association",
        "private_game_responses_included",
    ] {
        update.as_object_mut().unwrap().remove(key);
    }
    assert!(observation_valid(&update, &[action]));
}

fn private_source_reply_fixture() -> (Value, Value) {
    // Actual disposable cd747a1 producer output, using Companion source_envelope.
    let fixture: Value = serde_json::from_str(include_str!(
        "../bridge/test/fixtures/minecraft-private-source-reply.json"
    ))
    .unwrap();
    let source = &fixture["input"]["source"];
    let row = &fixture["response"]["outcomes"][0];
    let action = json!({"request_id":source["request_id"],"receipt":{
        "request_id":source["request_id"],"scope":"private_installation_input","state":"queued",
        "protocol":"installation_inputs.v1","caller_id":"companion","source":source,
        "receipt":fixture["receipt"]}});
    let mut update = outcome_update("terminal", "settled", json!(true));
    update["request_id"] = source["request_id"].clone();
    update["provenance"]["source_id"] = row["request_id"].clone();
    update["installation_source"] = row["source"].clone();
    update["domain_response_association"] = row["response_association"].clone();
    update["private_game_responses_included"] = json!(false);
    update["source_replies"] = row["source_replies"].clone();
    update["source_replies_capability"] = fixture["capability"]["source_replies"].clone();
    update["source_replies_omitted"] = json!(0);
    (action, update)
}

#[test]
fn private_source_reply_is_exactly_bound_recorded_evidence_not_hearing_or_completion() {
    let (action, update) = private_source_reply_fixture();
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let text = observation_text("Workshop", &update);
    assert!(text.contains("Recorded reply for this private conversation"));
    assert!(text.contains(update["source_replies"][0]["text"].as_str().unwrap()));
    assert!(text.contains("does not establish that it was spoken, heard, or acted on"));
    assert!(!text.contains("No public reply"));
    assert_eq!(update["fulfillment"], "not_established");
    assert_eq!(update["verified"], false);
    assert_eq!(update["responses"], json!([]));
    assert_eq!(update["private_game_responses_included"], false);
}

#[test]
fn private_source_replies_require_capability_original_source_audience_and_unique_ids() {
    let (action, update) = private_source_reply_fixture();
    for field in [
        "source_replies_capability",
        "installation_source",
        "source_replies_omitted",
    ] {
        let mut bad = update.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            !observation_valid(&bad, std::slice::from_ref(&action)),
            "{field}"
        );
    }
    for (field, value) in [
        ("version", json!(true)),
        ("version", json!(1.0)),
        ("max_items", json!(32.0)),
        ("max_text_bytes", json!(12000.0)),
        ("version", json!(2)),
        ("max_items", json!(33)),
    ] {
        let mut bad = update.clone();
        bad["source_replies_capability"][field] = value;
        assert!(
            !observation_valid(&bad, std::slice::from_ref(&action)),
            "{field}"
        );
    }
    for (field, value) in [
        (
            "audience",
            json!({"kind":"private","experience":"companion","id":"other-room"}),
        ),
        (
            "audience",
            json!({"kind":"private","experience":"voice","id":"disposable-companion-room"}),
        ),
        ("audience", json!("private")),
        ("delivery", json!("heard")),
        ("id", json!("invalid\nidentity")),
        ("at", json!("yesterday")),
    ] {
        let mut bad = update.clone();
        bad["source_replies"][0][field] = value;
        assert!(
            !observation_valid(&bad, std::slice::from_ref(&action)),
            "{field}"
        );
    }
    for (field, value) in [
        ("caller", json!({"id":"other","label":"Other"})),
        ("revision", json!("other-source")),
        ("call_id", json!("other-call")),
    ] {
        let mut bad = update.clone();
        bad["installation_source"][field] = value;
        assert!(
            !observation_valid(&bad, std::slice::from_ref(&action)),
            "{field}"
        );
    }
    let mut duplicate = update.clone();
    duplicate["source_replies"]
        .as_array_mut()
        .unwrap()
        .push(update["source_replies"][0].clone());
    assert!(!observation_valid(
        &duplicate,
        std::slice::from_ref(&action)
    ));
    let mut legacy = action.clone();
    legacy["receipt"] = json!({});
    assert!(!observation_valid(&update, &[legacy]));
    let mut unrelated = update.clone();
    unrelated["provenance"]["kind"] = json!("minecraft_observation");
    assert!(!observation_valid(&unrelated, &[action]));
    for field in SOURCE_REPLY_FIELDS {
        let mut alone = outcome_update("terminal", "settled", json!(true));
        alone[field] = update[field].clone();
        let legacy = json!({"request_id":alone["request_id"],"receipt":{}});
        assert!(
            !observation_valid(&alone, std::slice::from_ref(&legacy)),
            "{field}"
        );
        alone["provenance"]["kind"] = json!("minecraft_observation");
        assert!(!observation_valid(&alone, &[legacy]), "{field}");
    }
}

#[test]
fn private_source_reply_does_not_relax_public_validation_and_preserves_coverage_types() {
    let (action, mut update) = private_source_reply_fixture();
    update["domain_source_replies_omitted"] = json!(true);
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    assert!(observation_text("Workshop", &update).contains("private reply record is incomplete"));
    update["domain_private_responses_withheld"] = json!(true);
    let rendered = observation_text("Workshop", &update);
    assert!(rendered.contains("Recorded reply for this private conversation"));
    assert!(rendered.contains("Unrelated private game response content was withheld"));
    assert!(!rendered.contains("\nPrivate response content was withheld"));
    for (field, value) in [
        ("domain_source_replies_omitted", json!(1)),
        ("source_replies_omitted", json!(true)),
    ] {
        let mut bad = update.clone();
        bad[field] = value;
        assert!(
            !observation_valid(&bad, std::slice::from_ref(&action)),
            "{field}"
        );
    }
    update["responses"] = json!([{"id":"public","at":"2026-11-11T05:39:04.411Z",
        "audience":"private","text":"unrelated private game message"}]);
    assert!(!observation_valid(&update, &[action]));
    assert!(!observation_text("Workshop", &update).contains("unrelated private game message"));
}

#[test]
fn private_source_reply_announcement_omits_whole_multibyte_reply_with_qualification() {
    let (action, mut update) = private_source_reply_fixture();
    update["source_replies"][0]["text"] = json!(format!("{} not completion", "界".repeat(1100)));
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let text = observation_text("Workshop", &update);
    assert!(!text.contains("界"));
    assert!(text.contains("Some returned private replies are not included here"));
    assert!(!text.contains("Recorded reply for this private conversation"));
    update["source_replies"][0]["text"] = json!("界".repeat(4001));
    assert!(!observation_valid(&update, &[action]));
}

#[test]
fn private_source_reply_retention_deduplicates_same_revision_and_preserves_original_time() {
    let f = fixture();
    let source = config(&f.s).unwrap().unwrap().0.agents["default"][0].clone();
    let (action, update) = private_source_reply_fixture();
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    assert!(save_observation(&f.s, &f.id, &source, &update).unwrap());
    let mut later = update.clone();
    later["observed_at"] = json!(timestamp(now_ms() + 1000));
    assert!(!save_observation(&f.s, &f.id, &source, &later).unwrap());
    let db = f.s.db.lock().unwrap();
    let raw: String = db
        .query_row(
            "SELECT data FROM companion_observations WHERE call_id=?1 AND request_id=?2",
            params![f.id, update["request_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let saved: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(saved["source_replies"], update["source_replies"]);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_observation_revisions WHERE call_id=?1",
            [&f.id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(db);
    later["source_replies"][0]["text"] = json!("changed same revision");
    assert!(save_observation(&f.s, &f.id, &source, &later).is_err());
}

#[tokio::test]
async fn private_source_reply_follows_enrolled_original_action_and_survives_reload_without_resend()
{
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let raw: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT source FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                params![f.id, request],
                |r| r.get(0),
            )
            .unwrap();
    let original: Value = serde_json::from_str(&raw).unwrap();
    let wire = json!({"version":1,"request_id":request,"call_id":f.id,
        "intent_id":original["delegation_id"],"revision":original["source_revision"],
        "occurred_at":timestamp(original["occurred_at"].as_i64().unwrap()),
        "actor":{"id":"owner","label":"owner"},
        "audience":{"kind":"private","experience":"companion","id":f.room}});
    let (mut action, mut update) = private_source_reply_fixture();
    action["request_id"] = json!(request);
    action["receipt"]["request_id"] = json!(request);
    action["receipt"]["source"] = wire.clone();
    action["receipt"]["receipt"]["request_id"] = json!(queue);
    action["receipt"]["receipt"]["source_request_id"] = json!(request);
    update["request_id"] = json!(request);
    update["provenance"]["source_id"] = json!(queue);
    update["connection_id"] = json!("minecraft");
    update["installation_source"] = wire.clone();
    update["installation_source"]["caller"] = json!({"id":"companion","label":"Companion"});
    update["source_replies"][0]["audience"] = wire["audience"].clone();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_actions SET result=?3 WHERE call_id=?1 AND request_id=?2",
            params![f.id, request, action["receipt"].to_string()],
        )
        .unwrap();
    let mut response = fresh_result(&f, vec![]);
    response["actions"] = json!([update.clone()]);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        response.to_string(),
    )
    .unwrap();
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(update.clone())
    );
    assert_eq!(cards(&f).len(), 1);
    assert!(
        cards(&f)[0]["text"]
            .as_str()
            .unwrap()
            .contains("Recorded reply for this private conversation")
    );
    assert!(calls(&f, "decision").is_empty());
    let restarted = AppState::open(f.home.path().into()).unwrap();
    let (configuration, fingerprint) = config(&restarted).unwrap().unwrap();
    let source = &configuration.agents["default"][0];
    assert_eq!(
        tracked_actions(&restarted, &f.id, source, false)
            .unwrap()
            .len(),
        1
    );
    assert!(!save_observation(&restarted, &f.id, source, &update).unwrap());
    assert!(
        !preserve_requested_outcome(&restarted, &f.id, &f.id, source, &update, &fingerprint)
            .unwrap()
    );
    let other = another_call(&f, "other");
    assert!(
        tracked_actions(&restarted, &other, source, false)
            .unwrap()
            .is_empty()
    );
    assert!(
        !preserve_requested_outcome(&restarted, &other, &f.id, source, &update, &fingerprint)
            .unwrap()
    );
    let db = restarted.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let retained: String = db
        .query_row(
            "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id=?2",
            params![f.id, request],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&retained).unwrap(),
        action["receipt"]
    );
    drop(db);
    for (field, value) in [
        ("call_id", json!(other)),
        ("actor", json!("other")),
        ("conversation_id", json!("another-room")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        restarted
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE companion_actions SET source=?2 WHERE request_id=?1",
                params![request, changed.to_string()],
            )
            .unwrap();
        assert!(
            tracked_actions(&restarted, &f.id, source, false)
                .unwrap()
                .is_empty(),
            "{field}"
        );
        assert!(
            !preserve_requested_outcome(&restarted, &f.id, &f.id, source, &update, &fingerprint)
                .unwrap(),
            "{field}"
        );
    }
}

#[test]
fn retained_action_time_uses_exact_original_fragments_not_later_delegation() {
    let f = fixture();
    let (request, _) = retained_action(&f);
    let db = f.s.db.lock().unwrap();
    let raw: String = db
        .query_row(
            "SELECT source FROM companion_actions WHERE request_id=?1",
            [&request],
            |r| r.get(0),
        )
        .unwrap();
    let mut action: Value = serde_json::from_str(&raw).unwrap();
    let did = action["delegation_id"].as_str().unwrap().to_owned();
    let words = action["original_text"].as_str().unwrap();
    assert_eq!(original_human_source_time(&db, &f.id, &did).unwrap(), None);
    let old = now_ms() - 7_000_000;
    let event =
        json!({"role":"user","event_id":"old-human-event","text":words,"start_ms":10,"end_ms":20});
    db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'old-human-event','transcript',?2,?3)",params![f.id,event.to_string(),old]).unwrap();
    let seq = db.last_insert_rowid();
    let source = json!({"version":1,"fragments":[{"seq":seq,"role":"user","event_id":"old-human-event","text":words,"start_ms":10,"end_ms":20}]});
    db.execute(
        "UPDATE companion_delegations SET source=?3 WHERE call_id=?1 AND id=?2",
        params![f.id, did, source.to_string()],
    )
    .unwrap();
    action["time_basis"] = json!("retained_human_transcript");
    action["occurred_at"] = json!(old);
    db.execute(
        "UPDATE companion_actions SET source=?2 WHERE request_id=?1",
        params![request, action.to_string()],
    )
    .unwrap();
    assert_eq!(
        original_human_source_time(&db, &f.id, &did).unwrap(),
        Some(old)
    );
    assert!(original_source_valid(&db, &f.id, &request).unwrap());
    db.execute(
        "UPDATE companion_events SET data=json_set(data,'$.text','Changed fragment') WHERE seq=?1",
        [seq],
    )
    .unwrap();
    assert!(original_human_source_time(&db, &f.id, &did).is_err());
    assert!(!original_source_valid(&db, &f.id, &request).unwrap());
    db.execute(
        "UPDATE companion_delegations SET source='broken' WHERE call_id=?1 AND id=?2",
        params![f.id, did],
    )
    .unwrap();
    assert!(original_human_source_time(&db, &f.id, &did).is_err());
    assert!(!original_source_valid(&db, &f.id, &request).unwrap());
}

#[tokio::test]
async fn requested_status_preserves_old_progress_before_an_undated_backend_summary() {
    let f = fixture();
    let old = timestamp(now_ms() - 4 * 60 * 60 * 1000);
    let observed = timestamp(now_ms());
    let mut task = source_event();
    task["id"] = json!("current-assignment");
    task["kind"] = json!("task_observation");
    // Even a fresh event timestamp must not refresh the nested progress report.
    task["occurred_at"] = json!(observed);
    task["observed_at"] = json!(observed);
    task["actor"] = json!("Jarvis");
    task["provenance"] = json!({"kind":"minecraft_current_task","current_state_snapshot":true});
    task["text"] = json!(json!({"status":"running","progress":{"at":old,"text":"The blue door linkage was unfinished."}}).to_string());
    fresh_result(&f, vec![task]);
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    let summary = "The assignment is running and the blue door linkage is unfinished.";
    crate::companion_delivery::enqueue_batch(&f.s, &f.id, "result:undated", vec![json!({"op":"append","channel":"commentary","event_id":"undated-result","delegation_id":null,"content":summary})]).unwrap();
    // No authenticated quiet surface: neither qualification nor answer bypasses it.
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM companion_surfaces WHERE call_id=?1", [&f.id])
        .unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    assert!(commands.iter().all(|v| v["channel"] == "thinking"));
    let text = commands[0]["content"].as_str().unwrap();
    assert!(text.contains(&format!("progress.at {old}")));
    assert!(text.contains("progress report about 4 hours old at read"));
    assert!(text.contains(&format!("observed {observed}")));
    assert!(text.contains("assignment running"));
    assert!(!text.contains("newly verified progress"));
    assert!(
        commands[1]["content"]
            .as_str()
            .unwrap()
            .contains("missing reports do not prove inactivity")
    );
    let next = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    assert_eq!(next["commands"][0]["content"], summary);
    let db = f.s.db.lock().unwrap();
    let provenance: String = db.query_row("SELECT json_extract(data,'$.provenance.kind') FROM companion_awareness WHERE call_id=?1 LIMIT 1",[&f.id],|r|r.get(0)).unwrap();
    assert_eq!(provenance, "companion_requested_connection_status");
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    assert_eq!(calls(&f, "observer").len(), 1);
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn requested_status_pairs_actual_presence_and_public_message_with_their_source_times() {
    let f = fixture();
    let old = timestamp(now_ms() - 4 * 60 * 60 * 1000);
    let observed = timestamp(now_ms());
    let mut presence = source_event();
    presence["id"] = json!("selected-world-presence");
    presence["kind"] = json!("presence_observation");
    presence["actor"] = json!("Jarvis");
    presence["provenance"] = json!({"kind":"minecraft_public_presence","authority":false});
    presence["text"] = json!(json!({"position":{"x":104.5,"y":64,"z":-112.3},"ready":true,"busy":false,"turn_active":false,
        "meaning":"Observed position and activity flags; not proof of current work or request fulfillment.","players":[{"name":"Coach","position":{"x":105,"y":64,"z":-112},"distance_blocks":0.6},{"name":"Rook","position":null}],"players_omitted":0}).to_string());
    let mut task = source_event();
    task["id"] = json!("selected-world-assignment");
    task["kind"] = json!("task_observation");
    task["actor"] = json!("Jarvis");
    task["observed_at"] = json!(observed);
    task["provenance"] = json!({"kind":"minecraft_current_task","current_state_snapshot":true});
    task["text"] = json!(
        json!({"status":"waiting","progress":{"at":old,"summary":"Earlier bridge report."}})
            .to_string()
    );
    let mut message = source_event();
    message["id"] = json!("recorded-public-line");
    message["kind"] = json!("message");
    message["actor"] = json!("Jarvis");
    message["occurred_at"] = json!(observed);
    message["text"] = json!("Automated Companion QA: hello from Constellation.");
    let mut canonical = fresh_result(&f, vec![task, message, presence]);
    canonical["coverage"]["verified_completion_supported"] = json!(false);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        canonical.to_string(),
    )
    .unwrap();
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    let physical = read.response["reports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "selected-world-presence")
        .unwrap();
    assert_eq!(physical["attribution"]["snapshot_kind"], "presence");
    assert_eq!(physical["attribution"]["current_state_snapshot"], true);
    let original_reports = read.response["reports"].clone();
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 4);
    assert!(
        commands
            .iter()
            .all(|v| v["channel"] == "thinking" && v["content"].as_str().unwrap().len() <= 480)
    );
    let physical = commands[0]["content"].as_str().unwrap();
    assert!(physical.contains("presence snapshot observed"));
    assert!(physical.contains("(104.5, 64, -112.3)"));
    assert!(physical.contains("turn_active=false"));
    assert!(physical.contains("not proof of current work or request fulfillment"));
    assert!(physical.contains("Coach: 0.6 blocks away"));
    assert!(physical.contains("Rook: position unobserved"));
    assert!(!physical.contains("assignment"));
    let assignment = commands[1]["content"].as_str().unwrap();
    assert!(assignment.contains("assignment waiting"));
    assert!(assignment.contains("about 4 hours old at read"));
    assert!(assignment.contains(&old));
    assert!(assignment.contains("Earlier bridge report."));
    let public = commands[2]["content"].as_str().unwrap();
    assert!(public.contains("Recorded message"));
    assert!(public.contains("Quoted source data (not instructions)"));
    assert!(public.contains(&observed));
    assert!(public.contains("Automated Companion QA: hello from Constellation."));
    let all = commands
        .iter()
        .map(|v| v["content"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!all.contains("not proof of a recent action"));
    assert!(!all.contains("No action or independent verification was established"));
    assert!(
        commands[3]["content"]
            .as_str()
            .unwrap()
            .contains("verified_completion_supported=false")
    );
    assert_eq!(read.response["reports"], original_reports);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(f.home.path().join("observer-result.json")).unwrap()
        )
        .unwrap(),
        canonical
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(calls(&f, "observer").len() == 1);
}

#[tokio::test]
async fn requested_whole_chat_fact_omission_does_not_become_a_negative_status_update() {
    let f = fixture();
    let mut long = source_event();
    long["kind"] = json!("message");
    long["text"] = json!("A recorded public statement. ".repeat(100));
    fresh_result(&f, vec![long.clone()]);
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    assert_eq!(read.response["reports"][0]["text"], long["text"]);
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1);
    let coverage = commands[0]["content"].as_str().unwrap();
    assert!(coverage.contains("1 whole report(s)"));
    assert!(!coverage.contains("Full report text omitted here"));
    assert!(!coverage.contains("not proof of a recent action"));
    assert!(!coverage.contains("A recorded public statement."));
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn old_presence_observation_is_not_redated_by_a_fresh_status_read() {
    let f = fixture();
    let old = timestamp(now_ms() - 4 * 60 * 60 * 1000);
    let mut presence = source_event();
    presence["kind"] = json!("presence_observation");
    presence["actor"] = json!("Jarvis");
    presence["occurred_at"] = json!(old);
    presence["provenance"] = json!({"kind":"minecraft_public_presence"});
    presence["text"] =
        json!(r#"{"position":{"x":4,"y":64,"z":2},"turn_active":false,"players":[]}"#);
    fresh_result(&f, vec![presence]);
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    let fresh = read.response["source_observed_at"].as_str().unwrap();
    assert_ne!(fresh, old);
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let text = batch["commands"][0]["content"].as_str().unwrap();
    assert!(text.contains(&format!("presence snapshot observed {old}")));
    assert!(text.contains("about 4 hours old at read"));
    assert!(text.contains(&format!("read at {fresh}")));
    assert!(text.contains("position (4, 64, 2)"));
    assert!(!text.contains(&format!("snapshot observed {fresh}")));
}

#[tokio::test]
async fn requested_qualifiers_revalidate_private_identity_and_policy_before_dispatch() {
    for revoke in ["policy", "audience", "private", "endpoint", "closed"] {
        let f = fixture();
        fresh_result(&f, vec![source_event()]);
        let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
            .await
            .unwrap();
        preserve_requested_status(&f.s, &f.id, &read).unwrap();
        let _rx = quiet_browser(&f);
        match revoke {
            "policy" => rewrite_policy(&f, |v| {
                v["agents"]["default"][0]["owners"] = json!(["other"])
            }),
            "audience" => {
                rewrite_policy(&f, |v| v["agents"]["default"][0]["audiences"] = json!([]))
            }
            "private" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("UPDATE conversations SET shared=1 WHERE id=?1", [&f.room])
                    .unwrap();
            }
            "endpoint" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET endpoint_hash='changed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            "closed" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET state='closed' WHERE id=?1",
                        [&f.id],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            crate::companion_delivery::take_ready(&f.s, &f.id)
                .unwrap()
                .is_none(),
            "{revoke}"
        );
        let state: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM companion_voice_batches WHERE call_id=?1",
                    [&f.id],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(state, "withdrawn", "{revoke}");
    }
}

#[tokio::test]
async fn requested_qualifiers_exclude_private_data_and_mark_whole_fact_omissions() {
    let f = fixture();
    let mut events = Vec::new();
    for i in 0..7 {
        let mut event = source_event();
        event["id"] = json!(format!("report-{i}"));
        event["actor"] = json!("Jarvis");
        if i == 0 {
            event["actor"] = json!("a".repeat(160));
            event["id"] = json!("i".repeat(160));
        }
        events.push(event);
    }
    let mut private = source_event();
    private["id"] = json!("private-other-user");
    private["text"] = json!("Secret private activity");
    private["audience"] = json!({"kind":"private","owner":"other"});
    events.push(private);
    fresh_result(&f, events);
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    assert_eq!(read.response["reports"].as_array().unwrap().len(), 7);
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 5);
    assert!(
        commands
            .iter()
            .all(|c| c["content"].as_str().unwrap().len() <= 480 && c["channel"] == "thinking")
    );
    let all = batch.to_string();
    assert!(
        !all.contains("private-other-user")
            && !all.contains("Secret private activity")
            && !all.contains(&"i".repeat(160))
    );
    assert!(
        commands[4]["content"]
            .as_str()
            .unwrap()
            .contains("3 whole report(s)")
    );
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn requested_status_binding_cannot_move_to_a_different_call_or_changed_policy() {
    for changed in ["call", "policy"] {
        let f = fixture();
        fresh_result(&f, vec![source_event()]);
        let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
            .await
            .unwrap();
        if changed == "policy" {
            rewrite_policy(&f, |v| v["guidance"] = json!("Updated policy"));
        }
        let destination = if changed == "call" {
            "another-call"
        } else {
            f.id.as_str()
        };
        assert!(preserve_requested_status(&f.s, destination, &read).is_err());
        assert!(states(&f).is_empty());
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM companion_voice_batches", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn requested_qualifier_revisions_replace_only_unsent_same_source_context() {
    let f = fixture();
    let mut first = source_event();
    first["actor"] = json!("Jarvis");
    fresh_result(&f, vec![first.clone()]);
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let newer = timestamp(now_ms() - 60_000);
    first["revision"] = json!("second-revision");
    first["occurred_at"] = json!(newer);
    fresh_result(&f, vec![first]);
    let second = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    preserve_requested_status(&f.s, &f.id, &second).unwrap();
    let counts: (i64,i64)=f.s.db.lock().unwrap().query_row("SELECT SUM(state='pending'),SUM(state='superseded') FROM companion_voice_batches WHERE call_id=?1",[&f.id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(counts, (1, 1));
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    assert!(
        batch["commands"][0]["content"]
            .as_str()
            .unwrap()
            .contains(&newer)
    );
    crate::companion_delivery::sent(&f.s, &f.id, batch["batch_id"].as_str().unwrap()).unwrap();
    preserve_requested_status(&f.s, &f.id, &second).unwrap();
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    assert!(calls(&f, "decision").is_empty());
}

#[test]
fn requested_source_age_is_deterministic_and_never_refreshes_invalid_or_future_time() {
    let read = json!(1794247200000_i64); // 2026-11-09T18:00:00Z
    assert_eq!(
        source_age_at_read(&json!("2026-11-09T14:00:00Z"), &read),
        "about 4 hours old at read"
    );
    assert_eq!(
        source_age_at_read(&json!("2026-11-09T06:00:00-08:00"), &read),
        "about 4 hours old at read"
    );
    assert_eq!(
        source_age_at_read(&json!(1794232800000_i64), &read),
        "about 4 hours old at read"
    );
    for (at, expected) in [
        (1794247199999_i64, "less than a minute old at read"),
        (1794247140000, "about 1 minute old at read"),
        (1794243600000, "about 1 hour old at read"),
        (1794160800000, "about 1 day old at read"),
    ] {
        assert_eq!(source_age_at_read(&json!(at), &read), expected);
    }
    for invalid in [
        Value::Null,
        json!("yesterday"),
        json!(0),
        json!(-1),
        json!(i64::MAX),
    ] {
        assert_eq!(source_age_at_read(&invalid, &read), "age unknown at read");
    }
    assert_eq!(
        source_age_at_read(&json!("2026-11-09T18:00:01Z"), &read),
        "future-dated relative to this read; age unknown"
    );
    assert_eq!(
        source_age_at_read(&read, &Value::Null),
        "age unknown at read"
    );
}

#[tokio::test]
async fn explicit_status_reserves_current_physical_and_task_facts_before_historical_chat() {
    let f = fixture();
    let old = timestamp(now_ms() - 4 * 60 * 60 * 1000);
    let mut events: Vec<Value> = (0..20)
        .map(|index| {
            let mut event = source_event();
            event["id"] = json!(format!("historical-{index}"));
            event["occurred_at"] = json!(old);
            event["kind"] = json!("message");
            event
        })
        .collect();
    let mut physical = source_event();
    physical["id"] = json!("current-physical");
    physical["kind"] = json!("presence_observation");
    physical["text"] = json!(r#"{"position":{"x":17,"y":64,"z":3},"players":[],"busy":false}"#);
    events.push(physical);
    let mut task = source_event();
    task["id"] = json!("current-task");
    task["kind"] = json!("task_observation");
    task["observed_at"] = json!(timestamp(now_ms()));
    task["provenance"] = json!({"kind":"minecraft_current_task","current_state_snapshot":true});
    task["text"] = json!(
        json!({"status":"running","progress":{"at":old,"text":"Inspection still pending."}})
            .to_string()
    );
    events.push(task);
    fresh_result(&f, events);
    let read = connection_status(&f.s, &f.id, "minecraft").await.unwrap();
    assert_eq!(read["reports"].as_array().unwrap().len(), 16);
    assert_eq!(read["reports"][0]["id"], "current-task");
    assert_eq!(read["reports"][1]["id"], "current-physical");
    assert_eq!(read["omitted"], 6);
    assert_eq!(read["coverage"]["gap"], true);
    let physical: Value =
        serde_json::from_str(read["reports"][1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(physical["players"], json!([]));
    assert_eq!(physical["position"]["x"], 17);
    let task: Value = serde_json::from_str(read["reports"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(task["progress"]["at"], old);
    assert_eq!(
        read["reports"][0]["attribution"]["connection_id"],
        "minecraft"
    );
}

#[tokio::test]
async fn uncertain_quiet_is_retained_without_voice_context_but_explicit_read_remains_available() {
    let f = fixture();
    std::fs::write(f.home.path().join("decision-mode"), "low-confidence").unwrap();
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(states(&f), vec!["retained"]);
    assert!(rx.try_recv().is_err());
    {
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM companion_awareness_appends WHERE call_id=?1",
                [&f.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    fresh_result(&f, vec![source_event()]);
    let status = connection_status(&f.s, &f.id, "minecraft").await.unwrap();
    assert_eq!(status["reports"][0]["id"], "source-event-1");
    assert_eq!(status["authority"], false);
}

fn add_human_context(f: &Fixture, key: &str, text: &str) {
    f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)",params![f.id,key,json!({"role":"user","text":text}).to_string(),now_ms()-2000]).unwrap();
}

#[tokio::test]
async fn human_context_change_during_settled_decision_requires_a_new_context_evaluation() {
    let f = fixture();
    std::fs::write(f.home.path().join("decision-block"), "").unwrap();
    let s = f.s.clone();
    let id = f.id.clone();
    let h = f.headers.clone();
    let task = tokio::spawn(async move { tick(&s, &id, &h).await });
    entered(&f, "decision").await;
    add_human_context(
        &f,
        "selection",
        "Please use the personal connection, not the shared one.",
    );
    std::fs::remove_file(f.home.path().join("decision-block")).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(states(&f), vec!["pending"]);
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    let mut rx = quiet_browser(&f);
    run(&f).await.unwrap();
    let decisions = calls(&f, "decision");
    assert_eq!(decisions.len(), 2);
    assert_ne!(decisions[0]["request_id"], decisions[1]["request_id"]);
    assert_eq!(
        decisions[1]["transcript"][0]["text"],
        "Please use the personal connection, not the shared one."
    );
    assert!(rx.try_recv().is_ok());
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 2);
}

#[tokio::test]
async fn changed_unsent_context_withdraws_old_parts_without_satisfying_new_append_receipts() {
    let f = fixture();
    let mut event = source_event();
    event["kind"] = json!("presence_observation");
    event["text"] = json!(r#"{"position":{"x":17,"y":64,"z":3},"players":[],"busy":false}"#);
    observations(&f, vec![event]);
    tick(&f.s, &f.id, &f.headers).await.unwrap();
    let old: Vec<String> = {
        let db = f.s.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT event_id FROM companion_awareness_appends WHERE call_id=?1")
            .unwrap();
        st.query_map([&f.id], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(!old.is_empty());
    add_human_context(
        &f,
        "changed-selection",
        "Actually I meant my personal server.",
    );
    let _rx = quiet_browser(&f);
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(states(&f), vec!["pending"]);
    let withdrawn:i64=f.s.db.lock().unwrap().query_row("SELECT COUNT(*) FROM companion_awareness_appends WHERE call_id=?1 AND state='withdrawn'",[&f.id],|r|r.get(0)).unwrap();
    assert_eq!(withdrawn, old.len() as i64);
    tick(&f.s, &f.id, &f.headers).await.unwrap();
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .all(|v| !old.contains(&v["event_id"].as_str().unwrap().to_owned()))
    );
    for id in old {
        provider_event(
            &f.s,
            &f.id,
            &json!({"type":"session.thinking.appended","client_event_id":id}),
        )
        .unwrap();
    }
    assert_eq!(states(&f), vec!["context_queued"]);
    for command in commands {
        provider_event(
            &f.s,
            &f.id,
            &json!({"type":"session.thinking.appended","client_event_id":command["event_id"]}),
        )
        .unwrap();
    }
    assert_eq!(states(&f), vec!["context_accepted"]);
    assert_eq!(calls(&f, "decision").len(), 2);
}

#[tokio::test]
async fn unknown_paid_decision_or_append_is_not_retried_for_a_new_human_context() {
    for unknown in ["decision", "append"] {
        let f = fixture();
        let mut rx = quiet_browser(&f);
        if unknown == "decision" {
            std::fs::write(f.home.path().join("decision-mode"), "fail").unwrap();
        }
        run(&f).await.unwrap();
        if unknown == "append" {
            let command = rx.try_recv().unwrap();
            provider_event(&f.s,&f.id,&json!({"type":"error","error":{"type":"server_error","client_event_id":command["event_id"]}})).unwrap();
        }
        add_human_context(&f, "new-question", "Now tell me about my other connection.");
        run(&f).await.unwrap();
        assert_eq!(calls(&f, "decision").len(), 1, "{unknown}");
        assert!(rx.try_recv().is_err(), "{unknown}");
        assert_eq!(
            states(&f),
            vec![if unknown == "append" {
                "unknown"
            } else {
                "retained"
            }]
        );
    }
}

#[tokio::test]
async fn long_agent_speech_cannot_evict_retained_human_selection_from_decision_evidence() {
    let f = fixture();
    add_human_context(
        &f,
        "world-selection",
        "Only my personal connection, please.",
    );
    {
        let db = f.s.db.lock().unwrap();
        for index in 0..130 {
            db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)",params![f.id,format!("agent-{index}"),json!({"role":"agent","text":"A long answer fragment. "}).to_string(),now_ms()-2000]).unwrap();
        }
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,request_text,created_at) VALUES(?1,'selected-request',0,'completed','retained-message','Only my personal connection, please.',?2)",params![f.id,now_ms()-10000]).unwrap();
    }
    add_human_context(&f, "acknowledgement", "Mm-hm, is that current?");
    run(&f).await.unwrap();
    let decision = &calls(&f, "decision")[0];
    assert!(
        !decision["transcript"]
            .to_string()
            .contains("Only my personal")
    );
    assert_eq!(
        decision["request_context"]["human_fragments"][0],
        json!({"id":"world-selection","text":"Only my personal connection, please."})
    );
    assert_eq!(
        decision["request_context"]["human_fragments"][1]["text"],
        "Mm-hm, is that current?"
    );
    assert_eq!(
        decision["request_context"]["requests"][0]["id"],
        "selected-request"
    );
    assert_eq!(decision["request_context"]["omitted_human_fragments"], 0);
}

#[tokio::test]
async fn independent_relevance_allows_quiet_evidence_and_only_strong_proactive_advice_notifies() {
    for (mode, channel) in [
        ("predicate-quiet", "thinking"),
        ("predicate-notify", "commentary"),
    ] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        let command = rx.try_recv().unwrap();
        assert_eq!(command["channel"], channel);
        assert_eq!(states(&f), vec!["context_queued"]);
        let raw: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT decision FROM companion_awareness WHERE call_id=?1",
                    [&f.id],
                    |r| r.get(0),
                )
                .unwrap();
        let decision: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(decision["assessment"]["relevant_probability"], 0.95);
        assert!(decision.get("confidence").is_none());
        assert!(decision.get("provider_choice").is_none());
        run(&f).await.unwrap();
        assert_eq!(calls(&f, "decision").len(), 1);
    }
}

#[tokio::test]
async fn uncertain_refused_or_malformed_predicate_advice_never_appends_or_retries() {
    for mode in [
        "predicate-uncertain",
        "predicate-malformed",
        "predicate-mismatch",
        "predicate-refused",
    ] {
        let f = fixture();
        std::fs::write(f.home.path().join("decision-mode"), mode).unwrap();
        let mut rx = quiet_browser(&f);
        run(&f).await.unwrap();
        add_human_context(&f, "next-human-context", "Tell me more.");
        run(&f).await.unwrap();
        assert!(rx.try_recv().is_err(), "{mode}");
        assert_eq!(states(&f), vec!["retained"], "{mode}");
        assert_eq!(calls(&f, "decision").len(), 1, "{mode}");
    }
}

#[test]
fn predicate_policy_keeps_raw_probabilities_and_legacy_confidence_distinct() {
    for (relevant, proactive, decision, reason, state, notify) in [
        (0.85, 0.5, "quiet", "relevant_context", "ready", false),
        (0.85, 0.9, "notify", "relevant_proactive", "ready", true),
        (
            0.849,
            0.99,
            "quiet",
            "uncertain_relevance",
            "retained",
            false,
        ),
        (0.2, 0.99, "ignore", "irrelevant", "ignored", false),
    ] {
        let mut value = json!({"assessment":{"kind":"relevance_and_proactive_v1","relevant_probability":relevant,"proactive_probability":proactive},"decision":decision,"reason":reason});
        assert_eq!(decision_policy(&value), Some((state, notify)));
        value["confidence"] = json!(0.99);
        assert!(decision_policy(&value).is_none());
    }
    let mut legacy = json!({"confidence":0.19,"selected_probability":0.39,"provider_choice":"quiet","decision":"later","reason":"low_confidence"});
    assert_eq!(decision_policy(&legacy), Some(("retained", false)));
    legacy["confidence"] = json!(0.95);
    legacy["selected_probability"] = json!(0.95);
    legacy["decision"] = json!("quiet");
    legacy["reason"] = json!("provider_decision");
    assert_eq!(decision_policy(&legacy), Some(("ready", false)));
}

#[test]
fn scoped_saved_observations_select_before_bounds_and_never_fall_back_across_owners() {
    let f = fixture();
    rewrite_policy(&f, |c| {
        let mut home = c["agents"]["default"][0].clone();
        home["id"] = json!("home-server");
        home["label"] = json!("Home");
        let mut private = home.clone();
        private["id"] = json!("other-owner");
        private["owners"] = json!(["someone-else"]);
        c["agents"]["default"]
            .as_array_mut()
            .unwrap()
            .extend([home, private]);
    });
    let (cfg, fingerprint) = config(&f.s).unwrap().unwrap();
    for source in &cfg.agents["default"] {
        for index in 0..if source.id == "minecraft" { 1 } else { 20 } {
            let mut event = source_event();
            event["id"] = json!(format!("{}-{index}", source.id));
            event["text"] = json!(format!("{} report {index}", source.id));
            record_source_event(&f.s, &f.id, source, &event, &fingerprint, false).unwrap();
        }
    }
    let overview = current_observations(&f.s, &f.id, None).unwrap();
    assert_eq!(overview["observations"].as_array().unwrap().len(), 16);
    assert_eq!(overview["sources"].as_array().unwrap().len(), 2);
    assert!(!overview.to_string().contains("other-owner"));
    let selected = current_observations(&f.s, &f.id, Some("minecraft")).unwrap();
    assert_eq!(selected["observations"].as_array().unwrap().len(), 1);
    assert_eq!(selected["observations"][0]["source"]["id"], "minecraft-0");
    assert_eq!(selected["sources"].as_array().unwrap().len(), 1);
    assert_eq!(selected["sources"][0]["connection_id"], "minecraft");
    assert_eq!(selected["omitted"], 0);
    assert!(!selected.to_string().contains("home-server"));
    for scope in ["unknown", "other-owner", "MINECRAFT"] {
        let result = current_observations(&f.s, &f.id, Some(scope)).unwrap();
        assert_eq!(result["status"], "not_configured");
        assert_eq!(result["observations"], json!([]));
        assert_eq!(result["sources"], json!([]));
    }
    rewrite_policy(&f, |c| {
        c["agents"]["default"][0]["owners"] = json!(["other"])
    });
    assert_eq!(
        current_observations(&f.s, &f.id, Some("minecraft")).unwrap()["status"],
        "not_configured"
    );
    assert!(calls(&f, "observer").is_empty());
    assert!(calls(&f, "decision").is_empty());
    assert!(cards(&f).is_empty());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn current_status_and_preserved_voice_context_exclude_history_without_redating_progress() {
    let f = fixture();
    let old = timestamp(now_ms() - 4 * 60 * 60 * 1000);
    let observed = timestamp(now_ms());
    let mut task = source_event();
    task["id"] = json!("current-task");
    task["kind"] = json!("task_observation");
    task["observed_at"] = json!(observed);
    task["actor"] = json!("Jarvis");
    task["provenance"] = json!({"kind":"minecraft_current_task","current_state_snapshot":true,"token":"not-for-model"});
    task["text"] = json!(json!({"status":"running","progress":{"at":old,"text":"The blue door linkage was unfinished."}}).to_string());
    let mut presence = source_event();
    presence["id"] = json!("current-presence");
    presence["kind"] = json!("presence_observation");
    presence["occurred_at"] = json!(observed);
    presence["provenance"] = json!({"kind":"minecraft_public_presence"});
    presence["text"] = json!(
        json!({"position":{"x":10,"y":64,"z":-20},"ready":true,"busy":false,"turn_active":false,"meaning":"Observed position and activity flags; not proof of current work or request fulfillment."})
            .to_string()
    );
    let mut history = source_event();
    history["text"] = json!("Historical peer audit unrelated to the current question.");
    let mut private = history.clone();
    private["id"] = json!("private");
    private["audience"] = json!({"kind":"private","owner":"someone-else"});
    let mut source = fresh_result(&f, vec![history, private, task.clone(), presence.clone()]);
    source["coverage"]["omitted_events"] = json!(7);
    source["coverage"]["verified_completion_supported"] = json!(false);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        source.to_string(),
    )
    .unwrap();
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
        .await
        .unwrap();
    let response = &read.response;
    assert_eq!(response["reports"].as_array().unwrap().len(), 2);
    assert_eq!(response["reports"][0]["id"], "current-task");
    assert_eq!(response["reports"][0]["text"], task["text"]);
    assert_eq!(response["reports"][0]["revision"], task["revision"]);
    assert_eq!(
        response["reports"][0]["attribution"]["observed_at"],
        observed
    );
    assert_eq!(
        response["reports"][0]["provenance"],
        json!({"kind":"minecraft_current_task"})
    );
    assert_eq!(response["reports"][1]["text"], presence["text"]);
    assert_eq!(
        response["reports"][1]["attribution"]["occurred_at"],
        observed
    );
    assert!(response["reports"][1]["attribution"]["observed_at"].is_null());
    assert_eq!(response["source_observed_at"], source["observed_at"]);
    assert_eq!(
        response["selection"],
        json!({"view":"current","status":"typed_current_snapshots","excluded_reports":1})
    );
    assert_eq!(response["omitted"], 0);
    assert_eq!(response["coverage"]["omitted_events"], 7);
    assert_eq!(response["coverage"]["verified_completion_supported"], false);
    let original_response = response.clone();
    assert_eq!(response["presence"], json!({}));
    assert!(!response.to_string().contains("not-for-model"));
    assert!(!response.to_string().contains("Historical peer audit"));
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    let _rx = quiet_browser(&f);
    let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .unwrap();
    let commands = batch["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 3);
    assert!(commands.iter().all(|v| v["channel"] == "thinking"));
    let text = batch.to_string();
    assert!(text.contains(&format!("progress.at {old}")));
    assert!(text.contains("progress report about 4 hours old at read"));
    assert!(text.contains("1 report(s) excluded by selection"));
    assert!(text.contains("not proof of current work or request fulfillment"));
    assert!(text.contains("verified_completion_supported=false"));
    assert_eq!(read.response, original_response);
    assert!(!text.contains("Historical peer audit"));
    assert_eq!(
        calls(&f, "observer"),
        vec![json!({"version":1,"cursor":null,"actions":[]})]
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(cards(&f).is_empty());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn current_status_without_typed_snapshots_is_unknown_and_reports_view_remains_available() {
    let f = fixture();
    let history = source_event();
    let mut generic = history.clone();
    generic["id"] = json!("generic-presence");
    generic["kind"] = json!("presence_observation");
    generic["text"] = json!("idle");
    let mut malformed = history.clone();
    malformed["id"] = json!("malformed-task");
    malformed["kind"] = json!("task_observation");
    malformed["observed_at"] = json!(timestamp(now_ms()));
    malformed["provenance"] =
        json!({"kind":"minecraft_current_task","current_state_snapshot":true});
    fresh_result(&f, vec![history, generic, malformed]);
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
        .await
        .unwrap();
    assert_eq!(read.response["status"], "available");
    assert_eq!(
        read.response["selection"]["status"],
        "no_typed_current_facts"
    );
    assert_eq!(read.response["selection"]["excluded_reports"], 3);
    assert_eq!(read.response["reports"], json!([]));
    assert_eq!(read.response["presence"], json!({}));
    assert_eq!(read.response["omitted"], 0);
    assert_eq!(read.response["coverage"]["gap"], false);
    assert!(
        read.response["note"]
            .as_str()
            .unwrap()
            .contains("unknown, not inactivity")
    );
    preserve_requested_status(&f.s, &f.id, &read).unwrap();
    assert!(states(&f).is_empty());
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    let reports = connection_status(&f.s, &f.id, "minecraft").await.unwrap();
    assert_eq!(reports["reports"].as_array().unwrap().len(), 3);
    assert_eq!(
        reports["presence"],
        json!({"ready":true,"busy":false,"turn_active":false})
    );
    assert!(reports.get("selection").is_none());
    for scope in ["unknown", "MINECRAFT"] {
        assert_eq!(
            super::connection_status(&f.s, &f.id, scope, StatusView::Current)
                .await
                .unwrap()
                .response["status"],
            "not_configured"
        );
    }
    rewrite_policy(&f, |c| {
        c["agents"]["default"][0]["owners"] = json!(["other"])
    });
    assert_eq!(
        super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
            .await
            .unwrap()
            .response["status"],
        "not_configured"
    );
    assert_eq!(calls(&f, "observer").len(), 2);
    assert!(calls(&f, "decision").is_empty());
    assert!(cards(&f).is_empty());
}

#[tokio::test]
async fn current_status_counts_selection_response_bounds_and_source_omissions_separately() {
    let f = fixture();
    let mut events = vec![source_event()];
    for index in 0..17 {
        let mut event = source_event();
        event["id"] = json!(format!("snapshot-{index}"));
        event["kind"] = json!("presence_observation");
        event["provenance"] = json!({"kind":"minecraft_public_presence"});
        event["text"] =
            json!(json!({"turn_active":true,"position":{"x":index,"y":64,"z":0}}).to_string());
        events.push(event);
    }
    let mut source = fresh_result(&f, events);
    source["coverage"]["omitted_events"] = json!(42);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        source.to_string(),
    )
    .unwrap();
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
        .await
        .unwrap();
    assert_eq!(read.response["reports"].as_array().unwrap().len(), 16);
    assert_eq!(read.response["selection"]["excluded_reports"], 1);
    assert_eq!(read.response["omitted"], 1);
    assert_eq!(read.response["coverage"]["omitted_events"], 42);
    assert_eq!(read.response["coverage"]["gap"], true);
    assert!(
        read.response["reports"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["attribution"]["snapshot_kind"] == "presence")
    );
}

#[tokio::test]
async fn current_status_requires_snapshot_time_not_just_a_fresh_source_envelope() {
    let f = fixture();
    let now = timestamp(now_ms());
    let old = timestamp(now_ms() - 3_600_000);
    let mut presence = source_event();
    presence["kind"] = json!("presence_observation");
    presence["provenance"] = json!({"kind":"minecraft_public_presence"});
    presence["text"] = json!(json!({"turn_active":false,"position":null}).to_string());
    let mut fallback = presence.clone();
    fallback["id"] = json!("fresh-occurrence");
    let mut explicit = presence.clone();
    explicit["id"] = json!("fresh-observation-old-semantic-time");
    explicit["occurred_at"] = json!(old);
    explicit["observed_at"] = json!(now);
    let mut stale_explicit = presence.clone();
    stale_explicit["id"] = json!("stale-explicit");
    stale_explicit["observed_at"] = json!(old);
    let mut stale_fallback = presence.clone();
    stale_fallback["id"] = json!("stale-occurrence");
    stale_fallback["occurred_at"] = json!(old);
    let mut task = source_event();
    task["id"] = json!("fresh-task-old-progress");
    task["kind"] = json!("task_observation");
    task["provenance"] = json!({"kind":"minecraft_current_task","current_state_snapshot":true});
    task["observed_at"] = json!(now);
    task["text"] = json!(json!({"status":"running","progress":{"at":old}}).to_string());
    let mut missing = task.clone();
    missing["id"] = json!("missing-task-time");
    missing.as_object_mut().unwrap().remove("observed_at");
    let mut stale_task = task.clone();
    stale_task["id"] = json!("stale-task");
    stale_task["observed_at"] = json!(old);
    let mut future = task.clone();
    future["id"] = json!("future-task");
    future["observed_at"] = json!(timestamp(now_ms() + 60_000));
    fresh_result(
        &f,
        vec![
            fallback,
            explicit,
            stale_explicit,
            stale_fallback,
            task.clone(),
            missing,
            stale_task,
            future,
        ],
    );
    let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
        .await
        .unwrap();
    let ids: Vec<_> = read.response["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            "fresh-task-old-progress",
            "fresh-occurrence",
            "fresh-observation-old-semantic-time"
        ]
    );
    assert_eq!(read.response["selection"]["excluded_reports"], 5);
    assert_eq!(read.response["reports"][0]["text"], task["text"]);
    let reports = connection_status(&f.s, &f.id, "minecraft").await.unwrap();
    assert_eq!(reports["reports"].as_array().unwrap().len(), 8);
    presence.as_object_mut().unwrap().remove("occurred_at");
    fresh_result(&f, vec![presence]);
    let missing_time = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
        .await
        .unwrap();
    assert_eq!(missing_time.response["status"], "unavailable");
    assert_eq!(missing_time.response["reports"], json!([]));
    assert!(calls(&f, "decision").is_empty());
}

#[tokio::test]
async fn empty_current_read_withdraws_only_unsent_same_source_qualifiers() {
    let f = fixture();
    f.s.db
        .lock()
        .unwrap()
        .execute("DELETE FROM companion_surfaces WHERE call_id=?1", [&f.id])
        .unwrap();
    let mut history = source_event();
    history["text"] = json!("HISTORICAL-PEER-MARKER was reported complete yesterday.");
    fresh_result(&f, vec![history]);
    let earlier = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
        .await
        .unwrap();
    preserve_requested_status(&f.s, &f.id, &earlier).unwrap();
    for key in [
        "result:independent-answer",
        "requested-status:other-connection",
    ] {
        crate::companion_delivery::enqueue_batch(&f.s, &f.id, key, vec![json!({"op":"append","channel":"thinking","event_id":key,"delegation_id":null,"content":format!("Independent context {key}")})]).unwrap();
    }
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO companion_voice_batches SELECT 'earlier-unknown',call_id,semantic_key,updates,'unknown',created_at,updated_at FROM companion_voice_batches WHERE call_id=?1 AND semantic_key='requested-status:minecraft' AND state='pending'", [&f.id]).unwrap();
    }
    let receipt_data = |f: &Fixture| -> Vec<String> {
        let db = f.s.db.lock().unwrap();
        let mut statement = db
            .prepare("SELECT data FROM companion_awareness WHERE call_id=?1 ORDER BY event_key")
            .unwrap();
        statement
            .query_map([&f.id], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let before = receipt_data(&f);
    assert!(!before.is_empty());
    let current = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Current)
        .await
        .unwrap();
    assert_eq!(
        current.response["selection"]["status"],
        "no_typed_current_facts"
    );
    preserve_requested_status(&f.s, &f.id, &current).unwrap();
    assert_eq!(receipt_data(&f), before);
    {
        let db = f.s.db.lock().unwrap();
        assert_eq!(db.query_row("SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1 AND semantic_key='requested-status:minecraft' AND state='pending'", [&f.id], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(
            db.query_row(
                "SELECT state FROM companion_voice_batches WHERE id='earlier-unknown'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "unknown"
        );
        assert_eq!(db.query_row("SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1 AND state='superseded'", [&f.id], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1 AND state='pending'",
                [&f.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
    }
    let _rx = quiet_browser(&f);
    for _ in 0..2 {
        let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .unwrap();
        assert!(!batch.to_string().contains("HISTORICAL-PEER-MARKER"));
        assert!(batch.to_string().contains("Independent context"));
    }
    assert!(
        crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .is_none()
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(cards(&f).is_empty());
}

#[tokio::test]
async fn requested_status_qualifications_survive_long_labels_and_player_lists() {
    for view in [StatusView::Current, StatusView::Reports] {
        let f = fixture();
        rewrite_policy(&f, |v| {
            v["agents"]["default"][0]["label"] = json!("Workshop".repeat(10))
        });
        let mut event = source_event();
        event["id"] = json!("selected-presence");
        event["revision"] = json!("original-presence-revision");
        event["kind"] = json!("presence_observation");
        event["actor"] = json!("Jarvis");
        event["provenance"] = json!({"kind":"minecraft_public_presence","authority":false});
        event["text"] = json!(json!({
            "meaning":"Observed position and activity flags; not proof of current work or request fulfillment.",
            "position":{"x":-313.16,"y":73,"z":68.52},
            "ready":true,"busy":false,"turn_active":false,
            "players":(0..10).map(|i| json!({"name":format!("{i}{}", "p".repeat(120)),"distance_blocks":2.5})).collect::<Vec<_>>(),
            "players_omitted":999
        }).to_string());
        let mut source = fresh_result(&f, vec![event.clone()]);
        source["coverage"]["verified_completion_supported"] = json!(false);
        source["coverage"]["work_completion_supported"] = json!(false);
        let canonical = source.to_string();
        std::fs::write(f.home.path().join("observer-result.json"), &canonical).unwrap();
        let read = super::connection_status(&f.s, &f.id, "minecraft", view)
            .await
            .unwrap();
        let original = read.response.clone();
        assert_eq!(original["reports"][0]["text"], event["text"]);
        preserve_requested_status(&f.s, &f.id, &read).unwrap();
        let _rx = quiet_browser(&f);
        let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .unwrap();
        let commands = batch["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 2);
        assert!(
            commands
                .iter()
                .all(|c| c["content"].as_str().unwrap().len() <= 480)
        );
        let presence = commands[0]["content"].as_str().unwrap();
        assert!(presence.contains("not proof of current work or request fulfillment"));
        assert!(presence.contains("player detail(s) omitted"));
        let coverage = commands[1]["content"].as_str().unwrap();
        assert!(coverage.contains("verified_completion_supported=false"));
        assert!(coverage.contains("work_completion_supported=false"));
        assert_eq!(read.response, original);
        assert_eq!(
            std::fs::read_to_string(f.home.path().join("observer-result.json")).unwrap(),
            canonical
        );
        let db = f.s.db.lock().unwrap();
        let mut statement = db
            .prepare("SELECT data FROM companion_awareness WHERE call_id=?1")
            .unwrap();
        let projected = statement
            .query_map([&f.id], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(projected.len(), 2);
        for data in projected {
            let data: Value = serde_json::from_str(&data).unwrap();
            assert_eq!(data["provenance"]["report_id"], event["id"]);
            assert_eq!(data["provenance"]["report_revision"], event["revision"]);
            assert_eq!(data["occurred_at"], event["occurred_at"]);
            assert_eq!(data["audience"], event["audience"]);
            assert_eq!(data["actor"], event["actor"]);
        }
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM companion_actions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(statement);
        drop(db);
        assert!(calls(&f, "decision").is_empty());
        assert_eq!(calls(&f, "observer").len(), 1);
    }
}

#[tokio::test]
async fn requested_status_completion_coverage_preserves_only_explicit_false() {
    for value in [
        None,
        Some(Value::Null),
        Some(json!(true)),
        Some(json!(false)),
        Some(json!("false")),
    ] {
        let f = fixture();
        let mut source = fresh_result(&f, vec![source_event()]);
        if let Some(value) = &value {
            source["coverage"]["verified_completion_supported"] = value.clone();
            source["coverage"]["work_completion_supported"] = value.clone();
        }
        std::fs::write(
            f.home.path().join("observer-result.json"),
            source.to_string(),
        )
        .unwrap();
        let read = super::connection_status(&f.s, &f.id, "minecraft", StatusView::Reports)
            .await
            .unwrap();
        let original = read.response.clone();
        preserve_requested_status(&f.s, &f.id, &read).unwrap();
        let _rx = quiet_browser(&f);
        let batch = crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .unwrap();
        let commands = batch["commands"].as_array().unwrap();
        let text = commands.last().unwrap()["content"].as_str().unwrap();
        for key in ["verified_completion_supported", "work_completion_supported"] {
            assert_eq!(
                text.contains(&format!("{key}=false")),
                value == Some(json!(false))
            );
            assert!(!text.contains(&format!("{key}=true")));
        }
        assert!(
            !commands[0]["content"]
                .as_str()
                .unwrap()
                .contains("not proof of current work or request fulfillment")
        );
        assert_eq!(read.response, original);
        assert!(calls(&f, "decision").is_empty());
    }
}

fn work_resumption_fixture() -> (Value, Value) {
    let (action, mut update) = private_source_reply_fixture();
    let fixture: Value = serde_json::from_str(include_str!(
        "../bridge/test/fixtures/minecraft-work-resumption.json"
    ))
    .unwrap();
    update["work_resumptions"] = fixture["response"]["outcomes"][0]["work_resumptions"].clone();
    update["work_resumptions_omitted"] = json!(0);
    (action, update)
}

#[test]
fn work_resumptions_preserve_recorded_scheduling_without_claiming_execution() {
    let (action, mut update) = work_resumption_fixture();
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let text = observation_text("Workshop", &update);
    assert!(text.contains("scheduled"));
    assert!(text.contains("execution had not started in that record"));
    assert!(text.contains(update["work_resumptions"][0]["task_id"].as_str().unwrap()));
    assert!(!text.contains("work completed"));
    update["work_resumptions"][0]["execution_started"] = json!(true);
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let text = observation_text("Workshop", &update);
    assert!(text.contains("execution start was recorded"));
    assert!(!text.contains("resumed execution is not established"));
    assert!(text.contains("completion is not established"));
    assert_eq!(update["verified"], false);
}

#[test]
fn work_resumptions_reject_malformed_and_non_outcome_projections() {
    let (action, update) = work_resumption_fixture();
    for bad in [
        Value::Null,
        json!({}),
        json!(true),
        json!(vec![update["work_resumptions"][0].clone(); 17]),
    ] {
        let mut next = update.clone();
        next["work_resumptions"] = bad;
        assert!(!observation_valid(&next, std::slice::from_ref(&action)));
    }
    for (key, value) in [
        ("task_id", Value::Null),
        ("task_id", json!("")),
        ("recorded_status", json!("x".repeat(161))),
        ("scheduled", json!(1)),
        ("execution_started", Value::Null),
        ("execution_started", json!(0)),
        ("extra", json!("unexpected")),
    ] {
        let mut next = update.clone();
        next["work_resumptions"][0][key] = value;
        assert!(
            !observation_valid(&next, std::slice::from_ref(&action)),
            "{key}"
        );
    }
    for (key, value) in [
        ("domain_work_resumptions_omitted", json!(1)),
        ("work_resumptions_omitted", json!(true)),
    ] {
        let mut next = update.clone();
        next[key] = value;
        assert!(
            !observation_valid(&next, std::slice::from_ref(&action)),
            "{key}"
        );
    }
    let mut unscoped = outcome_update("unknown", "unknown", Value::Null);
    unscoped["provenance"]["kind"] = json!("minecraft_observation");
    unscoped["work_resumptions"] = update["work_resumptions"].clone();
    assert!(!observation_valid(
        &unscoped,
        &[json!({"request_id":"original-call-request"})]
    ));
    for key in ["call_id", "revision", "actor", "audience", "caller"] {
        let mut next = update.clone();
        next["installation_source"][key] = json!("another");
        assert!(
            !observation_valid(&next, std::slice::from_ref(&action)),
            "{key}"
        );
    }
}

#[test]
fn work_resumptions_optional_unknown_and_whole_omission_remain_truthful() {
    let (action, original) = private_source_reply_fixture();
    assert!(observation_valid(&original, std::slice::from_ref(&action)));
    assert!(!observation_text("Workshop", &original).contains("Recorded resumption"));
    let mut update = outcome_update("unknown", "unknown", Value::Null);
    update["domain_work_resumptions_omitted"] = json!(true);
    assert!(observation_valid(
        &update,
        &[json!({"request_id":"original-call-request"})]
    ));
    let text = observation_text("Workshop", &update);
    assert!(text.contains("resumption evidence is incomplete"));
    assert!(text.contains("outcome remains unknown"));
    assert!(!text.contains("execution had not started"));
}

#[test]
fn work_resumptions_changed_facts_retained_once_and_poll_time_deduplicated() {
    let f = fixture();
    let source = config(&f.s).unwrap().unwrap().0.agents["default"][0].clone();
    let (_, mut update) = work_resumption_fixture();
    assert!(save_observation(&f.s, &f.id, &source, &update).unwrap());
    update["observed_at"] = json!(timestamp(now_ms() + 1000));
    assert!(!save_observation(&f.s, &f.id, &source, &update).unwrap());
    update["work_resumptions"][0]["execution_started"] = json!(true);
    assert!(save_observation(&f.s, &f.id, &source, &update).is_err());
    update["revision"] = json!("changed-resumption-fact");
    assert!(save_observation(&f.s, &f.id, &source, &update).unwrap());
    assert!(!save_observation(&f.s, &f.id, &source, &update).unwrap());
    let saved = action_observation(&f.s, &f.id, update["request_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(saved["work_resumptions"], update["work_resumptions"]);
    assert_eq!(saved["installation_source"], update["installation_source"]);
}

#[test]
fn work_resumptions_announcement_keeps_whole_records_and_marks_omissions() {
    let (action, mut update) = work_resumption_fixture();
    update["work_resumptions"] = json!(
        (0..16)
            .map(|index| json!({
                "task_id":format!("{index}{}", "界".repeat(50)),
                "recorded_status":"界".repeat(50),
                "scheduled":true,"execution_started":false
            }))
            .collect::<Vec<_>>()
    );
    assert!(observation_valid(&update, std::slice::from_ref(&action)));
    let text = observation_text("Workshop", &update);
    let count = text.matches("Recorded resumption for task").count();
    assert!(count > 0 && count < 16);
    assert_eq!(
        count,
        text.matches("execution had not started in that record")
            .count()
    );
    assert!(text.contains("resumption evidence is incomplete"));
    assert!(text.contains("not a current activity check"));
    assert!(!text.contains("No resumption"));
}

#[test]
fn work_resumptions_actual_mixed_python_projection_fits_bound_with_honest_whole_omissions() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../bridge/test/fixtures/minecraft-work-resumption-projection.json"
    ))
    .unwrap();
    let update = &fixture["observation"];
    assert!(observation_valid(
        update,
        std::slice::from_ref(&fixture["action"])
    ));
    assert!(update.to_string().len() <= 16000);
    assert_eq!(update["responses"].as_array().unwrap().len(), 1);
    assert_eq!(update["source_replies"].as_array().unwrap().len(), 1);
    assert_eq!(update["source_replies_omitted"], 1);
    assert!(update["work_resumptions_omitted"].as_u64().unwrap() > 0);
    assert_eq!(update["domain_work_resumptions_omitted"], true);
    let text = observation_text("Workshop", update);
    assert!(text.contains("not a current activity check"));
    assert!(text.contains("resumption evidence is incomplete"));
    assert!(
        text.contains("Some returned private replies are not included here")
            || text.contains("available private reply record is incomplete")
    );
    assert!(!text.contains("STILL NOT COMPLETION"));
}

fn cumulative_response(f: &Fixture, update: &Value) {
    let mut response = fresh_result(f, vec![]);
    response["actions"] = json!([update]);
    std::fs::write(
        f.home.path().join("observer-result.json"),
        response.to_string(),
    )
    .unwrap();
}

async fn cumulative_parts(f: &Fixture) -> Vec<Value> {
    tick(&f.s, &f.id, &f.headers).await.unwrap();
    crate::companion_delivery::take_ready(&f.s, &f.id)
        .unwrap()
        .map(|batch| batch["commands"].as_array().unwrap().clone())
        .unwrap_or_default()
}

#[tokio::test]
async fn cumulative_outcome_projects_new_reply_after_exact_context_acceptance() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut update = returned_outcome(&f, &request, &queue, true);
    update["responses"][0]["text"] = json!("Existing Maker reply belongs to this request.");
    cumulative_response(&f, &update);
    let first = cumulative_parts(&f).await;
    assert!(first.iter().any(|part| {
        part["content"]
            .as_str()
            .unwrap()
            .contains("Existing Maker reply")
    }));
    for command in &first {
        provider_event(
            &f.s,
            &f.id,
            &json!({"type":"session.thinking.appended", "client_event_id":command["event_id"]}),
        )
        .unwrap();
    }
    assert_eq!(states(&f), vec!["context_accepted"]);
    assert!(
        cumulative_parts(&f).await.is_empty(),
        "unchanged revision must not replay"
    );
    update["revision"] = json!("cumulative-next");
    update["responses"].as_array_mut().unwrap().push(json!({"id":"new-reply","audience":"public","at":timestamp(now_ms()),"text":"New source fact: the doorway is clear."}));
    cumulative_response(&f, &update);
    let next = cumulative_parts(&f).await;
    let text = next
        .iter()
        .map(|part| part["content"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("New source fact"));
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(update.clone())
    );
    let read = action_observation_for_call(&f.s, &f.id, &request)
        .unwrap()
        .unwrap();
    assert!(
        read.to_string().contains("Existing Maker reply"),
        "full READ must retain canonical earlier reply"
    );
    assert!(
        !text.contains("Existing Maker reply"),
        "the exact already accepted source reply must not enter voice context again: {text}"
    );
}

#[tokio::test]
async fn cumulative_outcome_keeps_unknown_reply_available_without_retrying_old_append() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut update = returned_outcome(&f, &request, &queue, true);
    update["responses"][0]["text"] = json!("Uncertain earlier reply remains available.");
    cumulative_response(&f, &update);
    let first = cumulative_parts(&f).await;
    for command in &first {
        provider_event(&f.s, &f.id, &json!({"type":"error", "error":{"type":"server_error","client_event_id":command["event_id"]}})).unwrap();
    }
    assert_eq!(states(&f), vec!["unknown"]);
    assert!(cumulative_parts(&f).await.is_empty());
    update["revision"] = json!("new-cumulative-unknown");
    update["conversation"]["blocked_reason"] = json!("new_observed_blocker");
    cumulative_response(&f, &update);
    let next = cumulative_parts(&f).await;
    assert!(next.iter().any(|part| {
        part["content"]
            .as_str()
            .unwrap()
            .contains("Uncertain earlier reply")
    }));
    assert!(
        next.iter()
            .all(|part| first.iter().all(|old| old["event_id"] != part["event_id"]))
    );
    assert!(
        states(&f).contains(&"unknown".to_owned()),
        "old uncertain provider receipt is not upgraded"
    );
}

fn accept_cumulative_parts(f: &Fixture, parts: &[Value]) {
    for command in parts {
        provider_event(
            &f.s,
            &f.id,
            &json!({"type":"session.thinking.appended", "client_event_id":command["event_id"]}),
        )
        .unwrap();
    }
}

fn cumulative_text(parts: &[Value]) -> String {
    parts
        .iter()
        .map(|part| part["content"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn cumulative_outcome_keeps_same_id_changes_and_reversion_as_new_evidence() {
    for field in ["text", "at", "actor", "id"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = json!("Original reply identity.");
        cumulative_response(&f, &update);
        let original = update["responses"][0].clone();
        accept_cumulative_parts(&f, &cumulative_parts(&f).await);
        update["revision"] = json!("changed-reply");
        update["responses"][0][field] = match field {
            "text" => json!("Corrected reply identity."),
            "at" => json!("2026-11-09T00:00:01Z"),
            "actor" => json!("another-recorded-actor"),
            _ => json!("different-reply-id"),
        };
        cumulative_response(&f, &update);
        let changed = cumulative_parts(&f).await;
        assert!(
            cumulative_text(&changed).contains("reply identity"),
            "changed {field}"
        );
        accept_cumulative_parts(&f, &changed);
        if field != "id" {
            update["revision"] = json!("reverted-reply");
            update["responses"][0] = original;
            cumulative_response(&f, &update);
            assert!(
                cumulative_text(&cumulative_parts(&f).await).contains("Original reply identity"),
                "reverted {field}"
            );
        }
    }
}

#[tokio::test]
async fn cumulative_outcome_retains_new_state_without_inventing_missing_reply() {
    for (delivery, state) in [
        ("admitted", "admitted"),
        ("terminal", "settled"),
        ("interrupted", "interrupted"),
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = json!("Already available reply.");
        cumulative_response(&f, &update);
        accept_cumulative_parts(&f, &cumulative_parts(&f).await);
        update["revision"] = json!("new-state");
        update["delivery_state"] = json!(delivery);
        update["state"] = json!(state);
        update["conversation"]["blocked_reason"] = json!("fresh_observed_blocker");
        cumulative_response(&f, &update);
        let text = cumulative_text(&cumulative_parts(&f).await);
        assert!(!text.contains("Already available reply"));
        assert!(text.contains("fresh_observed_blocker"));
        assert!(!text.contains("No public reply") && !text.contains("later response is pending"));
        if state == "interrupted" {
            assert!(text.contains("Handling was interrupted"));
        }
        assert!(
            cards(&f).last().unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("Already available reply")
        );
    }
}

#[tokio::test]
async fn cumulative_outcome_does_not_credit_partial_unknown_qualification_or_fake_ack() {
    for mode in [
        "qualification-unknown",
        "wrong-channel",
        "missing-command",
        "missing-revision",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = json!("Partially supplied reply.");
        cumulative_response(&f, &update);
        let first = cumulative_parts(&f).await;
        for (index, part) in first.iter().enumerate() {
            if mode == "qualification-unknown" && index == 1 {
                provider_event(&f.s, &f.id, &json!({"type":"error", "error":{"type":"server_error", "client_event_id":part["event_id"]}})).unwrap();
            } else {
                let kind = if mode == "wrong-channel" {
                    "session.commentary.appended"
                } else {
                    "session.thinking.appended"
                };
                provider_event(
                    &f.s,
                    &f.id,
                    &json!({"type":kind,"client_event_id":part["event_id"]}),
                )
                .unwrap();
            }
        }
        if mode == "missing-command" {
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "DELETE FROM companion_voice_batches WHERE call_id=?1",
                    [&f.id],
                )
                .unwrap();
        }
        if mode == "missing-revision" {
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "DELETE FROM companion_observation_revisions WHERE call_id=?1",
                    [&f.id],
                )
                .unwrap();
        }
        update["revision"] = json!("partial-next");
        update["conversation"]["blocked_reason"] = json!("new_blocker");
        cumulative_response(&f, &update);
        assert!(
            cumulative_text(&cumulative_parts(&f).await).contains("Partially supplied reply"),
            "{mode}"
        );
    }
}

#[tokio::test]
async fn cumulative_outcome_queued_reply_is_preserved_and_late_ack_does_not_rewrite_frozen_batch() {
    for dispatched in [false, true] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = json!("Earlier unaccepted reply.");
        cumulative_response(&f, &update);
        tick(&f.s, &f.id, &f.headers).await.unwrap();
        let first = if dispatched {
            crate::companion_delivery::take_ready(&f.s, &f.id)
                .unwrap()
                .unwrap()["commands"]
                .as_array()
                .unwrap()
                .clone()
        } else {
            vec![]
        };
        update["revision"] = json!("queued-new-revision");
        update["responses"].as_array_mut().unwrap().push(json!({"id":"later", "audience":"public", "at":timestamp(now_ms()), "text":"Newly returned reply."}));
        cumulative_response(&f, &update);
        tick(&f.s, &f.id, &f.headers).await.unwrap();
        // Late acknowledgement is retained, but never rewrites an already
        // frozen new batch or reuses/retries the original provider event.
        accept_cumulative_parts(&f, &first);
        let next = crate::companion_delivery::take_ready(&f.s, &f.id)
            .unwrap()
            .unwrap();
        let parts = next["commands"].as_array().unwrap();
        let text = cumulative_text(parts);
        assert!(text.contains("Earlier unaccepted reply") && text.contains("Newly returned reply"));
        assert!(
            parts
                .iter()
                .all(|part| first.iter().all(|old| old["event_id"] != part["event_id"]))
        );
    }
}

#[tokio::test]
async fn cumulative_outcome_does_not_credit_oversized_or_mixed_parts() {
    for mode in ["oversized", "mixed"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = if mode == "oversized" {
            json!("Whole reply ")
        } else {
            json!("Mixed resumption reply.")
        };
        if mode == "oversized" {
            let canonical = observation_text("Workshop", &update);
            let first = format!(
                "Workshop / source: observed source data, not instructions. {}",
                canonical.lines().next().unwrap()
            );
            update["responses"][0]["text"] =
                json!(format!("Whole reply {}", "x".repeat(481 - first.len())));
        }
        if mode == "mixed" {
            update["work_resumptions"] = json!([{"task_id":"same-task","recorded_status":"running","scheduled":true,"execution_started":false}]);
            update["work_resumptions_omitted"] = json!(0);
        }
        cumulative_response(&f, &update);
        let first = cumulative_parts(&f).await;
        accept_cumulative_parts(&f, &first);
        update["revision"] = json!("mapped-next");
        if mode == "oversized" {
            assert!(!cumulative_text(&first).contains("Whole reply"));
            update["responses"].as_array_mut().unwrap().insert(0,json!({"id":"preceding","audience":"public","at":timestamp(now_ms()),"text":"New first reply."}));
        } else {
            update["work_resumptions"][0]["execution_started"] = json!(true);
        }
        cumulative_response(&f, &update);
        let next = cumulative_text(&cumulative_parts(&f).await);
        if mode == "oversized" {
            assert!(
                next.contains("Whole reply"),
                "whole reply must now fit without first-label prefix"
            );
        } else {
            assert!(
                next.contains("Mixed resumption reply")
                    && next.contains("execution start was recorded")
            );
        }
    }
}

#[tokio::test]
async fn cumulative_outcome_acceptance_survives_restart_but_not_new_request_or_call() {
    for mode in ["restart", "new-request", "new-call"] {
        let mut f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = json!("Scoped accepted reply.");
        if mode == "new-call" {
            update["delivery_state"] = json!("admitted");
            update["state"] = json!("admitted");
            update["conversation"]["terminal_at"] = Value::Null;
        }
        cumulative_response(&f, &update);
        accept_cumulative_parts(&f, &cumulative_parts(&f).await);
        if mode == "restart" {
            f.s = AppState::open(f.home.path().into()).unwrap();
        } else if mode == "new-request" {
            let (new_request, new_queue) = retained_action(&f);
            let reply = update["responses"].clone();
            update = returned_outcome(&f, &new_request, &new_queue, true);
            update["responses"] = reply;
        } else {
            f.id = another_call(&f, "owner");
            let _ = quiet_browser(&f);
        }
        update["revision"] = json!("scoped-next");
        update["delivery_state"] = json!("terminal");
        update["state"] = json!("settled");
        cumulative_response(&f, &update);
        let next = cumulative_text(&cumulative_parts(&f).await);
        assert_eq!(
            next.contains("Scoped accepted reply"),
            mode != "restart",
            "{mode}"
        );
    }
}

#[tokio::test]
async fn cumulative_outcome_earlier_accepted_fact_survives_projected_middle_revision() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut update = returned_outcome(&f, &request, &queue, true);
    update["responses"][0]["text"] = json!("Reply one.");
    for index in 1..=3 {
        update["revision"] = json!(format!("reply-revision-{index}"));
        if index > 1 {
            update["responses"].as_array_mut().unwrap().push(json!({"id":format!("reply-{index}"),"audience":"public","at":timestamp(now_ms()),"text":format!("Reply {index}.")}));
        }
        cumulative_response(&f, &update);
        let parts = cumulative_parts(&f).await;
        let text = cumulative_text(&parts);
        if index == 1 {
            assert!(text.contains("Reply one."));
        } else {
            assert!(!text.contains("Reply one."));
            assert!(text.contains(&format!("Reply {index}.")));
        }
        if index == 3 {
            assert!(!text.contains("Reply 2."));
        }
        accept_cumulative_parts(&f, &parts);
    }
    assert_eq!(
        action_observation(&f.s, &f.id, &request).unwrap(),
        Some(update)
    );
}

fn cumulative_private_outcome(f: &Fixture, request: &str, queue: &str) -> Value {
    let raw: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT source FROM companion_actions WHERE call_id=?1 AND request_id=?2",
                params![f.id, request],
                |r| r.get(0),
            )
            .unwrap();
    let original: Value = serde_json::from_str(&raw).unwrap();
    let wire = json!({"version":1,"request_id":request,"call_id":f.id,
        "intent_id":original["delegation_id"],"revision":original["source_revision"],
        "occurred_at":timestamp(original["occurred_at"].as_i64().unwrap()),
        "actor":{"id":"owner","label":"owner"},
        "audience":{"kind":"private","experience":"companion","id":f.room}});
    let (mut action, mut update) = private_source_reply_fixture();
    action["request_id"] = json!(request);
    action["receipt"]["request_id"] = json!(request);
    action["receipt"]["source"] = wire.clone();
    action["receipt"]["receipt"]["request_id"] = json!(queue);
    action["receipt"]["receipt"]["source_request_id"] = json!(request);
    update["request_id"] = json!(request);
    update["provenance"]["source_id"] = json!(queue);
    update["connection_id"] = json!("minecraft");
    update["installation_source"] = wire.clone();
    update["installation_source"]["caller"] = json!({"id":"companion","label":"Companion"});
    update["source_replies"][0]["audience"] = wire["audience"].clone();
    update["source_replies"][0]["text"] = json!("Private exact-source reply.");
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_actions SET result=?3 WHERE call_id=?1 AND request_id=?2",
            params![f.id, request, action["receipt"].to_string()],
        )
        .unwrap();
    update
}

#[tokio::test]
async fn cumulative_outcome_private_reply_uses_exact_source_and_changed_association() {
    for mode in ["same", "association", "audience", "actor", "policy"] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = cumulative_private_outcome(&f, &request, &queue);
        cumulative_response(&f, &update);
        let first = cumulative_parts(&f).await;
        assert!(cumulative_text(&first).contains("Private exact-source reply"));
        accept_cumulative_parts(&f, &first);
        let original = update.clone();
        update["revision"] = json!("private-next");
        update["responses"] = json!([{"id":"new-public","audience":"public","at":timestamp(now_ms()),"text":"New public evidence."}]);
        match mode {
            "association" => update["domain_response_association"] = json!("shared_turn"),
            "audience" => {
                update["source_replies"][0]["audience"]["id"] = json!("different-private-room")
            }
            "actor" => update["installation_source"]["actor"]["id"] = json!("different-human"),
            "policy" => rewrite_policy(&f, |policy| {
                policy["guidance"] = json!("Changed advisory policy.")
            }),
            _ => {}
        }
        cumulative_response(&f, &update);
        let next = cumulative_parts(&f).await;
        if matches!(mode, "audience" | "actor") {
            assert!(next.is_empty());
            assert_eq!(
                action_observation(&f.s, &f.id, &request).unwrap(),
                Some(original)
            );
        } else {
            let text = cumulative_text(&next);
            assert!(text.contains("New public evidence"));
            assert_eq!(
                text.contains("Private exact-source reply"),
                mode != "same",
                "{mode}"
            );
            assert_eq!(
                action_observation(&f.s, &f.id, &request).unwrap(),
                Some(update)
            );
        }
    }
}

#[tokio::test]
async fn cumulative_outcome_ambiguous_identical_replies_are_not_silently_collapsed() {
    let f = fixture();
    let (request, queue) = retained_action(&f);
    let mut update = returned_outcome(&f, &request, &queue, true);
    update["responses"][0]["text"] = json!("Same content, distinct source occurrences.");
    cumulative_response(&f, &update);
    accept_cumulative_parts(&f, &cumulative_parts(&f).await);
    update["revision"] = json!("ambiguous-next");
    let mut duplicate = update["responses"][0].clone();
    duplicate["id"] = json!("another-occurrence");
    update["responses"].as_array_mut().unwrap().push(duplicate);
    cumulative_response(&f, &update);
    assert_eq!(
        cumulative_text(&cumulative_parts(&f).await)
            .matches("Same content, distinct source occurrences")
            .count(),
        2
    );
}

#[tokio::test]
async fn cumulative_outcome_requires_retained_exact_command_and_source_correlation() {
    for mode in [
        "command-content",
        "malformed-command-json",
        "command-id",
        "command-channel",
        "batch-pending",
        "foreign-source",
        "semantic-key",
        "canonical-update-missing-request",
        "canonical-update-scalar-provenance",
        "canonical-event",
    ] {
        let f = fixture();
        let (request, queue) = retained_action(&f);
        let mut update = returned_outcome(&f, &request, &queue, true);
        update["responses"][0]["text"] = json!("Exact correlated reply.");
        cumulative_response(&f, &update);
        let first = cumulative_parts(&f).await;
        accept_cumulative_parts(&f, &first);
        {
            let db = f.s.db.lock().unwrap();
            if mode == "foreign-source" {
                db.execute(
                    "UPDATE companion_awareness SET connection_id='other-source' WHERE call_id=?1",
                    [&f.id],
                )
                .unwrap();
            } else if mode == "semantic-key" {
                db.execute("UPDATE companion_voice_batches SET semantic_key='awareness:other:unrelated' WHERE call_id=?1",[&f.id]).unwrap();
            } else if mode == "canonical-update-missing-request" {
                db.execute("UPDATE companion_observation_revisions SET data=json_remove(data,'$.request_id') WHERE call_id=?1",[&f.id]).unwrap();
            } else if mode == "canonical-update-scalar-provenance" {
                db.execute("UPDATE companion_observation_revisions SET data=json_set(data,'$.provenance',1) WHERE call_id=?1",[&f.id]).unwrap();
            } else if mode == "canonical-event" {
                db.execute("UPDATE companion_awareness SET data=json_set(data,'$.text','invented projection') WHERE call_id=?1",[&f.id]).unwrap();
            } else {
                let (batch, raw): (String, String) = db
                    .query_row(
                        "SELECT id,updates FROM companion_voice_batches WHERE call_id=?1",
                        [&f.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                let mut commands: Value = serde_json::from_str(&raw).unwrap();
                let reply = &mut commands.as_array_mut().unwrap()[0];
                match mode {
                    "command-content" => {
                        reply["content"] =
                            json!("Different bytes; cannot prove the canonical fact was accepted.")
                    }
                    "command-id" => reply["event_id"] = json!("different-uncorrelated-event"),
                    "command-channel" => reply["channel"] = json!("commentary"),
                    _ => {}
                }
                db.execute(
                    "UPDATE companion_voice_batches SET updates=?2,state=?3 WHERE id=?1",
                    params![
                        batch,
                        if mode == "malformed-command-json" {
                            "malformed retained bytes".to_owned()
                        } else {
                            commands.to_string()
                        },
                        if mode == "batch-pending" {
                            "pending"
                        } else {
                            "dispatching"
                        }
                    ],
                )
                .unwrap();
            }
        }
        update["revision"] = json!("correlation-next");
        update["conversation"]["blocked_reason"] = json!("new_blocker");
        cumulative_response(&f, &update);
        assert!(
            cumulative_text(&cumulative_parts(&f).await).contains("Exact correlated reply"),
            "{mode}"
        );
    }
}

#[tokio::test]
async fn cumulative_outcome_policy_change_cannot_hide_intervening_correction() {
    let f = fixture();
    let policy_path = f.home.path().join("companion-awareness.json");
    let original_policy = std::fs::read(&policy_path).unwrap();
    let (request, queue) = retained_action(&f);
    let mut update = returned_outcome(&f, &request, &queue, true);
    update["responses"][0]["text"] = json!("Earlier version A.");
    cumulative_response(&f, &update);
    accept_cumulative_parts(&f, &cumulative_parts(&f).await);
    rewrite_policy(&f, |policy| {
        policy["guidance"] = json!("Revised relevance guidance.")
    });
    update["revision"] = json!("policy-b");
    update["responses"][0]["text"] = json!("Changed version B.");
    cumulative_response(&f, &update);
    accept_cumulative_parts(&f, &cumulative_parts(&f).await);
    std::fs::write(policy_path, original_policy).unwrap();
    update["revision"] = json!("policy-a-again");
    update["responses"][0]["text"] = json!("Earlier version A.");
    cumulative_response(&f, &update);
    assert!(cumulative_text(&cumulative_parts(&f).await).contains("Earlier version A"));
}
