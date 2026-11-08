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
(root/'bridge-initial.json').write_text(json.dumps(c))
voice=(root/'bridge-mode').read_text()
if voice=='closed_setup':
 emit({'type':'event','event':{'type':'session.closed','event_id':'early-closed','session':{'id':'live-fixture'},'reason':'close_requested','usage':{'seconds':15}}})
 emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});sys.exit(0)
if voice=='early':emit({'type':'event','event':{'type':'session.started','event_id':'started','session':{'id':'live-fixture'}}})
emit({'type':'ready','session_id':'live-fixture','sdp':'v=0\r\nfixture-answer',**({'mcp_port':34567} if c.get('mcp') else {})})
if voice not in ('early','connecting'):emit({'type':'event','event':{'type':'session.started','event_id':'started','session':{'id':'live-fixture'}}})
if voice in ('connection_lost','expired','remote_hangup','terminal_eof'):
 import time
 time.sleep(.1)
 emit({'type':'event','event':{'type':'session.closed','event_id':'provider-terminal','session':{'id':'live-fixture'},'reason':'expired' if voice=='terminal_eof' else voice,'usage':{'seconds':120}}})
 if voice=='terminal_eof':sys.exit(0)
 emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});sys.exit(0)
if voice not in ('quiet','connecting','upload_delegate'):
 if voice!='missing':
  event={'type':'session.input_transcript.delta','event_id':'human-1','delta':'Please inspect the supplied test context and report it.','start_ms':10,'end_ms':90}
  if voice in ('anchor','anchors'):event.update({'delta':'Please inspect the existing work, without changing anything','start_ms':12000,'end_ms':12990})
  emit({'type':'event','event':event});emit({'type':'event','event':event})
 if voice in ('anchor','anchors'):emit({'type':'event','event':{'type':'session.input_transcript.delta','event_id':'human-tail','delta':' elsewhere','start_ms':13000,'end_ms':13200}})
 if voice=='overlap_ack':emit({'type':'event','event':{'type':'session.output_transcript.delta','event_id':'assistant-ack','delta':'Let me check.','start_ms':90,'end_ms':110}})
 if voice in ('delegate','missing','overlap_ack','anchor','anchors','append_error'):
  event={'type':'session.delegation.created','event_id':'delegation-1','offset_ms':100,'delegation':{'id':'delegate-1','target':'client'}}
  if voice in ('anchor','anchors'):event['offset_ms']=13000
  emit({'type':'event','event':event});emit({'type':'event','event':event})
