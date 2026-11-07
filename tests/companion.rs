use agent_conversations::{
    auth,
    config::{Agent, Config},
    server,
    state::{AppState, now_ms},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tempfile::TempDir;
use tower::ServiceExt;
struct Fixture {
    home: TempDir,
    s: Arc<AppState>,
    app: Router,
    token: String,
}
fn fixture(mode: &str) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let python = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
    let bridge = home.path().join("bridge.py");
    std::fs::write(&bridge,r#"import json,sys,pathlib
root=pathlib.Path(__file__).parent
def emit(v): print(json.dumps(v),flush=True)
c=json.loads(sys.stdin.readline())
with (root/'bridge-starts').open('a') as f:f.write('start\n')
voice=c['voice']
if voice=='closed_setup':
 emit({'type':'event','event':{'type':'session.closed','event_id':'early-closed','session':{'id':'live-fixture'},'reason':'close_requested','usage':{'seconds':15}}})
 emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});sys.exit(0)
if voice=='early':emit({'type':'event','event':{'type':'session.started','event_id':'started','session':{'id':'live-fixture'}}})
emit({'type':'ready','session_id':'live-fixture','sdp':'v=0\r\nfixture-answer'})
if voice not in ('early','connecting'):emit({'type':'event','event':{'type':'session.started','event_id':'started','session':{'id':'live-fixture'}}})
if voice not in ('quiet','connecting'):
 if voice!='missing':
  event={'type':'session.input_transcript.delta','event_id':'human-1','delta':'Please inspect the supplied test context and report it.','start_ms':10,'end_ms':90}
  emit({'type':'event','event':event});emit({'type':'event','event':event})
 if voice=='overlap_ack':emit({'type':'event','event':{'type':'session.output_transcript.delta','event_id':'assistant-ack','delta':'Let me check.','start_ms':90,'end_ms':110}})
 if voice in ('delegate','missing','overlap_ack'):
  event={'type':'session.delegation.created','event_id':'delegation-1','offset_ms':100,'delegation':{'id':'delegate-1','target':'client'}}
  emit({'type':'event','event':event});emit({'type':'event','event':event})
for line in sys.stdin:
 c=json.loads(line)
 with (root/'bridge-commands').open('a') as f:f.write(json.dumps(c)+'\n')
 if c['op']=='close':emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});break
 if c['op']=='append':
  if not c['content'].strip():emit({'type':'error','code':'invalid_content','message':'Empty content'});continue
  emit({'type':'event','event':{'type':'session.'+c['channel']+'.appended','event_id':'ack-'+c['event_id'],'client_event_id':c['event_id'],'start_ms':100,'end_ms':110}})
