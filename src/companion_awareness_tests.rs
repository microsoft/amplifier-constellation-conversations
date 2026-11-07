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
while (p/'observer-block').exists():time.sleep(.005)
print((p/'observer-result.json').read_text())
"#,
    )
    .unwrap();
    std::fs::write(home.path().join("decision.py"),r#"import json,pathlib,sys,time
p=pathlib.Path(__file__).parent
body=json.load(sys.stdin)
with (p/'decision-calls.jsonl').open('a') as f:f.write(json.dumps(body)+'\n')
(p/'decision-entered').write_text('entered')
while (p/'decision-block').exists():time.sleep(.005)
mode=(p/'decision-mode').read_text() if (p/'decision-mode').exists() else 'quiet'
if mode=='fail':sys.exit(1)
result={'version':1,'request_id':body['request_id'],'event_id':body['event']['id'],'status':'decided','decision':'notify' if mode=='notify' else 'quiet','provider_choice':'notify' if mode=='notify' else 'quiet','confidence':.95,'selected_probability':.95,'model':'gpt-6-luna','reason':'provider_decision','model_requests':1}
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
    f
}
fn observations(f: &Fixture, events: Vec<Value>) {
    std::fs::write(f.home.path().join("observer-result.json"),json!({"version":1,"connection_id":"minecraft","availability":"available","events":events,"actions":[],"cursor":{"sequence":1},"coverage":{"scope":"fixture","complete":false}}).to_string()).unwrap();
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
    tick(&f.s, &f.id, &f.headers).await
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
    assert_eq!(paid["context"]["user_speaking"], Value::Null);
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
        assert_eq!(states(&f), vec!["ready"]);
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
        assert!(
            !states(&f)
                .iter()
                .any(|v| v == "ready" || v == "context_queued"),
            "{removed}"
        );
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
async fn full_source_card_and_pointer_append_preserve_qualifiers_without_unseen_summary() {
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
    let append = rx.try_recv().unwrap();
    assert_eq!(append["op"], "append");
    assert_eq!(append["channel"], "thinking");
    assert!(
        append["content"]
            .as_str()
            .unwrap()
            .contains("Do not paraphrase unseen content")
    );
    assert!(
        !append["content"]
            .as_str()
            .unwrap()
            .contains("Detailed source")
    );
    assert_eq!(append["delegation_id"], Value::Null);
    assert_eq!(states(&f), vec!["context_queued"]);
    run(&f).await.unwrap();
    assert!(rx.try_recv().is_err());
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
async fn speaking_recent_transcript_or_new_delegation_defers_even_high_confidence_notice() {
    for blocked in ["speaking", "transcript", "delegation", "stale-browser"] {
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
                "delegation" => {
                    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'pending',0,'waiting_transcript','not-admitted',?2)",params![f.id,now_ms()]).unwrap();
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
        assert_eq!(states(&f), vec!["ready"], "{blocked}");
        assert_eq!(cards(&f).len(), 1);
    }
}

#[tokio::test]
async fn old_running_work_does_not_starve_context_but_new_delegation_rearms_guard() {
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
    assert!(rx.try_recv().is_err());
    assert!(states(&f).contains(&"ready".to_owned()));
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_delegations SET created_at=?2 WHERE call_id=?1",
            params![f.id, now_ms() - 7000],
        )
        .unwrap();
    run(&f).await.unwrap();
    assert_eq!(rx.try_recv().unwrap()["channel"], "thinking");
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
    for required in ["Rook", "minecraft", "message", "not verified"] {
        assert!(content.contains(required), "{content}");
    }
    assert!(!content.contains("Coach"));
    let current = current_observations(&f.s, &f.id).unwrap();
    assert_eq!(current["observations"][0]["source"], event);
    assert_eq!(current["observations"][0]["attribution"]["actor"], "Rook");
    assert_eq!(calls(&f, "decision").len(), 1);
    assert_eq!(calls(&f, "observer").len(), 1);
}

#[test]
fn observation_read_is_cached_bounded_current_policy_and_honest_about_coverage() {
    let f = fixture();
    let empty = current_observations(&f.s, &f.id).unwrap();
    assert_eq!(empty["status"], "no_saved_facts");
    assert_eq!(empty["sources"][0]["status"], "no_observation");
    assert!(
        empty["note"]
            .as_str()
            .unwrap()
            .contains("do not establish inactivity")
    );
    assert!(current_observations(&f.s, "another-call").is_err());
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
    let current = current_observations(&f.s, &f.id).unwrap();
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
        current_observations(&f.s, &f.id).unwrap()["observations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
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
    let reply = current_observations(&f.s, &f.id).unwrap();
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
    let saved = current_observations(&f.s, &f.id).unwrap();
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
        current_observations(&f.s, &f.id).unwrap()["observations"][0]["fresh"],
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
    assert!(current_observations(&f.s, &f.id).is_err());
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='connected',endpoint_hash='changed' WHERE id=?1",
            [&f.id],
        )
        .unwrap();
    assert!(current_observations(&f.s, &f.id).is_err());
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
    let saved = current_observations(&f.s, &f.id).unwrap();
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
        current_observations(&f.s, &f.id).unwrap()["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["fresh"] == true)
    );
    assert!(calls(&f, "decision").is_empty());
    assert!(rx.try_recv().is_err());
    rewrite_policy(&f, |c| c["agents"]["default"][0]["audiences"] = json!([]));
    let revoked = current_observations(&f.s, &f.id).unwrap();
    assert!(revoked["observations"].as_array().unwrap().is_empty());
    assert_eq!(revoked["sources"][0]["status"], "no_observation");
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "observer").last().unwrap()["cursor"], Value::Null);
    assert!(
        current_observations(&f.s, &f.id).unwrap()["observations"]
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
    let original = json!({"state":"queued","request_id":"request-1","domain_request_id":"domain-1","scope":"task_submission"});
    f.s.db.lock().unwrap().execute("INSERT INTO companion_actions(call_id,request_id,delegation_id,connection_id,instruction,source,state,result,created_at) VALUES(?1,'request-1','delegation-1','minecraft','Human requested the roof.','{}','queued',?2,?3)",params![f.id,original.to_string(),now_ms()]).unwrap();
    let at = now_ms();
    let initial = json!({"request_id":"request-1","connection_id":"minecraft","revision":"a1","state":"accepted","scope":"task","verified":false,"observed_at":timestamp(at),"provenance":{"transport":"fixture","task_id":"task-1"}});
    let send = |update: Value| {
        let p = f.home.path().join("observer-result.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        v["actions"] = json!([update]);
        std::fs::write(p, v.to_string()).unwrap();
    };
    send(initial.clone());
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, "request-1").unwrap(),
        Some(initial.clone())
    );
    assert_eq!(calls(&f, "decision").len(), 1);
    let mut seen_again = initial.clone();
    seen_again["observed_at"] = json!(timestamp(at + 1000));
    send(seen_again);
    run(&f).await.unwrap();
    assert_eq!(calls(&f, "decision").len(), 1);
    let mut rewritten = initial.clone();
    rewritten["state"] = json!("blocked");
    send(rewritten);
    let _ = run(&f).await;
    assert_eq!(
        action_observation(&f.s, &f.id, "request-1").unwrap(),
        Some(initial.clone())
    );
    assert_eq!(calls(&f, "decision").len(), 1);
    let mut older = initial.clone();
    older["revision"] = json!("a0");
    older["observed_at"] = json!(timestamp(at - 2000));
    older["state"] = json!("running");
    send(older);
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, "request-1").unwrap(),
        Some(initial.clone())
    );
    assert_eq!(calls(&f, "decision").len(), 1);
    let mut newest = initial.clone();
    newest["revision"] = json!("a2");
    newest["observed_at"] = json!(timestamp(at + 1000));
    newest["state"] = json!("reported_complete");
    send(newest.clone());
    run(&f).await.unwrap();
    assert_eq!(
        action_observation(&f.s, &f.id, "request-1").unwrap(),
        Some(newest)
    );
    assert_eq!(calls(&f, "decision").len(), 2);
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
                "SELECT result FROM companion_actions WHERE call_id=?1 AND request_id='request-1'",
                [&f.id],
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
        let request_id = uuid::Uuid::new_v4().to_string();
        let receipt = json!({"request_id":request_id,"state":"queued","receipt":{"source_id":"original-source"}});
        f.s.db.lock().unwrap().execute("INSERT INTO companion_actions(call_id,request_id,delegation_id,connection_id,instruction,source,state,result,created_at) VALUES(?1,?2,'delegation-1','minecraft','Original human request.','{}','queued',?3,?4)",params![f.id,request_id,receipt.to_string(),now_ms()]).unwrap();
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
        assert_eq!(
            calls(&f, "decision")[0]["event"]["id"],
            format!("action:{request_id}")
        );
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
