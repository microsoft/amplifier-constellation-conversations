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
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tower::ServiceExt;
struct Fixture {
    home: TempDir,
    s: Arc<AppState>,
    app: Router,
    token: String,
    browser_id: String,
    browser_refresh: Arc<AtomicBool>,
    browser_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in self.browser_tasks.get_mut().unwrap().drain(..) {
            task.abort();
        }
    }
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
receipt_port=34567
if voice=='retained_receipt':
 import os,threading,http.server,atexit
 (root/'receipt-pid').write_text(str(os.getpid()))
 atexit.register(lambda:(root/'receipt-exited').write_text('exited'))
 class Receipt(http.server.BaseHTTPRequestHandler):
  def do_POST(self):
   request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
   result={'jsonrpc':'2.0','id':request['id'],'result':{'content':[{'type':'text','text':'Matching saved finalization'}],'structuredContent':{'state':'closed','provider_finalization':'confirmed'}}}
   raw=json.dumps(result).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
  def log_message(self,*args):pass
 listener=http.server.ThreadingHTTPServer(('127.0.0.1',0),Receipt)
 receipt_port=listener.server_port
 threading.Thread(target=listener.serve_forever,daemon=True).start()
if voice=='closed_setup':
 emit({'type':'event','event':{'type':'session.closed','event_id':'early-closed','session':{'id':'live-fixture'},'reason':'close_requested','usage':{'seconds':15}}})
 emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});sys.exit(0)
if voice=='early':emit({'type':'event','event':{'type':'session.started','event_id':'started','session':{'id':'live-fixture'}}})
emit({'type':'ready','session_id':'live-fixture','sdp':'v=0\r\nfixture-answer',**({'mcp_port':receipt_port} if c.get('mcp') else {})})
if voice not in ('early','connecting'):emit({'type':'event','event':{'type':'session.started','event_id':'started','session':{'id':'live-fixture'}}})
if voice in ('connection_lost','expired','remote_hangup','terminal_eof'):
 import time
 time.sleep(.1)
 emit({'type':'event','event':{'type':'session.closed','event_id':'provider-terminal','session':{'id':'live-fixture'},'reason':'expired' if voice=='terminal_eof' else voice,'usage':{'seconds':120}}})
 if voice=='terminal_eof':sys.exit(0)
 emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});sys.exit(0)
if voice not in ('quiet','connecting','upload_delegate','retained_receipt'):
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
 if c['op']=='dispose':
  (root/'receipt-disposed').write_text('disposed');break
 if c['op']=='close':
  if voice=='retained_receipt':
   emit({'type':'event','event':{'type':'session.closed','event_id':'receipt-provider-terminal','session':{'id':'live-fixture'},'reason':'close_requested','usage':{'seconds':1}}})
   emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});continue
  emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});break
 if c['op']=='append':
  if not c['content'].strip() and c['channel']!='thinking':emit({'type':'error','code':'invalid_content','message':'Empty content'});continue
  if voice=='append_error' and c['content'] and c['channel']!='instructions':
   if second_sent:continue
   second_sent=True
   for suffix,client,outer in [('unrelated','never-queued',None),('conflict',c['event_id'],'another-command'),('matched',c['event_id'],None)]:
    event={'type':'error','event_id':'provider-'+suffix,'error':{'code':'invalid_value','type':'invalid_request_error','param':'delegation_id','client_event_id':client,'message':'The voice service reported an error.'}}
    if outer:event['client_event_id']=outer
    emit({'type':'event','event':event})
   emit({'type':'error','code':'voice_transport_failed','message':'Generic close'});continue
  emit({'type':'event','event':{'type':'session.'+c['channel']+'.appended','event_id':'ack-'+c['event_id'],'client_event_id':c['event_id'],'start_ms':100,'end_ms':110}})
  if voice=='control_on_hold' and 'full request is saved' in c['content']:
   emit({'type':'event','event':{'type':'session.input_transcript.delta','event_id':'human-close','delta':'Please end this call.','start_ms':700000,'end_ms':701000}})
   emit({'type':'event','event':{'type':'session.input_transcript.delta','event_id':'late-older-input','delta':'An older fragment.','start_ms':699000,'end_ms':699100}})
   emit({'type':'call_control','action':'end_call','decision':'requested','session_id':'live-fixture','source_event_id':'human-close','trigger_id':'trigger:close.1','decision_event_id':'decision:close.1','source_start_ms':700000,'source_end_ms':701000,'probability':.99})
   emit({'type':'closed','confirmed':True,'session_id':'live-fixture'});break
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
    s.db.lock().unwrap().execute("INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,'owner','fixture-surface-csrf',?2)",rusqlite::params![auth::digest("fixture-surface-cookie"),now_ms()+120_000]).unwrap();
    let app = server::router(s.clone());
    let token = std::fs::read_to_string(home.path().join("secrets/control-token")).unwrap();
    Fixture {
        home,
        s,
        app,
        token,
        browser_id: uuid::Uuid::new_v4().to_string(),
        browser_refresh: Arc::new(AtomicBool::new(true)),
        browser_tasks: Mutex::new(Vec::new()),
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
async fn post_browser_surface(
    app: &Router,
    cookie_name: &str,
    id: &str,
    body: Value,
) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/companion/calls/{id}/surface/presence"))
                .header(
                    header::COOKIE,
                    format!("{cookie_name}=fixture-surface-cookie"),
                )
                .header("x-csrf-token", "fixture-surface-csrf")
                .header(header::ORIGIN, "https://example.test:18445")
                .header(header::HOST, "example.test:18445")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}