"#).unwrap();
    let c = Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Companion tests".into(),
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
            command: python.clone(),
            args: vec![format!(
                "{}/tests/fixture_agent.py",
                env!("CARGO_MANIFEST_DIR")
            )],
            cwd: home.path().to_string_lossy().into(),
            session_cwd: None,
            env: BTreeMap::from([(
                "FIXTURE_LOG".into(),
                home.path().join("acp.jsonl").to_string_lossy().into(),
            )]),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        },
    };
    c.save(home.path()).unwrap();
    std::fs::write(home.path().join("companion.json"),json!({"enabled":true,"node":python,"bridge":bridge,"profiles":{"default":{"voice":mode,"description":"Fixture agent"}},"max_call_seconds":1800}).to_string()).unwrap();
    let s = AppState::open(home.path().into()).unwrap();
    let app = server::router(s.clone());
    let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    Fixture {
        home,
        s,
        app,
        token,
    }
}
async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {}", f.token))
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let r = f.app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let bytes = to_bytes(r.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn start(f: &Fixture) -> (Value, Value) {
    let input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"agent_id":"default","sdp":"v=0\r\nfixture-offer"});
    let (status, value) = request(f, "POST", "/api/v1/companion/calls", Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(["connecting", "connected"].contains(&value["state"].as_str().unwrap()));
    (input, value)
}
async fn until(f: &Fixture, id: &str, kind: &str) -> Value {
    for _ in 0..100 {
        let (_, v) = request(f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        if v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == kind)
        {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("No event {kind}");
}
async fn end(f: &Fixture, id: &str) {
    request(
        f,
        "POST",
        &format!("/api/v1/companion/calls/{id}/close"),
        Some(json!({})),
    )
    .await;
    until(f, id, "closed").await;
}
#[tokio::test]
async fn duplicate_create_never_opens_second_paid_session_and_hangup_does_not_cancel_work() {
    let f = fixture("quiet");
    let (input, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let (status, again) = request(&f, "POST", "/api/v1/companion/calls", Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], call["id"]);
    assert_eq!(again["sdp"], call["sdp"]);
    let mut changed = input.clone();
    changed["sdp"] = json!("v=0\r\nchanged");
    assert_eq!(
        request(&f, "POST", "/api/v1/companion/calls", Some(changed))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut second = input;
    second["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert_eq!(
        request(&f, "POST", "/api/v1/companion/calls", Some(second))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (tx, rx) = tokio::sync::watch::channel(false);
    f.s.running.lock().unwrap().insert(room.into(), tx);
    end(&f, id).await;
    assert!(!*rx.borrow());
    assert!(f.s.running.lock().unwrap().contains_key(room));
    assert_eq!(
        std::fs::read_to_string(f.home.path().join("bridge-starts")).unwrap(),
        "start\n"
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
}
#[tokio::test]
async fn public_fragments_are_retained_without_automatic_backend_prompt() {
    let f = fixture("talk");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let v = until(&f, id, "transcript").await;
    assert_eq!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["kind"] == "transcript")
            .count(),
        1
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    let count: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE status='voice_fragment'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 1);
    assert!(
        !agent_conversations::companion::context_registered(
            &f.s,
            call["conversation_id"].as_str().unwrap()
        )
        .unwrap()
    );
    end(&f, id).await;
}
#[tokio::test]
async fn duplicate_delegation_executes_one_actual_acp_prompt_and_only_settled_result_returns() {
    let f = fixture("delegate");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let v = until(&f, id, "card").await;
    let card = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "card")
        .unwrap();
    assert!(
        card["data"]["text"]
            .as_str()
            .unwrap()
            .contains("Fixture response")
    );
    let lines = std::fs::read_to_string(f.home.path().join("acp.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        lines
            .iter()
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
    let setup = lines.iter().find(|v| v["method"] == "session/new").unwrap();
    assert!(setup["params"]["_meta"]["io.github.bkrabach.conversation"].is_null());
    assert!(
        lines
            .iter()
            .find(|v| v["method"] == "session/prompt")
            .unwrap()["params"]["prompt"]
            .to_string()
            .contains("Please inspect")
    );
    end(&f, id).await;
}
#[tokio::test]
async fn missing_human_transcript_is_held_without_inventing_task_or_opening_acp_session() {
    let f = fixture("missing");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    for _ in 0..100 {
        let (_, v) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        if v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["data"]["state"] == "held")
        {
            assert!(!f.home.path().join("acp.jsonl").exists());
            end(&f, id).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("Missing transcript was not held");
}
#[tokio::test]
async fn call_access_is_owner_bound_and_browser_mutations_require_csrf() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let cookie = "other-cookie";
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'other','csrf',?2)",
            rusqlite::params![auth::digest(cookie), now_ms() + 60_000],
        )
        .unwrap();
    let req = Request::builder()
        .uri(format!("/api/v1/companion/calls/{id}"))
        .header(
            header::COOKIE,
            format!("{}={cookie}", auth::cookie_name(&f.s)),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        f.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let cookie = "owner-cookie";
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'owner','csrf',?2)",
            rusqlite::params![auth::digest(cookie), now_ms() + 60_000],
        )
        .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/companion/calls/{id}/close"))
        .header(
            header::COOKIE,
            format!("{}={cookie}", auth::cookie_name(&f.s)),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        f.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    end(&f, id).await;
}
#[tokio::test]
async fn stale_heartbeat_closes_only_media_and_restart_preserves_unknown_delegation() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET heartbeat_at=?2 WHERE id=?1",
            rusqlite::params![id, now_ms() - 61_000],
        )
        .unwrap();
    until(&f, id, "closed").await;
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='connected' WHERE id=?1",
            [id],
        )
        .unwrap();
    f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'uncertain',100,'dispatched','same-source',?2)",rusqlite::params![id,now_ms()]).unwrap();
    let loaded = AppState::open(f.home.path().into()).unwrap();
    let db = loaded.db.lock().unwrap();
    assert_eq!(
        db.query_row("SELECT state FROM companion_calls WHERE id=?1", [id], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "interrupted"
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM companion_delegations WHERE id='uncertain'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unknown"
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
}

#[tokio::test]
async fn explicit_camera_or_typed_question_uses_existing_delivery_and_returns_one_voice_card() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let source = uuid::Uuid::new_v4().to_string();
    let body = json!({"request_id":source,"text":"Explain the test object","target_agent_ids":["default"],"attachment_ids":[]});
    assert_eq!(
        request(
            &f,
            "POST",
            &format!("/api/v1/conversations/{room}/messages"),
            Some(body.clone())
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    request(
        &f,
        "POST",
        &format!("/api/v1/conversations/{room}/messages"),
        Some(body),
    )
    .await;
    let v = until(&f, id, "card").await;
    let cards = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "card")
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 1);
    assert!(cards[0]["data"]["delegation_id"].is_null());
    tokio::time::sleep(Duration::from_millis(650)).await;
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    assert_eq!(commands.lines().filter(|l| l.contains("append")).count(), 1);
    end(&f, id).await;
}