second_sent=False
for line in sys.stdin:
 c=json.loads(line)
 with (root/'bridge-commands').open('a') as f:f.write(json.dumps(c)+'\n')
 if c['op']=='close':emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});break
 if c['op']=='append':
  if not c['content'].strip():emit({'type':'error','code':'invalid_content','message':'Empty content'});continue
  if voice=='append_error':
   if second_sent:continue
   second_sent=True
   for suffix,client,outer in [('unrelated','never-queued',None),('conflict',c['event_id'],'another-command'),('matched',c['event_id'],None)]:
    event={'type':'error','event_id':'provider-'+suffix,'error':{'code':'invalid_value','type':'invalid_request_error','param':'delegation_id','client_event_id':client,'message':'The voice service reported an error.'}}
    if outer:event['client_event_id']=outer
    emit({'type':'event','event':event})
   emit({'type':'error','code':'voice_transport_failed','message':'Generic close'});continue
  emit({'type':'event','event':{'type':'session.'+c['channel']+'.appended','event_id':'ack-'+c['event_id'],'client_event_id':c['event_id'],'start_ms':100,'end_ms':110}})
  if voice=='upload_delegate' and 'is available.' in c['content'] and not second_sent:
   second_sent=True
   emit({'type':'event','event':{'type':'session.delegation.created','event_id':'upload-delegation','offset_ms':100,'delegation':{'id':'delegate-upload','target':'client'}}})
  if voice=='anchors' and not second_sent:
   second_sent=True
   emit({'type':'event','event':{'type':'session.input_transcript.delta','event_id':'human-second','delta':'Now give me a separate summary.','start_ms':15000,'end_ms':15200}})
   emit({'type':'event','event':{'type':'session.delegation.created','event_id':'delegation-2','offset_ms':15100,'delegation':{'id':'delegate-2','target':'client'}}})
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
    std::fs::write(home.path().join("bridge-mode"), mode).unwrap();
    std::fs::write(home.path().join("companion.json"),json!({"enabled":true,"node":python,"bridge":bridge,"profiles":{"default":{"voice":"alloy","description":"Fixture agent"}},"max_call_seconds":1800}).to_string()).unwrap();
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
async fn provider_append_rejection_is_correlated_without_replaying_settled_delegation() {
    let f = fixture("append_error");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let result = until(&f, id, "closed").await;
    let events = result["events"].as_array().unwrap();
    let queued = events
        .iter()
        .find(|e| e["kind"] == "context_delivery" && e["data"]["state"] == "queued")
        .unwrap();
    let receipts = events
        .iter()
        .filter(|e| e["kind"] == "context_delivery" && e["data"]["state"] != "queued")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    let rejection = &receipts[0]["data"];
    assert_eq!(rejection["state"], "rejected");
    assert_eq!(rejection["event_id"], queued["data"]["event_id"]);
    assert_eq!(rejection["delegation_id"], "delegate-1");
    assert_eq!(rejection["channel"], queued["data"]["channel"]);
    assert_eq!(rejection["provider_event_id"], "provider-matched");
    assert_eq!(rejection["error"]["param"], "delegation_id");
    assert!(
        events
            .iter()
            .any(|e| e["kind"] == "delegation" && e["data"]["state"] == "completed")
    );
    assert!(events.iter().any(|e| e["kind"] == "card"));
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    let append_ids = commands
        .lines()
        .map(|v| serde_json::from_str::<Value>(v).unwrap())
        .filter(|v| v["op"] == "append")
        .map(|v| v["event_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(!append_ids.is_empty());
    assert_eq!(
        append_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        append_ids.len()
    );
    let acp = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert_eq!(
        acp.lines()
            .map(|v| serde_json::from_str::<Value>(v).unwrap())
            .filter(|v| v["method"] == "session/prompt")
            .count(),
        1
    );
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
    let appends: Vec<Value> = commands
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|c: &Value| c["op"] == "append")
        .collect();
    assert!(!appends.is_empty());
    assert!(
        appends
            .iter()
            .all(|c| c["content"].as_str().unwrap().len() <= 480)
    );
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

#[tokio::test]
async fn delegation_anchor_includes_observed_straddling_word_with_original_timing() {
    let f = fixture("anchor");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let v = until(&f, id, "card").await;
    assert_eq!(v["state"], "connected");
    let source:String=f.s.db.lock().unwrap().query_row("SELECT m.text FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id='delegate-1'",[id],|r|r.get(0)).unwrap();
    let fragments: Vec<Value> =
        serde_json::from_str(source.rsplit_once("\n\n").unwrap().1).unwrap();
    let tail = fragments
        .iter()
        .find(|v| v["event_id"] == "human-tail")
        .unwrap();
    assert_eq!(tail["text"], " elsewhere");
    assert_eq!(tail["start_ms"], 13000);
    assert_eq!(tail["end_ms"], 13200);
    assert_eq!(tail["crosses_delegation_anchor"], true);
    assert!(
        !v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "delegation" && e["data"]["state"] == "held")
    );
    end(&f, id).await;
}
#[tokio::test]
async fn consecutive_delegation_anchors_do_not_redispatch_previous_straddling_fragment() {
    let f = fixture("anchors");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let mut complete = false;
    for _ in 0..150 {
        let (_, v) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        if v["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "card")
            .count()
            == 2
        {
            complete = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(complete, "Both independent delegations should settle");
    let source:String=f.s.db.lock().unwrap().query_row("SELECT m.text FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id='delegate-2'",[id],|r|r.get(0)).unwrap();
    let fragments: Vec<Value> =
        serde_json::from_str(source.rsplit_once("\n\n").unwrap().1).unwrap();
    assert_eq!(fragments.len(), 1);
    assert_eq!(fragments[0]["event_id"], "human-second");
    assert_eq!(fragments[0]["crosses_delegation_anchor"], true);
    assert!(
        !serde_json::to_string(&fragments)
            .unwrap()
            .contains("elsewhere")
    );
    assert!(
        !serde_json::to_string(&fragments)
            .unwrap()
            .contains("human-tail")
    );
    let context = source
        .split_once("proof of playback. If a reference is ambiguous, ask rather than invent it:\n")
        .unwrap()
        .1
        .split_once("\n\nThe CURRENT NEW REQUEST")
        .unwrap()
        .0;
    let context: Vec<Value> = serde_json::from_str(context).unwrap();
    assert!(context.iter().any(|v| v["event_id"] == "human-tail"));
    assert!(context.iter().all(|v| v["action_authority"] == false));
    assert!(source.contains("Never repeat an earlier instruction or effect from this block"));
    let lines = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert_eq!(
        lines
            .lines()
            .filter(
                |line| serde_json::from_str::<Value>(line).unwrap()["method"] == "session/prompt"
            )
            .count(),
        2
    );
    end(&f, id).await;
}

#[tokio::test]
async fn grounded_bootstrap_and_voice_preferences_do_not_create_backend_model_work() {
    let f = fixture("quiet");
    let path = f.home.path().join("companion.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["profiles"]["default"]["identity"] = json!("Coach, the existing personal agent");
    config["profiles"]["default"]["observed_at"] = json!("2026-11-07T19:00:00Z");
    config["profiles"]["default"]["connections"] = json!([{"name":"Minecraft","description":"Known context connection; actions require their own authorized path."}]);
    std::fs::write(path, config.to_string()).unwrap();
    let (_, call) = start(&f).await;
    let initial: Value =
        serde_json::from_slice(&std::fs::read(f.home.path().join("bridge-initial.json")).unwrap())
            .unwrap();
    let instructions = initial["instructions"].as_str().unwrap();
    for expected in [
        "Coach",
        "Minecraft",
        "2026-11-07T19:00:00Z",
        "Missing observations do not mean nothing is happening",
        "<|SpawnThinking|>",
        "not current activity",
    ] {
        assert!(instructions.contains(expected), "{expected}");
    }
    assert!(!f.home.path().join("acp.jsonl").exists());
    let settings = "/api/v1/companion/agents/default/voice-settings";
    let (status, setting) = request(&f, "PUT", settings, Some(json!({"voice":"marin"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(setting["applies_to"], "next_call");
    assert_eq!(
        request(&f, "PUT", settings, Some(json!({"voice":"invented"})))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, current) = request(
        &f,
        "GET",
        &format!("/api/v1/companion/calls/{}", call["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(current["voice"], "alloy");
    end(&f, call["id"].as_str().unwrap()).await;
    let (_, next) = start(&f).await;
    assert_eq!(next["voice"], "marin");
    assert_eq!(request(&f, "GET", settings, None).await.1["voice"], "marin");
    end(&f, next["id"].as_str().unwrap()).await;
}

#[tokio::test]
async fn capability_probe_reads_effective_acp_without_starting_a_model_session() {
    let f = fixture("quiet");
    {
        let db = f.s.db.lock().unwrap();
        let mut a: Value = serde_json::from_str(
            &db.query_row(
                "SELECT config FROM agent_endpoints WHERE id='default'",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        a["env"]["FIXTURE_IMAGES"] = json!("1");
        db.execute(
            "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
            [a.to_string()],
        )
        .unwrap();
    }
    let (_, catalog) = request(&f, "GET", "/api/v1/companion", None).await;
    assert!(catalog["contacts"][0]["image_input"].is_null());
    let (status, capabilities) = request(
        &f,
        "GET",
        "/api/v1/companion/agents/default/capabilities",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(capabilities["image_input"], true);
    let events = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert!(events.contains("initialize"));
    assert!(!events.contains("session/new"));
    assert!(!events.contains("session/prompt"));
}

#[tokio::test]
async fn upload_awareness_is_owned_durable_idempotent_and_does_not_auto_prompt() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let path = format!("/api/v1/companion/calls/{id}/uploads");
    let upload = uuid::Uuid::new_v4().to_string();
    let mut event =
        json!({"upload_id":upload,"state":"captured","source":"paste","name":"notes.txt"});
    assert_eq!(
        request(&f, "POST", &path, Some(event.clone())).await.0,
        StatusCode::OK
    );
    event["state"] = json!("uploading");
    assert_eq!(
        request(&f, "POST", &path, Some(event.clone())).await.0,
        StatusCode::OK
    );
    let (status, attachment) = request(
        &f,
        "POST",
        &format!("/api/v1/conversations/{room}/attachments"),
        Some(json!({"name":"notes.txt","mime_type":"text/plain","data":"aGVsbG8="})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    event["state"] = json!("available");
    event["attachment_ids"] = json!([attachment["id"]]);
    let (_, receipt) = request(&f, "POST", &path, Some(event.clone())).await;
    assert_eq!(receipt["voice_awareness"], "queued");
    let (_, again) = request(&f, "POST", &path, Some(event.clone())).await;
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["voice_awareness"], "queued");
    event["state"] = json!("failed");
    event["attachment_ids"] = json!([]);
    assert_eq!(
        request(&f, "POST", &path, Some(event)).await.0,
        StatusCode::CONFLICT
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!f.home.path().join("acp.jsonl").exists());
    {
        let db = f.s.db.lock().unwrap();
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM input_deliveries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let pending: bool = db
            .query_row(
                "SELECT message_id IS NULL FROM attachments WHERE id=?1",
                [attachment["id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(pending);
        let events: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='upload'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 3);
    }
    end(&f, id).await;
}

#[tokio::test]
async fn delayed_result_preserves_qualifiers_full_lookup_and_delegation_without_interrupting() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let text = format!(
        "{}The local test videos run on ARM64; these are local files, not YouTube videos.\n{}",
        "This is a complete introductory fact. ".repeat(20),
        "Further complete detail. ".repeat(3000)
    );
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('late-source',?1,'user','Find local test videos','sent',?2,?2,'owner')",rusqlite::params![room,now_ms()-190_000]).unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('late-result',?1,'assistant',?2,'complete',?3,?3,'Coach','default')",rusqlite::params![room,text,now_ms()]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES('late-delivery','late-source',?1,'default','direct','completed','late-result',?2)",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'late-delegation',100,'dispatched','late-source',?2)",rusqlite::params![id,now_ms()-190_000]).unwrap();
    }
    let v = until(&f, id, "card").await;
    let card = &v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "card")
        .unwrap()["data"];
    assert_eq!(card["late"], true);
    assert_eq!(card["request_id"], "late-source");
    assert_eq!(card["truncated"], true);
    let (status, full) = request(&f, "GET", card["result_url"].as_str().unwrap(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(full["text"], text);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE input_deliveries SET state='incorporated' WHERE id='late-delivery'",
            [],
        )
        .unwrap();
    let (status, full) = request(&f, "GET", card["result_url"].as_str().unwrap(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(full["text"], text);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET agent_id='different-agent' WHERE id='late-result'",
            [],
        )
        .unwrap();
    assert_eq!(
        request(&f, "GET", card["result_url"].as_str().unwrap(), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET agent_id='default' WHERE id='late-result'",
            [],
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(650)).await;
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    let appends: Vec<Value> = commands
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|v: &Value| v["op"] == "append")
        .collect();
    assert!(!appends.is_empty());
    assert!(appends.iter().all(|v| v["channel"] == "thinking"
        && v["delegation_id"] == "late-delegation"
        && v["content"].as_str().unwrap().len() <= 480));
    assert!(appends.iter().any(|v| {
        v["content"]
            .as_str()
            .unwrap()
            .contains("on ARM64; these are local files, not YouTube videos.")
    }));
    end(&f, id).await;
}

#[tokio::test]
async fn saved_upload_is_sent_once_when_voice_delegates_without_a_mandatory_question() {
    let f = fixture("upload_delegate");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let path = format!("/api/v1/companion/calls/{id}/uploads");
    let upload = uuid::Uuid::new_v4().to_string();
    let mut event =
        json!({"upload_id":upload,"state":"uploading","source":"drop","name":"notes.txt"});
    assert_eq!(
        request(&f, "POST", &path, Some(event.clone())).await.0,
        StatusCode::OK
    );
    let (_, attachment) = request(
        &f,
        "POST",
        &format!("/api/v1/conversations/{room}/attachments"),
        Some(json!({"name":"notes.txt","mime_type":"text/plain","data":"aGVsbG8="})),
    )
    .await;
    event["state"] = json!("available");
    event["attachment_ids"] = json!([attachment["id"]]);
    assert_eq!(
        request(&f, "POST", &path, Some(event.clone())).await.0,
        StatusCode::OK
    );
    let v = until(&f, id, "card").await;
    assert!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "delegation" && e["data"]["state"] == "completed")
    );
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let prompts: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|e: &Value| e["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0]["params"]["prompt"].to_string().contains("hello"));
    assert!(
        prompts[0]["params"]["prompt"]
            .to_string()
            .contains("do not automatically summarize")
    );
    assert_eq!(
        request(&f, "POST", &path, Some(event)).await.1["duplicate"],
        true
    );
    let target: Option<String> =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT message_id FROM attachments WHERE id=?1",
                [attachment["id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
    assert!(target.is_some());
    end(&f, id).await;
}

#[tokio::test]
async fn progress_polling_keeps_call_alive_when_elapsed_time_and_receipt_change() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('progress-source',?1,'user','Inspect the long task','sent',?2,?2,'owner')",rusqlite::params![room,now_ms()-20_000]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES('progress-delivery','progress-source',?1,'default','direct','submitted',?2)",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'progress-delegation',100,'dispatched','progress-source',?2)",rusqlite::params![id,now_ms()-20_000]).unwrap();
    }
    until(&f, id, "progress").await;
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE input_deliveries SET state='incorporated' WHERE id='progress-delivery'",
            [],
        )
        .unwrap();
    // Crossing several old minute buckets without new information must not
    // generate another status or a spoken "still waiting" interruption.
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_delegations SET created_at=?1 WHERE id='progress-delegation'",
            [now_ms() - 190_000],
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1700)).await;
    let (_, record) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
    assert_eq!(record["state"], "connected");
    assert_eq!(
        record["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "progress")
            .count(),
        1
    );
    let commands =
        std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap_or_default();
    assert!(
        !commands
            .lines()
            .any(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "append")
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}

#[tokio::test]
async fn newly_shared_png_reaches_image_capable_backend_after_many_historical_uploads() {
    let f = fixture("upload_delegate");
    {
        let mut agent = f.s.agent("default").unwrap().unwrap();
        agent.env.insert("FIXTURE_IMAGES".into(), "1".into());
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
                [serde_json::to_string(&agent).unwrap()],
            )
            .unwrap();
    }
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('historical-share',?1,'user','Previous shared document','complete',?2,?2,'owner')",rusqlite::params![room,now_ms()-60_000]).unwrap();
        db.execute("INSERT INTO attachments(id,conversation_id,name,mime_type,data,bytes,created_by,created_at,message_id) VALUES('historical-artifact',?1,'previous.txt','text/plain',X'6869',2,'owner',?2,'historical-share')",rusqlite::params![room,now_ms()-60_000]).unwrap();
        for _ in 0..21 {
            let upload = uuid::Uuid::new_v4().to_string();
            let data = json!({"upload_id":upload,"state":"available","source":"drop","name":"previous.txt","attachments":[{"id":"historical-artifact","name":"previous.txt","mime_type":"text/plain","bytes":2}],"conversation_id":room});
            db.execute("INSERT INTO companion_uploads(call_id,upload_id,state,data,updated_at) VALUES(?1,?2,'available',?3,?4)",rusqlite::params![id,upload,data.to_string(),now_ms()-60_000]).unwrap();
        }
    }
    let path = format!("/api/v1/companion/calls/{id}/uploads");
    let upload = uuid::Uuid::new_v4().to_string();
    let mut event =
        json!({"upload_id":upload,"state":"uploading","source":"camera","name":"scene.png"});
    assert_eq!(
        request(&f, "POST", &path, Some(event.clone())).await.0,
        StatusCode::OK
    );
    let image = "iVBORw0KGgo=";
    let (_, artifact) = request(
        &f,
        "POST",
        &format!("/api/v1/conversations/{room}/attachments"),
        Some(json!({"name":"scene.png","mime_type":"image/png","data":image})),
    )
    .await;
    event["state"] = json!("available");
    event["attachment_ids"] = json!([artifact["id"]]);
    assert_eq!(
        request(&f, "POST", &path, Some(event)).await.0,
        StatusCode::OK
    );
    until(&f, id, "card").await;
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let prompts: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|v: &Value| v["method"] == "session/prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    let blocks = prompts[0]["params"]["prompt"].as_array().unwrap();
    let supplied = blocks
        .iter()
        .find(|b| b["type"] == "image")
        .expect("saved PNG bytes must reach the advertised ACP image path");
    assert_eq!(supplied["mimeType"], "image/png");
    assert_eq!(supplied["data"], image);
    assert!(blocks.iter().any(|b| {
        b["text"]
            .as_str()
            .is_some_and(|text| text.contains("delegation_id is delegate-upload"))
    }));
    end(&f, id).await;
}

fn configure_call_permission_fixture(f: &Fixture, mode: &str, duplicate: bool) {
    std::fs::write(f.home.path().join("companion-actions.json"),json!({"enabled":true,"agents":{"default":[{"id":"minecraft","label":"Fixture","description":"A fixed fixture route","command":["/bin/true"]}]}}).to_string()).unwrap();
    let mut agent = f.s.agent("default").unwrap().unwrap();
    agent
        .env
        .insert("FIXTURE_COMPANION_PERMISSIONS".into(), mode.into());
    if duplicate {
        agent.mcp_servers.push(json!({"type":"http","name":"companion","url":"https://example.test/unrelated","headers":[]}));
    }
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
            [serde_json::to_string(&agent).unwrap()],
        )
        .unwrap();
}
#[tokio::test]
async fn owned_companion_native_permissions_allow_once_in_setup_and_active_turn_only() {
    let f = fixture("delegate");
    configure_call_permission_fixture(&f, "known", false);
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    until(&f, id, "card").await;
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let events: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let outcomes: Vec<_> = events
        .iter()
        .filter(|v| {
            v["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("companion-permission-"))
                && v.get("result").is_some()
        })
        .collect();
    assert_eq!(outcomes.len(), 5);
    assert!(
        outcomes
            .iter()
            .all(|v| v["result"]["outcome"]["optionId"] == "allow_once")
    );
    assert!(f.s.permissions.list(room, "owner").is_empty());
    let setup = events
        .iter()
        .find(|v| v["method"] == "session/new")
        .unwrap();
    let descriptors = setup["params"]["mcpServers"].as_array().unwrap();
    assert_eq!(
        descriptors
            .iter()
            .filter(|m| m["name"] == "companion")
            .count(),
        1
    );
    assert!(descriptors.iter().any(|m| {
        m["url"]
            .as_str()
            .is_some_and(|url| url.contains(&format!("/calls/{id}/mcp")))
    }));
    end(&f, id).await;
}
#[tokio::test]
async fn unrelated_mcp_with_companion_title_still_waits_for_human_permission() {
    let f = fixture("delegate");
    configure_call_permission_fixture(&f, "other", false);
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let mut pending = None;
    for _ in 0..100 {
        if let Some(p) = f.s.permissions.list(room, "owner").first() {
            pending = Some(p.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pending = pending.expect("unrelated MCP must remain a human decision");
    let path = format!(
        "/api/v1/conversations/{room}/permissions/{}",
        pending["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&f, "POST", &path, Some(json!({"option_id":"reject_once"})))
            .await
            .0,
        StatusCode::OK
    );
    until(&f, id, "card").await;
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let outcome: Value = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|v| v["id"] == "companion-permission-other" && v.get("result").is_some())
        .unwrap();
    assert_eq!(outcome["result"]["outcome"]["optionId"], "reject_once");
    end(&f, id).await;
}
#[tokio::test]
async fn static_companion_name_collision_is_rejected_before_session_setup() {
    let f = fixture("delegate");
    configure_call_permission_fixture(&f, "known", true);
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let mut interrupted = false;
    for _ in 0..100 {
        interrupted=f.s.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE role='assistant' AND status='interrupted')",[],|r|r.get(0)).unwrap();
        if interrupted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(interrupted);
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert!(!log.contains("session/new"));
    assert!(!log.contains("session/prompt"));
    end(&f, id).await;
}

#[tokio::test]
async fn capture_image_delivery_uses_actual_initialized_acp_capability_not_static_profile() {
    for image in [false, true] {
        let f = fixture("delegate");
        configure_call_permission_fixture(&f, "none", false);
        let mut agent = f.s.agent("default").unwrap().unwrap();
        if image {
            agent.env.insert("FIXTURE_IMAGES".into(), "1".into());
        }
        f.s.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
                [serde_json::to_string(&agent).unwrap()],
            )
            .unwrap();
        let path = f.home.path().join("companion.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["profiles"]["default"]["image_input"] = json!(!image);
        std::fs::write(path, config.to_string()).unwrap();
        let (_, call) = start(&f).await;
        let id = call["id"].as_str().unwrap();
        until(&f, id, "card").await;
        let (actual,at):(bool,i64)=f.s.db.lock().unwrap().query_row("SELECT image_input,observed_at FROM companion_surface_capabilities WHERE call_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(actual, image);
        assert!(at > 0);
        let events = std::fs::read_to_string(f.home.path().join("acp.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|v| v["method"] == "initialize")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|v| v["method"] == "session/prompt")
                .count(),
            1
        );
        end(&f, id).await;
    }
}

fn settlement_source(f: &Fixture, call: &Value, name: &str, state: &str, target: &str) {
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?1,'sent',?3,?3,'owner')",rusqlite::params![name,room,now_ms()]).unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES(?1,?1,?2,'default','direct',?3,?4,?5)",rusqlite::params![name,room,state,target,now_ms()]).unwrap();
    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,100,'dispatched',?2,?3)",rusqlite::params![id,name,now_ms()]).unwrap();
}
fn terminal_fixture(f: &Fixture, call: &Value, id: &str, status: &str) {
    f.s.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant','One settled answer',?3,?4,?4,'Coach','default')",rusqlite::params![id,call["conversation_id"].as_str().unwrap(),status,now_ms()]).unwrap();
}
async fn wait_delegation(f: &Fixture, call: &Value, delegation: &str, state: &str) {
    for _ in 0..100 {
        let observed: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM companion_delegations WHERE call_id=?1 AND id=?2",
                    rusqlite::params![call["id"].as_str().unwrap(), delegation],
                    |r| r.get(0),
                )
                .unwrap();
        if observed == state {
            return;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("Delegation {delegation} did not become {state}");
}
#[tokio::test]
async fn shared_target_settles_completed_and_incorporated_sources_once_without_closing_call() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    terminal_fixture(&f, &call, "shared-target", "complete");
    settlement_source(&f, &call, "first-completed", "completed", "shared-target");
    settlement_source(&f, &call, "second-completed", "completed", "shared-target");
    wait_delegation(&f, &call, "second-completed", "completed").await;
    // A native incorporated receipt can arrive after the canonical result was
    // published. It links to that result without changing or speaking it again.
    settlement_source(
        &f,
        &call,
        "later-incorporated",
        "incorporated",
        "shared-target",
    );
    wait_delegation(&f, &call, "later-incorporated", "completed").await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (_, record) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
    assert_eq!(record["state"], "connected");
    let events = record["events"].as_array().unwrap();
    assert_eq!(events.iter().filter(|e| e["kind"] == "card").count(), 1);
    let links = events
        .iter()
        .filter(|e| e["kind"] == "delegation" && e["data"]["state"] == "completed")
        .collect::<Vec<_>>();
    assert_eq!(links.len(), 3);
    assert!(
        links
            .iter()
            .all(|e| e["data"]["message_id"] == "shared-target"
                && e["data"]["target_status"] == "complete")
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM input_deliveries WHERE id='later-incorporated'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "incorporated"
    );
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    assert_eq!(
        commands
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "append")
            .count(),
        1
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}
#[tokio::test]
async fn incorporation_requires_exact_successful_target_and_unknown_does_not_become_success() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    terminal_fixture(&f, &call, "pending-target", "streaming");
    terminal_fixture(&f, &call, "failed-target", "interrupted");
    terminal_fixture(&f, &call, "other-agent-target", "complete");
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET agent_id='other-agent' WHERE id='other-agent-target'",
            [],
        )
        .unwrap();
    settlement_source(
        &f,
        &call,
        "incorporated-pending",
        "incorporated",
        "pending-target",
    );
    settlement_source(
        &f,
        &call,
        "incorporated-failed",
        "incorporated",
        "failed-target",
    );
    settlement_source(&f, &call, "unknown-source", "unknown", "pending-target");
    settlement_source(
        &f,
        &call,
        "wrong-target",
        "incorporated",
        "other-agent-target",
    );
    wait_delegation(&f, &call, "incorporated-failed", "unknown").await;
    wait_delegation(&f, &call, "unknown-source", "unknown").await;
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_delegations WHERE state='dispatched'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_events WHERE kind='card'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET status='complete' WHERE id='pending-target'",
            [],
        )
        .unwrap();
    wait_delegation(&f, &call, "incorporated-pending", "completed").await;
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM companion_delegations WHERE id='unknown-source'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "unknown"
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM companion_delegations WHERE id='wrong-target'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "dispatched"
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}
#[tokio::test]
async fn hangup_keeps_terminal_record_but_never_dispatches_or_narrates_pending_work() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    terminal_fixture(&f, &call, "after-close", "streaming");
    settlement_source(
        &f,
        &call,
        "accepted-before-close",
        "incorporated",
        "after-close",
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_delegations SET created_at=?1",
            [now_ms() - 20_000],
        )
        .unwrap();
    until(&f, id, "progress").await;
    end(&f, id).await;
    let last_seq: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT MAX(seq) FROM companion_events WHERE call_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_delegations SET created_at=?1",
            [now_ms() - 190_000],
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND seq>?2",
                rusqlite::params![id, last_seq],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET status='complete' WHERE id='after-close'",
            [],
        )
        .unwrap();
    wait_delegation(&f, &call, "accepted-before-close", "completed").await;
    let (_, record) = request(
        &f,
        "GET",
        &format!("/api/v1/companion/calls/{id}?after={last_seq}"),
        None,
    )
    .await;
    assert_eq!(record["state"], "closed");
    let events = record["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["kind"] == "card"));
    assert!(
        !events
            .iter()
            .any(|e| e["kind"] == "progress" || e["kind"] == "context_delivery")
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
}
#[tokio::test]
async fn call_permissions_are_visible_only_to_owner_and_noticed_once_without_arguments() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let params = json!({"toolCall":{"title":"Private operation","rawInput":{"secret":"never-say-this"}},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"},{"optionId":"deny","name":"Decline","kind":"reject_once"}]});
    let pending =
        f.s.permissions
            .request(room, "owner", "default", &params, Duration::from_secs(10))
            .unwrap();
    let other =
        f.s.permissions
            .request(room, "other", "default", &params, Duration::from_secs(10))
            .unwrap();
    let record = until(&f, id, "permission").await;
    assert_eq!(record["permissions"].as_array().unwrap().len(), 1);
    assert_eq!(record["permissions"][0]["id"], pending.id);
    assert_ne!(record["permissions"][0]["id"], other.id);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    let appends = commands
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|v| v["op"] == "append")
        .collect::<Vec<_>>();
    assert_eq!(appends.len(), 1);
    assert!(
        appends[0]["content"]
            .as_str()
            .unwrap()
            .contains("permission choice")
    );
    assert!(!commands.contains("never-say-this"));
    assert!(!commands.contains("Private operation"));
    let path = format!("/api/v1/conversations/{room}/permissions/{}", pending.id);
    assert_eq!(
        request(&f, "POST", &path, Some(json!({"option_id":"deny"})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(pending.wait().await["outcome"]["optionId"], "deny");
    let (_, record) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
    assert_eq!(record["permissions"], json!([]));
    assert_eq!(
        record["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "permission")
            .count(),
        1
    );
    end(&f, id).await;
}

#[tokio::test]
async fn legacy_result_card_is_preserved_when_later_source_settles_without_respeaking() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    terminal_fixture(&f, &call, "legacy-result", "complete");
    // Original preview cards had no message_id or attachments fields. Their
    // immutable event key, room and complete text still identify the result.
    let original = json!({"delegation_id":null,"conversation_id":room,"text":"One settled answer","truncated":false,"silent":false}).to_string();
    f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'result:legacy-result','card',?2,?3)",rusqlite::params![id,original,now_ms()]).unwrap();
    settlement_source(&f, &call, "native-later", "incorporated", "legacy-result");
    wait_delegation(&f, &call, "native-later", "completed").await;
    let (_, record) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
    assert_eq!(record["state"], "connected");
    assert_eq!(f.s.db.lock().unwrap().query_row("SELECT data FROM companion_events WHERE call_id=?1 AND event_key='result:legacy-result'",[id],|r|r.get::<_,String>(0)).unwrap(), original);
    assert!(
        !std::fs::read_to_string(f.home.path().join("bridge-commands"))
            .unwrap_or_default()
            .contains("append")
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}

#[tokio::test]
async fn current_incorporated_followup_keeps_shared_result_relevant_without_renewing_old_request() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    terminal_fixture(&f, &call, "combined-answer", "complete");
    settlement_source(&f, &call, "old-question", "completed", "combined-answer");
    settlement_source(
        &f,
        &call,
        "current-followup",
        "incorporated",
        "combined-answer",
    );
    let old_at = now_ms() - 190_000;
    let current_at = now_ms() - 1_000;
    {
        {
            let db = f.s.db.lock().unwrap();
            db.execute(
                "UPDATE messages SET created_at=?1 WHERE id='old-question'",
                [old_at],
            )
            .unwrap();
            db.execute(
                "UPDATE companion_delegations SET created_at=?1 WHERE id='old-question'",
                [old_at],
            )
            .unwrap();
            db.execute(
                "UPDATE messages SET created_at=?1 WHERE id='current-followup'",
                [current_at],
            )
            .unwrap();
            db.execute(
                "UPDATE companion_delegations SET created_at=?1 WHERE id='current-followup'",
                [current_at],
            )
            .unwrap();
        }
        wait_delegation(&f, &call, "current-followup", "completed").await;
        let (_, record) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        let events = record["events"].as_array().unwrap();
        let card = &events.iter().find(|e| e["kind"] == "card").unwrap()["data"];
        assert_eq!(card["late"], false);
        assert_eq!(card["request_id"], "current-followup");
        assert_eq!(card["delegation_id"], "current-followup");
        assert_eq!(events.iter().filter(|e| e["kind"] == "card").count(), 1);
        for source in ["old-question", "current-followup"] {
            assert!(events.iter().any(|e| e["kind"] == "delegation"
                && e["data"]["request_id"] == source
                && e["data"]["message_id"] == "combined-answer"));
        }
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT created_at FROM messages WHERE id='old-question'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            old_at
        );
        assert_eq!(
            db.query_row(
                "SELECT state FROM input_deliveries WHERE id='current-followup'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "incorporated"
        );
    }
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}

#[tokio::test]
async fn provider_loss_is_confirmed_finalization_but_interrupted_call() {
    for (reason, state) in [
        ("connection_lost", "interrupted"),
        ("expired", "closed"),
        ("remote_hangup", "closed"),
    ] {
        let f = fixture(reason);
        let (_, call) = start(&f).await;
        let id = call["id"].as_str().unwrap();
        let result = until(&f, id, "closed").await;
        assert_eq!(result["state"], state);
        assert_eq!(result["closure"]["confirmed"], true);
        assert_eq!(result["closure"]["provider_reason"], reason);
        if reason == "connection_lost" {
            assert!(
                result["closure"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("lost unexpectedly")
            );
        }
    }
}
#[tokio::test]
async fn browser_diagnostics_are_private_bounded_observations_not_authority() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let mut diag = json!({"trigger":"heartbeat","visibility":"visible","peer_state":"connected","ice_state":"completed","channel_state":"open","audio_state":"running","wake_lock":"held","control_age_ms":0,"heartbeat_age_ms":0,"timer_gap_ms":0});
    for i in 0..3 {
        diag["control_age_ms"] = json!(i);
        let (status, _) = request(
            &f,
            "POST",
            &format!("/api/v1/companion/calls/{id}/heartbeat"),
            Some(json!({"diagnostics":diag})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let count: i64 = f
        .s
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM companion_events WHERE call_id=?1 AND kind='browser_diagnostic'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    diag["visibility"] = json!("hidden");
    let (status, _) = request(
        &f,
        "POST",
        &format!("/api/v1/companion/calls/{id}/heartbeat"),
        Some(json!({"diagnostics":diag})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for mutation in [
        json!({"sdp":"private"}),
        json!({"control_age_ms":3600001}),
        json!({"trigger":"arbitrary secret"}),
    ] {
        let mut bad = diag.clone();
        for (k, v) in mutation.as_object().unwrap() {
            bad[k] = v.clone();
        }
        let (status, _) = request(
            &f,
            "POST",
            &format!("/api/v1/companion/calls/{id}/heartbeat"),
            Some(json!({"diagnostics":bad})),
        )
        .await;
        assert_ne!(status, StatusCode::OK);
    }
    let (status, _) = request(
        &f,
        "POST",
        &format!("/api/v1/companion/calls/{id}/close"),
        Some(json!({"reason":"page_exit","diagnostics":diag})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    until(&f, id, "closed").await;
    let browser_close:String=f.s.db.lock().unwrap().query_row("SELECT data FROM companion_events WHERE call_id=?1 AND event_key='browser-close-request'",[id],|r|r.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&browser_close).unwrap()["reason"],
        "page_exit"
    );
}

#[tokio::test]
async fn new_private_call_seeds_saved_context_without_repeating_backend_work() {
    let f = fixture("quiet");
    let (_, first) = start(&f).await;
    let first_id = first["id"].as_str().unwrap();
    f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'prior-human','transcript',?2,?3)",rusqlite::params![first_id,json!({"role":"user","text":"What about the oak counter?","start_ms":100,"end_ms":900}).to_string(),now_ms()]).unwrap();
    end(&f, first_id).await;
    let (_, second) = start(&f).await;
    let initial: Value =
        serde_json::from_slice(&std::fs::read(f.home.path().join("bridge-initial.json")).unwrap())
            .unwrap();
    let history: Value = serde_json::from_str(initial["history"].as_str().unwrap()).unwrap();
    assert_eq!(history["source_call_id"], first_id);
    assert_eq!(history["automatic_resume_authorized"], false);
    assert!(history.to_string().contains("oak counter"));
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM companion_delegations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    end(&f, second["id"].as_str().unwrap()).await;
}

#[tokio::test]
async fn confirmed_teardown_does_not_erase_browser_timeout_cause() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let (status, _) = request(
        &f,
        "POST",
        &format!("/api/v1/companion/calls/{id}/close"),
        Some(json!({"reason":"control_timeout"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = until(&f, id, "closed").await;
    assert_eq!(result["state"], "interrupted");
    assert_eq!(result["closure"]["confirmed"], true);
    assert_eq!(result["closure"]["local_cause"], "control_timeout");
    assert!(
        result["closure"]["reason"]
            .as_str()
            .unwrap()
            .contains("device was lost")
    );
}

#[tokio::test]
async fn persisted_provider_terminal_survives_bridge_eof_before_wrapper() {
    let f = fixture("terminal_eof");
    let (_, call) = start(&f).await;
    let result = until(&f, call["id"].as_str().unwrap(), "closed").await;
    assert_eq!(result["state"], "closed");
    assert_eq!(result["closure"]["confirmed"], true);
    assert_eq!(result["closure"]["provider_reason"], "expired");
}