fn available_browser_surface(browser: &str) -> Value {
    json!({"browser_id":browser,"camera_enabled":false,"capture_consent":false,
        "user_speaking":false,"agent_speaking":false,
        "playback":{"state":"ready","floor":"open","output_active":false,"interruption_id":null}})
}
async fn attach_browser(f: &Fixture, id: &str) {
    let body = available_browser_surface(&f.browser_id);
    let app = f.app.clone();
    let cookie = auth::cookie_name(&f.s).to_owned();
    let id = id.to_owned();
    let first = post_browser_surface(&app, &cookie, &id, body.clone()).await;
    if first != StatusCode::OK {
        let state: String =
            f.s.db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state FROM companion_calls WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
        assert!(
            ["closed", "interrupted", "closing"].contains(&state.as_str()),
            "{first}: {state}"
        );
        return;
    }
    // A real foreground browser refreshes presence independently of tool/model
    // completion. This fixture uses the authenticated public endpoint, not a
    // database shortcut that pretends the user has heard a generated answer.
    let enabled = f.browser_refresh.clone();
    let state = f.s.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let active: bool = state
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT state IN ('connecting','connected') FROM companion_calls WHERE id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !active {
                break;
            }
            if enabled.load(Ordering::SeqCst) {
                let _ = post_browser_surface(&app, &cookie, &id, body.clone()).await;
            }
        }
    });
    f.browser_tasks.lock().unwrap().push(task);
}
fn current_human_segments(prompt: &str) -> Vec<Value> {
    let block = prompt
        .split_once("Current human source (")
        .expect("explicit source block")
        .1;
    let source = block
        .split_once("):\n")
        .expect("source block header")
        .1
        .split_once("\n\nRecent conversation,")
        .expect("separate reference context")
        .0;
    serde_json::from_str(source).expect("exact current human source JSON")
}
async fn start(f: &Fixture) -> (Value, Value) {
    let input = json!({"request_id":uuid::Uuid::new_v4().to_string(),"agent_id":"default","sdp":"v=0\r\nfixture-offer"});
    let (status, value) = request(f, "POST", "/api/v1/companion/calls", Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(["connecting", "connected"].contains(&value["state"].as_str().unwrap()));
    attach_browser(f, value["id"].as_str().unwrap()).await;
    (input, value)
}
async fn until(f: &Fixture, id: &str, kind: &str) -> Value {
    let mut after = 0;
    let mut seen = Vec::new();
    for _ in 0..100 {
        let (_, mut v) = request(
            f,
            "GET",
            &format!("/api/v1/companion/calls/{id}?after={after}"),
            None,
        )
        .await;
        let page = v["events"].as_array().unwrap();
        if let Some(last) = page.last() {
            after = last["seq"].as_i64().unwrap();
        }
        seen.extend(page.iter().cloned());
        if seen.iter().any(|e| e["kind"] == kind) {
            v["events"] = json!(seen);
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
async fn local_call_duration_accepts_unset_null_and_positive_bounds_without_a_ceiling() {
    let f = fixture("quiet");
    let path = f.home.path().join("companion.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (bound, available) in [
        (None, true),
        (Some(Value::Null), true),
        (Some(json!(1)), true),
        (Some(json!(1800)), true),
        (Some(json!(7200)), true),
        (Some(json!(0)), false),
        (Some(json!(-1)), false),
        (Some(json!(1.5)), false),
    ] {
        let mut config = original.clone();
        config.as_object_mut().unwrap().remove("max_call_seconds");
        if let Some(bound) = bound {
            config["max_call_seconds"] = bound;
        }
        std::fs::write(&path, config.to_string()).unwrap();
        let (status, catalog) = request(&f, "GET", "/api/v1/companion", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(catalog["available"], available, "{config}");
    }
    assert!(!f.home.path().join("bridge-starts").exists());
    assert!(!f.home.path().join("acp.jsonl").exists());
}
#[tokio::test]
async fn explicit_local_call_duration_closes_through_existing_finalization_path() {
    let f = fixture("quiet");
    let path = f.home.path().join("companion.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    config["max_call_seconds"] = json!(1);
    std::fs::write(path, config.to_string()).unwrap();
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let result = until(&f, id, "closed").await;
    assert_eq!(result["state"], "closed");
    assert!(
        result["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["data"]["type"] == "host_close_requested"
                && e["data"]["source"] == "duration_limit")
    );
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    assert_eq!(
        commands
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "close")
            .count(),
        1
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
}
#[tokio::test]
async fn provider_append_rejection_is_correlated_without_replaying_settled_delegation() {
    let f = fixture("append_error");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let result = until(&f, id, "closed").await;
    let events = result["events"].as_array().unwrap();
    let receipts = events
        .iter()
        .filter(|e| e["kind"] == "context_delivery" && e["data"]["state"] == "rejected")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    let rejection = &receipts[0]["data"];
    let queued = events
        .iter()
        .find(|e| {
            e["kind"] == "context_delivery"
                && e["data"]["state"] == "queued"
                && e["data"]["event_id"] == rejection["event_id"]
        })
        .unwrap();
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
async fn deadline_wake_keeps_late_correction_and_duplicate_source_in_one_prompt() {
    let f = fixture("delegate");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    until(&f, id, "delegation").await;
    let created: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT created_at FROM companion_delegations WHERE call_id=?1 AND id='delegate-1'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
    assert!(!f.home.path().join("acp.jsonl").exists());
    // A correction delivered after the delegation still belongs to its source
    // when its provider timeline is within/straddles the delegation anchor.
    speech_fixture(
        &f,
        &call,
        "late-correction",
        "user",
        95,
        " Actually, report only; do not change anything.",
    );
    wait_delegation(&f, &call, "delegate-1", "completed").await;
    let (saved_created, source): (i64, String) = f.s.db.lock().unwrap().query_row(
        "SELECT created_at,source FROM companion_delegations WHERE call_id=?1 AND id='delegate-1'",
        [id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(saved_created, created);
    let source: Value = serde_json::from_str(&source).unwrap();
    assert_eq!(source["fragments"].as_array().unwrap().len(), 2);
    assert_eq!(source["fragments"][1]["event_id"], "late-correction");
    assert_eq!(source["fragments"][1]["start_ms"], 95);
    assert_eq!(source["fragments"][1]["end_ms"], 295);
    let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let prompts = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|v| v["method"] == "session/prompt")
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0]["params"]["prompt"]
            .to_string()
            .contains("Actually, report only")
    );
    let dispatched_at: i64 = f.s.db.lock().unwrap().query_row(
        "SELECT at FROM companion_events WHERE call_id=?1 AND kind='delegation' AND json_extract(data,'$.state')='dispatched'",
        [id], |r| r.get(0),
    ).unwrap();
    assert!(dispatched_at - created >= 2000, "grace was shortened");
    end(&f, id).await;
}

#[tokio::test]
async fn pending_deadline_obeys_close_access_heartbeat_and_duration_guards() {
    for guard in ["close", "access", "private_room", "heartbeat", "duration"] {
        let f = fixture("delegate");
        if guard == "duration" {
            let path = f.home.path().join("companion.json");
            let mut c: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            c["max_call_seconds"] = json!(1);
            std::fs::write(path, c.to_string()).unwrap();
        }
        let (_, call) = start(&f).await;
        let id = call["id"].as_str().unwrap();
        until(&f, id, "delegation").await;
        match guard {
            "close" => {
                request(
                    &f,
                    "POST",
                    &format!("/api/v1/companion/calls/{id}/close"),
                    Some(json!({})),
                )
                .await;
            }
            "access" => {
                f.s.db
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM tokens WHERE hash=?1", [auth::digest(&f.token)])
                    .unwrap();
            }
            "private_room" => {
                f.s.db.lock().unwrap().execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'other','member',?2)", rusqlite::params![call["conversation_id"].as_str().unwrap(),now_ms()]).unwrap();
            }
            "heartbeat" => {
                f.browser_refresh.store(false, Ordering::SeqCst);
                f.s.db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE companion_calls SET heartbeat_at=?2 WHERE id=?1",
                        rusqlite::params![id, now_ms() - 61_000],
                    )
                    .unwrap();
            }
            "duration" => {}
            _ => unreachable!(),
        }
        for _ in 0..100 {
            let state: String =
                f.s.db
                    .lock()
                    .unwrap()
                    .query_row("SELECT state FROM companion_calls WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .unwrap();
            if ["closed", "interrupted"].contains(&state.as_str()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        let db = f.s.db.lock().unwrap();
        let state: String = db
            .query_row("SELECT state FROM companion_calls WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            ["closed", "interrupted"].contains(&state.as_str()),
            "{guard}: {state}"
        );
        let accepted: i64 = db.query_row("SELECT COUNT(*) FROM companion_delegations WHERE call_id=?1 AND state IN ('dispatched','completed')", [id], |r|r.get(0)).unwrap();
        assert_eq!(accepted, 0, "{guard}");
        assert!(!f.home.path().join("acp.jsonl").exists(), "{guard}");
    }
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
    let commands = commands
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    let close = commands.iter().position(|v| v["op"] == "close").unwrap();
    assert!(commands[close + 1..].iter().all(|v| v["op"] != "append"));
    let appends = commands
        .iter()
        .filter(|v| v["op"] == "append")
        .collect::<Vec<_>>();
    assert_eq!(appends.len(), 3);
    assert_eq!(appends[0]["channel"], "thinking");
    assert!(appends[0]["content"].as_str().unwrap().is_empty());
    assert_eq!(appends[1]["channel"], "instructions");
    assert!(
        appends[1]["content"]
            .as_str()
            .unwrap()
            .contains("still awaits checked information")
    );
    assert_eq!(appends[2]["channel"], "instructions");
    assert!(
        appends[2]["content"]
            .as_str()
            .unwrap()
            .contains("remains unknown after its handshake")
    );
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
    let persisted: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT source FROM companion_delegations WHERE call_id=?1 AND id='delegate-1'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
    let persisted: Value = serde_json::from_str(&persisted).unwrap();
    let tail = persisted["fragments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["event_id"] == "human-tail")
        .unwrap();
    assert_eq!(tail["text"], " elsewhere");
    assert_eq!(tail["start_ms"], 13000);
    assert_eq!(tail["end_ms"], 13200);
    assert!(source.contains("only a clear new human request supplies action authority"));
    assert!(source.contains("Unknown effects must not be retried"));
    assert!(source.contains(
        "Distinguish the reported state and any cause stated by the source from your inference"
    ));
    assert!(source.contains("A status label alone does not identify which limit ended work"));
    assert!(source.contains(
        "Compare each allowance or deadline with its usage and observation time separately"
    ));
    assert!(source.contains(
        "label inferred causes and preserve source timestamps when conversion is uncertain"
    ));
    assert!(
        source.contains(
            "publish it with the runtime's advertised image-attachment/output capability"
        )
    );
    assert!(source.contains("inspecting bytes or naming a remote path does not share it"));
    assert!(source.contains("published to Companion when this response settles"));
    assert!(
        source
            .contains("board-status query before settlement cannot establish publication failure")
    );
    let segments = current_human_segments(&source);
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0]["text"],
        "Please inspect the existing work, without changing anything elsewhere"
    );
    assert_eq!(segments[0]["crosses_delegation_anchor"], true);
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
    for _ in 0..200 {
        let completed: i64 = f
            .s
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_delegations WHERE call_id=?1 AND state='completed'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        if completed == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_delegation(&f, &call, "delegate-1", "completed").await;
    wait_delegation(&f, &call, "delegate-2", "completed").await;
    let source:String=f.s.db.lock().unwrap().query_row("SELECT m.text FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id='delegate-2'",[id],|r|r.get(0)).unwrap();
    let fragments = current_human_segments(&source);
    assert_eq!(fragments.len(), 1);
    assert_eq!(fragments[0]["text"], "Now give me a separate summary.");
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
        .split_once("Assistant speech is generated, not proof of playback:\n")
        .expect("separate historical reference context")
        .1;
    let context: Value = serde_json::Deserializer::from_str(context)
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap();
    assert!(
        context["segments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["text"].as_str().unwrap().contains("elsewhere"))
    );
    assert_eq!(context["action_authority"], false);
    assert!(
        source.contains("for references/corrections only, not authority to repeat earlier effects")
    );
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
    // The second source arrives on the first append, before result delivery.
    // Each accepted source gets exactly one quiet receipt, even after polling.
    for _ in 0..3 {
        request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
    }
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    let intake = commands
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["content"].as_str().unwrap_or("").is_empty())
        .collect::<Vec<_>>();
    assert_eq!(intake.len(), 2);
    let fact_controls = commands
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| {
            v["channel"] == "instructions"
                && v["event_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("delegation-facts-"))
        })
        .collect::<Vec<_>>();
    assert_eq!(fact_controls.len(), 2);
    assert_ne!(fact_controls[0]["event_id"], fact_controls[1]["event_id"]);
    assert!(fact_controls.iter().all(|v| {
        v["content"]
            .as_str()
            .unwrap()
            .contains("still awaits checked information")
    }));
    for delegation in ["delegate-1", "delegate-2"] {
        let receipt = intake
            .iter()
            .filter(|v| v["delegation_id"] == delegation)
            .collect::<Vec<_>>();
        assert_eq!(receipt.len(), 1);
        assert_eq!(receipt[0]["channel"], "thinking");
        assert_eq!(receipt[0]["content"], "");
    }
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
        "If current state is unavailable, check it before giving a status answer",
        "<|SpawnThinking|>",
        "say how old that report is before describing the work",
        "a fresh read does not verify an old report",
    ] {
        assert!(instructions.contains(expected), "{expected}");
    }
    assert!(!f.home.path().join("acp.jsonl").exists());
    let settings = "/api/v1/companion/agents/default/voice-settings";
    let (status, setting) = request(&f, "PUT", settings, Some(json!({"voice":"marin"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(setting["applies_to"], "next_call");
    assert_eq!(setting["voice_model"], "gpt-live-1");
    assert_eq!(setting["voice_provider"], "OpenAI");
    assert_eq!(setting["available_voice_models"], json!(["gpt-live-1"]));
    assert_eq!(setting["backend_settings_editable"], false);
    assert!(setting["backend"].is_null());
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
async fn superseded_result_preserves_qualifiers_full_lookup_and_delegation_without_interrupting() {
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
        // The result is earlier context because a distinct request replaced it,
        // not simply because useful work took longer than thirty seconds.
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('new-question',?1,'user','Now check the other issue','sent',?2,?2,'owner')",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES('new-question','new-question',?1,'default','direct','queued',?2)",rusqlite::params![room,now_ms()]).unwrap();
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
    for (image, tools) in [(false, false), (true, false), (false, true), (true, true)] {
        let f = fixture("delegate");
        if tools {
            configure_call_permission_fixture(&f, "none", false);
        }
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
        let initial: Value = serde_json::from_slice(
            &std::fs::read(f.home.path().join("bridge-initial.json")).unwrap(),
        )
        .unwrap();
        assert!(
            initial["instructions"]
                .as_str()
                .unwrap()
                .contains("\"image_input\":null")
        );
        let controls = std::fs::read_to_string(f.home.path().join("bridge-commands"))
            .unwrap()
            .lines()
            .map(|v| serde_json::from_str::<Value>(v).unwrap())
            .filter(|v| {
                v["event_id"]
                    .as_str()
                    .is_some_and(|v| v.starts_with("image-capability-"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            controls.len(),
            1,
            "one actual capability control, independent of tool installation"
        );
        assert_eq!(controls[0]["channel"], "instructions");
        assert!(controls[0]["content"].as_str().unwrap().contains(if image {
            "advertised image input support"
        } else {
            "image input is unsupported"
        }));

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

fn speech_fixture(f: &Fixture, call: &Value, key: &str, role: &str, start: i64, text: &str) {
    f.s.db.lock().unwrap().execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,?2,'transcript',?3,?4)",rusqlite::params![call["id"].as_str().unwrap(),key,json!({"event_id":key,"role":role,"start_ms":start,"end_ms":start+200,"text":text}).to_string(),now_ms()]).unwrap();
}
fn pending_voice_fixture(f: &Fixture, call: &Value, delegation: &str, offset: i64) {
    f.s.db.lock().unwrap().execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,?3,'waiting_transcript',?4,?5)",rusqlite::params![call["id"].as_str().unwrap(),delegation,offset,uuid::Uuid::new_v4().to_string(),now_ms()-3000]).unwrap();
}

#[tokio::test]
async fn long_spoken_request_and_followup_preserve_exact_source_and_human_card_label() {
    let f = fixture("quiet");
    agent_fixture_env(&f, "FIXTURE_IMAGES", "1");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let mut exact = String::new();
    for i in 0..700 {
        let text = if i == 0 {
            "Inspect the shop carefully"
        } else if i == 350 {
            " without changing any structure"
        } else {
            " and report what you see"
        };
        exact.push_str(text);
        speech_fixture(&f, &call, &format!("long-{i}"), "user", 200 * i, text);
    }
    pending_voice_fixture(&f, &call, "long-request", 140_000);
    wait_delegation(&f, &call, "long-request", "completed").await;
    let (source,prompt):(String,String)=f.s.db.lock().unwrap().query_row("SELECT d.source,m.text FROM companion_delegations d JOIN messages m ON m.id=d.message_id WHERE d.call_id=?1 AND d.id='long-request'",[id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let saved: Value = serde_json::from_str(&source).unwrap();
    assert!(source.len() > 48 * 1024);
    assert_eq!(saved["fragments"].as_array().unwrap().len(), 700);
    assert_eq!(
        saved["fragments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["text"].as_str().unwrap())
            .collect::<String>(),
        exact
    );
    assert!(prompt.len() < 48 * 1024);
    let segments = current_human_segments(&prompt);
    assert_eq!(segments[0]["text"], exact);
    let record = until(&f, id, "card").await;
    let card = record["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "card")
        .unwrap();
    assert!(
        card["data"]["request_text"]
            .as_str()
            .unwrap()
            .starts_with("Inspect the shop")
    );
    assert!(
        !card["data"]["request_text"]
            .as_str()
            .unwrap()
            .contains("companion_request_action")
    );
    speech_fixture(
        &f,
        &call,
        "followup-ask",
        "assistant",
        141_000,
        "Shall I save that feedback?",
    );
    speech_fixture(
        &f,
        &call,
        "followup-human",
        "user",
        142_000,
        "Yes, but keep it private.",
    );
    pending_voice_fixture(&f, &call, "followup-request", 142_100);
    wait_delegation(&f, &call, "followup-request", "completed").await;
    let saved: String = f
        .s
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT source FROM companion_delegations WHERE call_id=?1 AND id='followup-request'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["fragments"].as_array().unwrap().len(), 2);
    assert_eq!(saved["fragments"][1]["text"], "Yes, but keep it private.");
    let room = call["conversation_id"].as_str().unwrap();
    let (_, photo) = request(&f, "POST", &format!("/api/v1/conversations/{room}/attachments"),
        Some(json!({"name":"camera.png","mime_type":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="}))).await;
    let upload = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        request(
            &f,
            "POST",
            &format!("/api/v1/companion/calls/{id}/uploads"),
            Some(
                json!({"upload_id":upload,"state":"captured","source":"camera","name":"camera.png"})
            )
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, _) = request(&f, "POST", &format!("/api/v1/companion/calls/{id}/uploads"),
        Some(json!({"upload_id":upload,"state":"available","source":"camera","name":"camera.png","attachment_ids":[photo["id"]]}))).await;
    assert_eq!(status, StatusCode::OK);
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE companion_uploads SET updated_at=?2 WHERE call_id=?1",
            rusqlite::params![id, now_ms() - 4000],
        )
        .unwrap();
    speech_fixture(
        &f,
        &call,
        "photo-human",
        "user",
        143_000,
        "Now inspect this photo. Do not send or publish it.",
    );
    pending_voice_fixture(&f, &call, "photo-request", 143_100);
    wait_delegation(&f, &call, "photo-request", "completed").await;
    let prompts = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    let logs = prompts
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    let prompts = logs
        .iter()
        .filter(|v| v["method"] == "session/prompt")
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 3);
    assert!(
        prompts[1]["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Shall I save that feedback?")
    );
    assert!(
        prompts[1]["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Yes, but keep it private.")
    );
    assert!(
        prompts[2]["params"]["prompt"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "image" && v["mimeType"] == "image/png")
    );
    assert!(
        prompts[2]["params"]["prompt"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Do not send or publish it.")
    );
    assert_eq!(
        logs.iter().filter(|v| v["method"] == "session/new").count(),
        1
    );
    assert_eq!(
        logs.iter()
            .filter(|v| v["method"] == "session/load")
            .count(),
        2
    );
    let owner: String = f.s.db.lock().unwrap().query_row("SELECT d.id FROM attachments a JOIN companion_delegations d ON d.message_id=a.message_id WHERE a.id=?1", [photo["id"].as_str().unwrap()], |r|r.get(0)).unwrap();
    assert_eq!(owner, "photo-request");
    end(&f, id).await;
}

#[tokio::test]
async fn late_human_source_repairs_an_empty_held_request_without_replaying_it() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    pending_voice_fixture(&f, &call, "late-source", 1000);
    wait_delegation(&f, &call, "late-source", "held").await;
    speech_fixture(
        &f,
        &call,
        "late-human",
        "user",
        900,
        "Inspect the saved photo without changing anything.",
    );
    wait_delegation(&f, &call, "late-source", "completed").await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let prompts = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert_eq!(
        prompts
            .lines()
            .filter(|l| serde_json::from_str::<Value>(l).unwrap()["method"] == "session/prompt")
            .count(),
        1
    );
    end(&f, id).await;
}

#[tokio::test]
async fn blocked_received_inputs_report_not_started_once_and_preserve_unknown_receipts() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    terminal_fixture(&f, &call, "interrupted-target", "interrupted");
    settlement_source(
        &f,
        &call,
        "uncertain-source",
        "unknown",
        "interrupted-target",
    );
    for name in ["pending-one", "pending-two"] {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user','Please check the latest status','complete',?3,?3,'owner')",rusqlite::params![name,room,now_ms()]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?1,?1,?2,'default','direct','received',?3)",rusqlite::params![name,room,now_ms()]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,?2,2000,'dispatched',?2,?3)",rusqlite::params![id,name,now_ms()]).unwrap();
    }
    let record = until(&f, id, "backend_status").await;
    assert!(
        record["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "progress"
                && e["data"]["state"] == "blocked"
                && e["data"]["delivery_state"] == "received")
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    assert_eq!(
        commands
            .lines()
            .filter(|l| l.contains("has not reached the agent"))
            .count(),
        1
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    {
        let db = f.s.db.lock().unwrap();
        assert_eq!(
            db.query_row(
                "SELECT state FROM input_deliveries WHERE id='uncertain-source'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "unknown"
        );
        assert_eq!(db.query_row("SELECT COUNT(*) FROM input_deliveries WHERE id IN ('pending-one','pending-two') AND state='received' AND target_turn IS NULL",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    }
    end(&f, id).await;
}

#[tokio::test]
async fn genuine_source_capacity_preserves_text_and_independent_hangup_does_not_release_work() {
    let f = fixture("control_on_hold");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let (tx, rx) = tokio::sync::watch::channel(false);
    f.s.running.lock().unwrap().insert(room.into(), tx);
    for i in 1..=6 {
        speech_fixture(
            &f,
            &call,
            &format!("large-{i}"),
            "user",
            i * 1000,
            &"word ".repeat(2000),
        );
    }
    pending_voice_fixture(&f, &call, "capacity-request", 6500);
    let record = until(&f, id, "closed").await;
    assert_eq!(record["state"], "closed");
    let source: String = f
        .s
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT source FROM companion_delegations WHERE call_id=?1 AND id='capacity-request'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(source.len() > 60_000);
    assert!(
        record["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "call_control" && e["data"]["decision"] == "requested")
    );
    assert!(record["events"].as_array().unwrap().iter().any(|e| {
        e["kind"] == "delegation"
            && e["data"]["state"] == "held"
            && e["data"]["message"]
                .as_str()
                .unwrap()
                .contains("full request is saved")
    }));
    assert!(!*rx.borrow());
    assert!(f.s.running.lock().unwrap().contains_key(room));
    assert!(!f.home.path().join("acp.jsonl").exists());
}

fn agent_fixture_env(f: &Fixture, name: &str, value: &str) {
    let db = f.s.db.lock().unwrap();
    let mut config: Value = serde_json::from_str(
        &db.query_row(
            "SELECT config FROM agent_endpoints WHERE id='default'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    config["env"][name] = json!(value);
    db.execute(
        "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
        [config.to_string()],
    )
    .unwrap();
}

#[tokio::test]
async fn advertised_backend_settings_are_observed_only_after_real_fixture_setup() {
    let f = fixture("quiet");
    agent_fixture_env(&f, "FIXTURE_MODEL_SETTINGS", "1");
    let settings = "/api/v1/companion/agents/default/voice-settings";
    assert!(request(&f, "GET", settings, None).await.1["backend"].is_null());
    let (_, call) = start(&f).await;
    assert!(request(&f, "GET", settings, None).await.1["backend"].is_null());
    speech_fixture(
        &f,
        &call,
        "settings-human",
        "user",
        10,
        "Check the fixture status.",
    );
    pending_voice_fixture(&f, &call, "settings-request", 100);
    wait_delegation(&f, &call, "settings-request", "completed").await;
    let (_, setting) = request(&f, "GET", settings, None).await;
    assert_eq!(setting["backend"]["model"]["id"], "fixture-local-model");
    assert_eq!(setting["backend"]["model"]["name"], "Fixture local model");
    assert_eq!(setting["backend"]["options"][0]["value"], "low");
    assert_eq!(setting["backend"]["options"][0]["editable"], false);
    assert_eq!(setting["backend"]["source"], "acp_session_setup");
    assert!(setting["backend"]["observed_at"].as_i64().unwrap() > 0);
    assert_eq!(setting["backend_settings_editable"], false);
    end(&f, call["id"].as_str().unwrap()).await;
}

#[tokio::test]
async fn interrupted_driver_preserves_bounded_actual_causes_and_never_replays_input() {
    for (terminal, kind) in [
        ("rpc_error", "acp_rejected"),
        ("refusal", "incomplete_turn"),
        ("disconnect", "transport_error"),
    ] {
        let f = fixture("quiet");
        agent_fixture_env(&f, "FIXTURE_TERMINAL", terminal);
        let (_, call) = start(&f).await;
        let id = call["id"].as_str().unwrap();
        let room = call["conversation_id"].as_str().unwrap();
        speech_fixture(
            &f,
            &call,
            "error-human",
            "user",
            10,
            "Please inspect the fixture status.",
        );
        pending_voice_fixture(&f, &call, "error-request", 100);
        wait_delegation(&f, &call, "error-request", "unknown").await;
        let raw: String = f.s.db.lock().unwrap().query_row("SELECT data FROM audit WHERE kind='turn.interrupted' AND conversation_id=?1 ORDER BY seq DESC LIMIT 1", [room], |r| r.get(0)).unwrap();
        let audit: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(audit["cause"]["kind"], kind, "{terminal}: {audit}");
        if terminal == "rpc_error" {
            assert_eq!(audit["cause"]["rpc_code"], -32088);
            assert_eq!(audit["cause"]["phase"], "session/prompt");
        } else if terminal == "refusal" {
            assert_eq!(audit["cause"]["stop_reason"], "refusal");
        }
        assert!(!raw.contains("private fixture secret"));
        assert!(!raw.contains("token"));
        let (delivery,status): (String,String) = f.s.db.lock().unwrap().query_row("SELECT d.state,m.status FROM input_deliveries d JOIN messages m ON m.id=d.target_turn WHERE d.conversation_id=?1", [room], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(delivery, "unknown");
        assert_eq!(status, "interrupted");
        for _ in 0..3 {
            request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        }
        let log = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|l| serde_json::from_str::<Value>(l).unwrap()["method"] == "session/prompt")
                .count(),
            1
        );
        end(&f, id).await;
    }
}

#[tokio::test]
async fn oversized_source_and_short_followup_remain_retained_without_false_same_call_recovery() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let exact = "Keep this whole instruction. ".repeat(2200);
    speech_fixture(&f, &call, "oversized-human", "user", 100, &exact);
    pending_voice_fixture(&f, &call, "oversized-request", 1000);
    wait_delegation(&f, &call, "oversized-request", "held").await;
    speech_fixture(
        &f,
        &call,
        "smaller-human",
        "user",
        1100,
        "Just do the first part.",
    );
    pending_voice_fixture(&f, &call, "smaller-request", 1200);
    wait_delegation(&f, &call, "smaller-request", "held").await;
    {
        let db = f.s.db.lock().unwrap();
        let original: String = db.query_row("SELECT source FROM companion_delegations WHERE call_id=?1 AND id='oversized-request'", [id], |r|r.get(0)).unwrap();
        let followup: String = db.query_row("SELECT source FROM companion_delegations WHERE call_id=?1 AND id='smaller-request'", [id], |r|r.get(0)).unwrap();
        let original: Value = serde_json::from_str(&original).unwrap();
        let followup: Value = serde_json::from_str(&followup).unwrap();
        assert_eq!(original["fragments"][0]["text"], exact);
        assert_eq!(followup["fragments"][0]["text"], exact);
        assert_eq!(followup["fragments"][1]["text"], "Just do the first part.");
        let notices = db.prepare("SELECT data FROM companion_events WHERE call_id=?1 AND kind='delegation' AND json_extract(data,'$.state')='held'").unwrap().query_map([id], |r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        assert_eq!(notices.len(), 2);
        for notice in notices {
            assert!(notice.contains("too large for this call to send"));
            assert!(notice.contains("start a new call with a smaller request"));
            assert!(notice.contains("retained and unexecuted"));
            assert!(!notice.contains("please narrow"));
        }
    }
    assert!(!f.home.path().join("acp.jsonl").exists());
    end(&f, id).await;
}

#[tokio::test]
async fn media_close_retains_original_receipt_transport_only_until_accepted_work_finishes() {
    let f = fixture("retained_receipt");
    std::fs::write(
        f.home.path().join("companion-actions.json"),
        json!({"enabled":true,"agents":{}}).to_string(),
    )
    .unwrap();
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let initial: Value =
        serde_json::from_slice(&std::fs::read(f.home.path().join("bridge-initial.json")).unwrap())
            .unwrap();
    assert_eq!(initial["retain_receipts"], true);
    let grant = initial["mcp"]["token"].as_str().unwrap().to_owned();
    let before_port: i64 =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT port FROM companion_tool_grants WHERE call_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
    {
        let db = f.s.db.lock().unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('receipt-source',?1,'user','End this call','complete',?2,?2,'owner')",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('receipt-turn',?1,'assistant','','streaming',?2,?2,'Coach','default')",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES('receipt-delivery','receipt-source',?1,'default','direct','submitted','receipt-turn',?2)",rusqlite::params![room,now_ms()]).unwrap();
        db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?1,'receipt-delegation',100,'dispatched','receipt-source',?2)",rusqlite::params![id,now_ms()]).unwrap();
    }
    end(&f, id).await;
    let pid = std::fs::read_to_string(f.home.path().join("receipt-pid")).unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap()
            .success()
    );
    assert!(!f.home.path().join("receipt-disposed").exists());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT port FROM companion_tool_grants WHERE call_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        before_port
    );
    let tool_request = |name: &str, args: Value| json!({"jsonrpc":"2.0","id":41,"method":"tools/call","params":{"name":name,"arguments":args}});
    for (name, args, wanted) in [
        ("companion_surface_status", json!({}), StatusCode::OK),
        (
            "companion_hangup",
            json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"receipt-delegation"}),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let result = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/companion/calls/{id}/mcp"))
                    .header("authorization", format!("Bearer {grant}"))
                    .header("content-type", "application/json")
                    .body(Body::from(tool_request(name, args).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), wanted);
        if wanted == StatusCode::OK {
            let body: Value =
                serde_json::from_slice(&to_bytes(result.into_body(), 65536).await.unwrap())
                    .unwrap();
            assert_eq!(
                body["result"]["structuredContent"]["provider_finalization"],
                "confirmed"
            );
        }
    }
    {
        let db = f.s.db.lock().unwrap();
        db.execute(
            "UPDATE messages SET status='complete',text='The call ended.' WHERE id='receipt-turn'",
            [],
        )
        .unwrap();
        db.execute(
            "UPDATE input_deliveries SET state='completed' WHERE id='receipt-delivery'",
            [],
        )
        .unwrap();
    }
    for _ in 0..100 {
        if f.home.path().join("receipt-exited").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(f.home.path().join("receipt-disposed").exists());
    assert!(f.home.path().join("receipt-exited").exists());
    assert!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT port FROM companion_tool_grants WHERE call_id=?1",
                [id],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap()
            .is_none()
    );
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(commands.iter().filter(|v| v["op"] == "close").count(), 1);
    assert_eq!(commands.iter().filter(|v| v["op"] == "dispose").count(), 1);
    assert_eq!(
        std::fs::read_to_string(f.home.path().join("bridge-starts")).unwrap(),
        "start\n"
    );
    assert!(
        !f.home.path().join("acp.jsonl").exists(),
        "Receipt lifecycle must not create a model turn"
    );
}

#[tokio::test]
async fn explicit_acp_image_result_is_saved_published_and_available_for_scoped_reinspection() {
    // Actual ACP fixture transport and result publication, synthetic PNG bytes;
    // this is not a Minecraft renderer or model-understanding test.
    let f = fixture("quiet");
    configure_call_permission_fixture(&f, "none", false);
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
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    let (_,accepted)=request(&f,"POST",&format!("/api/v1/conversations/{room}/messages"),Some(json!({"request_id":uuid::Uuid::new_v4().to_string(),"text":"media proof: explicitly share this fixture image","target_agent_ids":["default"],"attachment_ids":[]}))).await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    let saved = until(&f, id, "card").await;
    let card = saved["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "card")
        .unwrap();
    let artifact = card["data"]["attachments"][0]["id"]
        .as_str()
        .expect("explicit ACP image must be published with its settled answer");
    assert_eq!(card["data"]["attachments"][0]["mime_type"], "image/png");
    assert_eq!(card["data"]["presentation"], "current");
    let token: String =
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT token FROM companion_tool_grants WHERE call_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/companion/calls/{id}/tools/surface/attachment:{artifact}"
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let receipt: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(
        receipt["image"]["data"],
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="
    );
    assert_eq!(receipt["image_delivery"], "provided");
    assert_eq!(receipt["source"], "agent_result");
    assert_eq!(receipt["message_id"], card["data"]["message_id"]);
    assert_eq!(receipt["inspection"], false);
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_surface_commands WHERE call_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let acp = std::fs::read_to_string(f.home.path().join("acp.jsonl")).unwrap();
    assert_eq!(
        acp.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|e| e["method"] == "session/prompt")
            .count(),
        1,
        "image reinspection must not recapture or dispatch another prompt"
    );
    end(&f, id).await;
}

#[tokio::test]
async fn output_images_follow_current_request_identity_and_preserve_private_scope() {
    for case in [
        "delayed",
        "acknowledged",
        "corrected",
        "manual-request",
        "incorporated-followup",
        "closed",
        "old-call-source",
        "other-owner",
        "other-agent",
        "changed-endpoint",
        "shared-room",
        "unknown",
        "streaming",
        "text",
    ] {
        let f = fixture("quiet");
        let (_, call) = start(&f).await;
        let id = call["id"].as_str().unwrap();
        let room = call["conversation_id"].as_str().unwrap();
        // Keep the target nonterminal until all provenance/state variants are set.
        terminal_fixture(&f, &call, "image-target", "streaming");
        if case != "text" {
            agent_conversations::attachments::receive(
                &f.s,
                room,
                "image-target",
                "Coach",
                &json!({"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="}),
            ).unwrap();
        }
        settlement_source(&f, &call, "image-source", "incorporated", "image-target");
        if case == "corrected" || case == "incorporated-followup" {
            settlement_source(
                &f,
                &call,
                "newer-source",
                if case == "corrected" {
                    "submitted"
                } else {
                    "incorporated"
                },
                if case == "corrected" {
                    "different-target"
                } else {
                    "image-target"
                },
            );
        }
        {
            let db = f.s.db.lock().unwrap();
            match case {
                "delayed" => {
                    // Keep the exact source inside this call while simulating
                    // slow publication; age alone cannot revoke the request.
                    db.execute(
                        "UPDATE companion_calls SET created_at=?2 WHERE id=?1",
                        rusqlite::params![id, now_ms() - 40_000],
                    )
                    .unwrap();
                    db.execute(
                        "UPDATE messages SET created_at=?1 WHERE id='image-source'",
                        [now_ms() - 31_000],
                    )
                    .unwrap();
                }
                "acknowledged" => {
                    db.execute("INSERT INTO companion_events(call_id,event_key,kind,data,at) VALUES(?1,'later-human','transcript',?2,?3)", rusqlite::params![id,json!({"role":"user","text":"Okay, I'm still here."}).to_string(),now_ms()+1]).unwrap();
                }
                "manual-request" => {
                    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('manual-new',?1,'user','Switch to the other question','sent',?2,?2,'owner')",rusqlite::params![room,now_ms()]).unwrap();
                    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES('manual-new','manual-new',?1,'default','direct','queued',?2)",rusqlite::params![room,now_ms()]).unwrap();
                }
                "old-call-source" => {
                    db.execute("UPDATE messages SET created_at=(SELECT created_at-1 FROM companion_calls WHERE id=?1) WHERE id='image-source'", [id]).unwrap();
                }
                "other-owner" => {
                    db.execute(
                        "UPDATE messages SET actor='someone-else' WHERE id='image-source'",
                        [],
                    )
                    .unwrap();
                }
                "other-agent" => {
                    db.execute(
                        "UPDATE messages SET agent_id='someone-else' WHERE id='image-target'",
                        [],
                    )
                    .unwrap();
                }
                "changed-endpoint" => {
                    db.execute("UPDATE agent_endpoints SET config=json_set(config,'$.label','Changed endpoint') WHERE id='default'", []).unwrap();
                }
                "shared-room" => {
                    db.execute("UPDATE conversations SET shared=1 WHERE id=?1", [room])
                        .unwrap();
                }
                "unknown" => {
                    db.execute(
                        "UPDATE input_deliveries SET state='unknown' WHERE id='image-source'",
                        [],
                    )
                    .unwrap();
                }
                _ => {}
            }
        }
        if case == "closed" {
            end(&f, id).await;
        }
        if case != "streaming" {
            f.s.db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE messages SET status='complete' WHERE id='image-target'",
                    [],
                )
                .unwrap();
        }
        if ["unknown", "streaming", "other-agent", "changed-endpoint"].contains(&case) {
            tokio::time::sleep(Duration::from_millis(800)).await;
        } else {
            wait_delegation(&f, &call, "image-source", "completed").await;
        }
        let (_, record) = request(&f, "GET", &format!("/api/v1/companion/calls/{id}"), None).await;
        let cards = record["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "card")
            .collect::<Vec<_>>();
        if ["unknown", "streaming", "other-agent", "changed-endpoint"].contains(&case) {
            assert!(
                cards.is_empty(),
                "{case}: incomplete or unrelated output must not publish"
            );
        } else {
            assert_eq!(cards.len(), 1, "{case}");
            let current = ["delayed", "acknowledged", "incorporated-followup"].contains(&case);
            assert_eq!(
                cards[0]["data"]["presentation"],
                if current { "current" } else { "later" },
                "{case}"
            );
            if current {
                assert_eq!(cards[0]["data"]["late"], false, "{case}");
            }
            if case == "delayed" {
                assert!(cards[0]["data"]["elapsed_seconds"].as_i64().unwrap() >= 31);
            }
        }
        assert!(
            !f.home.path().join("acp.jsonl").exists(),
            "{case}: presentation must not dispatch work"
        );
        if case != "closed" {
            end(&f, id).await;
        }
    }
}

#[tokio::test]
async fn settled_image_publication_receipt_accompanies_unmodified_backend_answer() {
    let f = fixture("quiet");
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let room = call["conversation_id"].as_str().unwrap();
    terminal_fixture(&f, &call, "image-receipt-target", "streaming");
    let answer = "Attached the screenshot. I could not upload it to the board because no local upload tool exists.";
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET text=?1 WHERE id='image-receipt-target'",
            [answer],
        )
        .unwrap();
    agent_conversations::attachments::receive(&f.s,room,"image-receipt-target","Coach",&json!({"type":"image","mimeType":"image/png","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="})).unwrap();
    settlement_source(
        &f,
        &call,
        "image-receipt-source",
        "incorporated",
        "image-receipt-target",
    );
    f.s.db
        .lock()
        .unwrap()
        .execute(
            "UPDATE messages SET status='complete' WHERE id='image-receipt-target'",
            [],
        )
        .unwrap();
    let mut batch = None;
    for _ in 0..100 {
        batch = f.s.db.lock().unwrap().query_row("SELECT updates FROM companion_voice_batches WHERE call_id=?1 AND semantic_key='result:image-receipt-target'",[id],|r|r.get::<_,String>(0)).optional().unwrap();
        if batch.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let updates: Value =
        serde_json::from_str(&batch.expect("publication facts must travel with answer")).unwrap();
    let updates = updates.as_array().unwrap();
    assert!(
        updates.len() >= 4,
        "Evidence, complete answer, publication facts and speakable update stay together"
    );
    let answer_parts = &updates[1..updates.len() - 2];
    assert!(answer_parts.iter().all(|v| v["channel"] == "thinking"));
    let sentences: Vec<_> = answer_parts
        .iter()
        .flat_map(|v| v["content"].as_str().unwrap().lines())
        .collect();
    assert_eq!(
        sentences,
        vec![
            "Attached the screenshot.",
            "I could not upload it to the board because no local upload tool exists."
        ],
        "All source sentences remain complete and ordered before the authoritative publication receipt"
    );
    let receipt = &updates[updates.len() - 2];
    assert_eq!(receipt["channel"], "thinking");
    let content = receipt["content"].as_str().unwrap();
    assert!(
        content.contains("image-receipt-target"),
        "Quiet result identity still matches board membership"
    );
    assert!(content.contains("1 image(s) in shared space"));
    assert!(content.contains("offered to the current board"));
    assert!(content.contains("At publication time, board membership was not yet confirmed"));
    let spoken = updates.last().unwrap();
    assert_eq!(spoken["channel"], "commentary");
    assert_eq!(spoken["content"], "The image is saved in our shared space.");
    for command in updates {
        assert!(command["content"].as_str().unwrap().len() <= 480);
    }
    let spoken_content = spoken["content"].as_str().unwrap();
    assert!(!spoken_content.contains("image-receipt-target"));
    assert!(!spoken_content.contains("receipt"));
    assert!(!spoken_content.contains("Do not"));
    assert!(
        !updates[0]["content"]
            .as_str()
            .unwrap()
            .contains("instructions")
    );
    let card: String = f.s.db.lock().unwrap().query_row("SELECT data FROM companion_events WHERE call_id=?1 AND event_key='result:image-receipt-target'",[id],|r|r.get(0)).unwrap();
    let card: Value = serde_json::from_str(&card).unwrap();
    assert_eq!(
        card["text"], answer,
        "Canonical model answer is evidence, not rewritten"
    );
    assert_eq!(card["presentation"], "current");
    assert!(
        !f.home.path().join("acp.jsonl").exists(),
        "Publication must not recapture or reprompt"
    );
    end(&f, id).await;
}

#[tokio::test]
async fn tools_disabled_browser_floor_defers_saved_result_until_fresh_open_without_replaying_work()
{
    let f = fixture("quiet");
    f.browser_refresh.store(false, Ordering::SeqCst);
    assert!(!f.home.path().join("companion-actions.json").exists());
    let (_, call) = start(&f).await;
    let id = call["id"].as_str().unwrap();
    let cookie = auth::cookie_name(&f.s);
    let mut unknown = available_browser_surface(&f.browser_id);
    unknown["user_speaking"] = Value::Null;
    unknown["agent_speaking"] = Value::Null;
    unknown["playback"]["state"] = json!("unknown");
    unknown["playback"]["floor"] = json!("unknown");
    unknown["playback"]["output_active"] = Value::Null;
    assert_eq!(
        post_browser_surface(&f.app, &cookie, id, unknown).await,
        StatusCode::OK
    );
    terminal_fixture(&f, &call, "floor-target", "complete");
    settlement_source(&f, &call, "floor-source", "completed", "floor-target");
    let saved = until(&f, id, "card").await;
    assert!(
        saved["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "card" && e["data"]["message_id"] == "floor-target")
    );
    wait_delegation(&f, &call, "floor-source", "completed").await;
    let assert_unsent = || {
        assert!(
            !std::fs::read_to_string(f.home.path().join("bridge-commands"))
                .unwrap_or_default()
                .lines()
                .any(|line| serde_json::from_str::<Value>(line).unwrap()["op"] == "append")
        );
        assert_eq!(f.s.db.lock().unwrap().query_row("SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1 AND state='pending'",[id],|r|r.get::<_,i64>(0)).unwrap(),1);
    };
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_unsent();
    // Model an otherwise-open browser observation that has expired. No new
    // live presence is fabricated to make the pending result dispatch.
    let open = available_browser_surface(&f.browser_id);
    f.s.db.lock().unwrap().execute("UPDATE companion_surfaces SET user_speaking=0,agent_speaking=0,playback=?2,updated_at=?3 WHERE call_id=?1",rusqlite::params![id,open["playback"].to_string(),now_ms()-6000]).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_unsent();
    let mut speaking = open.clone();
    speaking["user_speaking"] = json!(true);
    speaking["playback"]["floor"] = json!("user");
    assert_eq!(
        post_browser_surface(&f.app, &cookie, id, speaking).await,
        StatusCode::OK
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_unsent();
    assert_eq!(
        post_browser_surface(&f.app, &cookie, id, open).await,
        StatusCode::OK
    );
    let mut accepted = false;
    for _ in 0..80 {
        accepted=f.s.db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM companion_events WHERE call_id=?1 AND kind='context_delivery' AND json_extract(data,'$.state')='accepted')",[id],|r|r.get(0)).unwrap();
        if accepted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        accepted,
        "fresh open browser should deliver the saved exact result"
    );
    let commands = std::fs::read_to_string(f.home.path().join("bridge-commands")).unwrap();
    let appends: Vec<Value> = commands
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|v: &Value| v["op"] == "append")
        .collect();
    assert_eq!(appends.len(), 1);
    assert_eq!(appends[0]["delegation_id"], "floor-source");
    assert!(
        appends[0]["content"]
            .as_str()
            .unwrap()
            .contains("One settled answer")
    );
    assert!(!f.home.path().join("acp.jsonl").exists());
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM input_deliveries WHERE id='floor-source'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "completed"
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_tool_grants WHERE call_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let capture = request(
        &f,
        "POST",
        &format!("/api/v1/companion/calls/{id}/tools/capture"),
        Some(
            json!({"operation_id":uuid::Uuid::new_v4().to_string(),"delegation_id":"floor-source"}),
        ),
    )
    .await;
    assert!(
        capture.0.is_client_error(),
        "telemetry must not enable disabled capture tools"
    );
    assert_eq!(
        f.s.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM companion_surface_commands WHERE call_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    end(&f, id).await;
}