#[tokio::test]
async fn accepted_backend_work_settles_after_hangup_without_reopening_or_replaying_media() {
    let f = fixture("delegate");
    let slow = f.home.path().join("slow.py");
    std::fs::write(&slow,r#"import json,sys,time,pathlib
root=pathlib.Path(__file__).parent
for line in sys.stdin:
 v=json.loads(line);m=v.get('method');p=v.get('params',{})
 if m=='initialize':r={'protocolVersion':1,'agentCapabilities':{'loadSession':True}}
 elif m in ('session/new','session/load'):r={'sessionId':'slow-original'}
 elif m=='session/prompt':
  (root/'backend-admitted').write_text('accepted');time.sleep(2)
  print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'slow-original','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Settled after hangup'}}}}),flush=True)
  r={'stopReason':'end_turn'}
 else:continue
 print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':r}),flush=True)
"#).unwrap();
    let mut agent = f.s.agent("default").unwrap().unwrap();
    agent.args = vec![slow.to_string_lossy().into()];
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
            [serde_json::to_string(&agent).unwrap()],
        )
        .unwrap();
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    for _ in 0..100 {
        if f.home.path().join("backend-admitted").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(f.home.path().join("backend-admitted").exists());
    end(&f, id).await;
    let v = until(&f, id, "card").await;
    assert_eq!(v["state"], "closed");
    assert!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "card" && e["data"]["text"] == "Settled after hangup")
    );
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    assert_eq!(commands.lines().filter(|l| l.contains("append")).count(), 0);
    assert_eq!(
        std::fs::read_to_string(f.home.path().join("bridge-starts")).unwrap(),
        "start\n"
    );
}

#[tokio::test]
async fn provider_session_started_before_ready_is_buffered_until_matching_setup() {
    let f = fixture("early");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let v = until(&f, id, "provider").await;
    assert_eq!(v["state"], "connected");
    assert_eq!(v["session_id"], "live-fixture");
    end(&f, id).await;
}

#[tokio::test]
async fn error_after_durable_intake_retains_admission_without_false_rejection_or_replay() {
    let f = fixture("delegate");
    f.s.db.lock().unwrap().execute_batch("CREATE TRIGGER fail_intake_audit BEFORE INSERT ON audit WHEN NEW.kind='input.received' BEGIN SELECT RAISE(ABORT,'fixture postcommit audit failure'); END;").unwrap();
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let mut observed = false;
    for _ in 0..100 {
        let (_, v) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        if v["events"].as_array().unwrap().iter().any(|e| {
            e["data"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("request was saved")
        }) {
            observed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(observed);
    {
        let db = f.s.db.lock().unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM input_deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            db.query_row("SELECT state FROM companion_delegations", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "dispatched"
        );
    }
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}

#[tokio::test]
async fn companion_page_allows_local_media_without_relaxing_text_page_or_scripts() {
    let f = fixture("quiet");
    for (path, media) in [("/companion", true), ("/", false)] {
        let r = f
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let csp = r.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert_eq!(csp.contains("img-src 'self' data: blob:"), media);
        assert_eq!(csp.contains("media-src 'self' blob:"), media);
        assert!(csp.contains("script-src 'self';"));
        assert!(csp.contains("connect-src 'self';"));
    }
}

#[tokio::test]
async fn replacing_call_agent_or_endpoint_closes_media_and_never_dispatches_to_substitute() {
    for substitute in [true, false] {
        let f = fixture("delegate");
        let (_, call) = start(&f).await;
        let id = call["id"].as_str().unwrap();
        let room = call["conversation_id"].as_str().unwrap();
        {
            let db = f.s.db.lock().unwrap();
            if substitute {
                db.execute("INSERT INTO agent_endpoints SELECT 'substitute',config,updated_at FROM agent_endpoints WHERE id='default'", []).unwrap();
                db.execute(
                    "UPDATE room_agents SET agent_id='substitute' WHERE conversation_id=?1",
                    [room],
                )
                .unwrap();
            } else {
                let mut config: Value = serde_json::from_str(
                    &db.query_row(
                        "SELECT config FROM agent_endpoints WHERE id='default'",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .unwrap(),
                )
                .unwrap();
                config["args"] = json!(["different-endpoint.py"]);
                db.execute(
                    "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
                    [config.to_string()],
                )
                .unwrap();
            }
        }
        let result = until(&f, id, "closed").await;
        assert_eq!(result["state"], "closed");
        let (status, _) = request(&f, "POST", &format!("/api/v1/conversations/{room}/messages"), Some(json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"Do not send to the substitute"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            f.s.db
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM input_deliveries", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(!f.home.path().join("acp.jsonl").exists());
    }
}

#[tokio::test]
async fn connecting_call_is_owner_discoverable_heartbeated_exclusive_and_restart_interrupted() {
    let f = fixture("connecting");
    let (mut input, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    assert_eq!(call["state"], "connecting");
    let (_, catalog) = request(&f, "GET", "/api/v1/companion", None).await;
    assert_eq!(catalog["active_call"]["id"], id);
    assert_eq!(catalog["active_call"]["state"], "connecting");
    assert!(catalog["active_call"].get("sdp").is_none());
    input["request_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert_eq!(
        request(&f, "POST", "/api/v1/companion/calls", Some(input))
            .await
            .0,
        StatusCode::CONFLICT
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET heartbeat_at=?2 WHERE id=?1",
            rusqlite::params![id, now_ms() - 30_000],
        )
        .unwrap();
    request(
        &f,
        "POST",
        &format!("/api/v1/companion/calls/{id}/heartbeat"),
        Some(json!({})),
    )
    .await;
    let heartbeat: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT heartbeat_at FROM companion_calls WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
    assert!(now_ms() - heartbeat < 2000);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'other','csrf',?2)",
            rusqlite::params![auth::digest("other-active"), now_ms() + 60_000],
        )
        .unwrap();
    let req = Request::builder()
        .uri("/api/v1/companion")
        .header(
            header::COOKIE,
            format!("{}=other-active", auth::cookie_name(&f.s)),
        )
        .body(Body::empty())
        .unwrap();
    let r = f.app.clone().oneshot(req).await.unwrap();
    let other: Value =
        serde_json::from_slice(&to_bytes(r.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    assert!(other["active_call"].is_null());
    std::fs::remove_file(f.home.path().join("companion.json")).unwrap();
    let (_, catalog) = request(&f, "GET", "/api/v1/companion", None).await;
    assert_eq!(catalog["available"], false);
    assert_eq!(catalog["active_call"]["id"], id);
    end(&f, id).await;
    let (_, catalog) = request(&f, "GET", "/api/v1/companion", None).await;
    assert!(catalog["active_call"].is_null());
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_calls SET state='connecting' WHERE id=?1",
            [id],
        )
        .unwrap();
    let loaded = AppState::open(f.home.path().into()).unwrap();
    assert_eq!(
        loaded
            .db
            .lock()
            .unwrap()
            .query_row("SELECT state FROM companion_calls WHERE id=?1", [id], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "interrupted"
    );
}

#[tokio::test]
async fn created_then_closed_before_ready_preserves_provider_identity_and_terminal() {
    let f = fixture("closed_setup");
    let (status,call)=request(&f,"POST","/api/v1/companion/calls",Some(json!({"request_id":uuid::Uuid::new_v4().to_string(),"agent_id":"default","sdp":"v=0\r\nfixture-offer"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(call["state"], "closed");
    assert_eq!(call["session_id"], "live-fixture");
    assert!(call["sdp"].is_null());
    let v = until(&f, call["id"].as_str().unwrap(), "provider").await;
    assert!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "provider"
                && e["data"]["type"] == "session.closed"
                && e["data"]["session"]["id"] == "live-fixture")
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
}
#[tokio::test]
async fn silent_settled_result_is_recorded_without_empty_append_or_call_closure() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    // A terminal delivery fixture models a silent/tool-only public result; no
    // fake human input or model turn is dispatched by this observer test.
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('silent-source',?1,'user','terminal fixture','sent',?2,?2,'owner')",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('silent-result',?1,'assistant','  ','complete',?2,?2,'Coach','default')",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES('silent-delivery','silent-source',?1,'default','direct','completed','silent-result',?2)",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'silent-delegation',100,'dispatched','silent-source',?2)",rusqlite::params![id,now_ms()]).unwrap();
    }
    let v = until(&f, id, "result").await;
    assert_eq!(v["state"], "connected");
    assert!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "delegation"
                && e["data"]["id"] == "silent-delegation"
                && e["data"]["state"] == "completed")
    );
    assert!(
        !v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "card")
    );
    let commands =
        std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap_or_default();
    assert!(!commands.lines().any(|line| line.contains("append")));
    end(&f, id).await;
}
#[tokio::test]
async fn assistant_ack_overlap_does_not_hold_a_settled_human_request() {
    let f = fixture("overlap_ack");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let v = until(&f, id, "card").await;
    assert_eq!(v["state"], "connected");
    assert!(
        !v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "delegation" && e["data"]["state"] == "held")
    );
    let lines = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert_eq!(
        lines
            .lines()
            .filter(
                |line| serde_json::from_str::<Value>(line).unwrap()["method"] == "session/prompt"
            )
            .count(),
        1
    );
    end(&f, id).await;
}
