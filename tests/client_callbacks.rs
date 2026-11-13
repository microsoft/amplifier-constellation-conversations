//! Protocol fixture: real child process, real permission hub, no model or host credentials.
use agent_conversations::{
    config::{Agent, ClientPolicy, Config},
    driver,
    state::AppState,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
struct Fixture {
    _temp: tempfile::TempDir,
    state: Arc<AppState>,
    room: String,
    path: PathBuf,
}
fn fixture() -> Fixture {
    fixture_mode(false)
}
fn fixture_mode(lifecycle: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let home = base.join("service");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let script = base.join("agent.py");
    std::fs::write(&script,r#"import sys,json,os

def send(v): print(json.dumps({'jsonrpc':'2.0',**v}),flush=True)
def call(i,method,params):
 send({'id':i,'method':method,'params':params})
 value=json.loads(sys.stdin.readline()); assert value['id']==i; return value
caps={}
for line in sys.stdin:
 v=json.loads(line); m=v.get('method'); rid=v.get('id')
 if m=='initialize':
  caps=v['params']['clientCapabilities']; send({'id':rid,'result':{'protocolVersion':1,'agentCapabilities':{'loadSession':True}}})
 elif m in ('session/new','session/load'):
  sid='fixture-session'
  if os.environ.get('LIFECYCLE')=='1':
   p={'sessionId':sid,'path':os.environ['RESULT_FILE'],'content':'reviewed result'}
   result=call(90,'fs/write_text_file',p); assert result.get('result')=={},result
   result=call(91,'fs/read_text_file',{'sessionId':sid,'path':p['path']}); assert result.get('result',{}).get('content')=='reviewed result',result
   assert 'error' in call(92,'fs/read_text_file',{'sessionId':'foreign-session','path':p['path']})
  send({'id':rid,'result':{'sessionId':sid}})
 elif m=='session/prompt':
  sid=v['params']['sessionId']
  if os.environ.get('LIFECYCLE')=='1':
   text='setup callbacks completed'
  elif caps.get('fs',{}).get('writeTextFile'):
   p={'sessionId':sid,'path':os.environ['RESULT_FILE'],'content':'reviewed result'}
   result=call(100,'fs/write_text_file',p); assert result.get('result')=={},result
   result=call(101,'fs/read_text_file',{'sessionId':sid,'path':p['path']}); assert result.get('result',{}).get('content')=='reviewed result',result
   result=call(102,'fs/read_text_file',{'sessionId':'foreign-session','path':p['path']}); assert 'error' in result,result
   text='approved write and read completed'
  else:
   result=call(100,'fs/read_text_file',{'sessionId':sid,'path':os.environ['RESULT_FILE']}); assert 'error' in result
   text='callbacks disabled for this audience'
  send({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':text}}}})
  send({'id':rid,'result':{'stopReason':'end_turn'}})
 elif m=='session/cancel': break
"#).unwrap();
    let path = workspace.join("reviewed.txt");
    let mut env = BTreeMap::new();
    env.insert("RESULT_FILE".into(), path.to_string_lossy().into_owned());
    if lifecycle {
        env.insert("LIFECYCLE".into(), "1".into());
    }
    let config = Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Fixture".into(),
        owner: "alice".into(),
        allowed_users: vec!["bob".into()],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: String::new(),
        pam_service: "agent-conversations".into(),
        tls_cert: home.join("unused-cert"),
        tls_key: home.join("unused-key"),
        tls_ca: None,
        import_source: "fixture".into(),
        import_surface: "fixture".into(),
        auth_mode: Default::default(),
        transport: Default::default(),
        agent: Some(Agent {
            label: "ACP fixture".into(),
            command: "/usr/bin/python3".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: workspace.to_string_lossy().into_owned(),
            session_cwd: None,
            env,
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: ClientPolicy::default(),
        }),
    };
    config.save(&home).unwrap();
    let state = AppState::open(home).unwrap();
    let room = state
        .create_room("Files", "alice", &["default".into()])
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut agent = state.agent("default").unwrap().unwrap();
    agent.client = ClientPolicy {
        read_text_file: true,
        write_text_file: true,
        workspace_roots: vec![workspace.to_string_lossy().into_owned()],
        allowed_rooms: vec![room.clone()],
        allowed_users: vec!["alice".into()],
        allowed_agents: vec!["default".into()],
        ..Default::default()
    };
    agent.validate().unwrap();
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE agent_endpoints SET config=?1 WHERE id='default'",
            [serde_json::to_string(&agent).unwrap()],
        )
        .unwrap();
    Fixture {
        _temp: temp,
        state,
        room,
        path,
    }
}
fn input(
    f: &Fixture,
    message_id: &str,
    text: &str,
    participation: &str,
) -> agent_conversations::delivery::Input {
    let id = uuid::Uuid::new_v4().to_string();
    let db = f.state.db.lock().unwrap();
    db.execute("INSERT OR IGNORE INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'alice')",rusqlite::params![message_id,f.room,text,agent_conversations::state::now_ms()]).unwrap();
    let token = std::fs::read_to_string(f.state.home.join("secrets/control-token")).unwrap();
    db.execute(
        "INSERT INTO input_authority(message_id,token_hash) VALUES(?1,?2)",
        rusqlite::params![message_id, agent_conversations::auth::digest(&token)],
    )
    .unwrap();
    let seq = db
        .query_row("SELECT seq FROM messages WHERE id=?1", [message_id], |r| {
            r.get(0)
        })
        .unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,updated_at) VALUES(?1,?2,?3,'default',?4,'received',1)",rusqlite::params![id,message_id,f.room,participation]).unwrap();
    agent_conversations::delivery::Input {
        id,
        message_id: message_id.into(),
        seq,
        room: f.room.clone(),
        agent: "default".into(),
        actor: "alice".into(),
        text: text.into(),
        participation: participation.into(),
        attachments: vec![],
    }
}
fn start(f: &Fixture) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let (tx, _) = tokio::sync::watch::channel(false);
    f.state.running.lock().unwrap().insert(f.room.clone(), tx);
    tokio::spawn(driver::deliver(
        f.state.clone(),
        input(
            f,
            &uuid::Uuid::new_v4().to_string(),
            "Please write and read the reviewed result",
            "direct",
        ),
    ))
}
async fn permission(f: &Fixture) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let list = f.state.permissions.list(&f.room, "alice");
            if let Some(value) = list.first() {
                break value.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn ordinary_acp_callbacks_wait_for_actor_bound_review_and_publish_actual_results() {
    let f = fixture();
    let turn = start(&f);
    let request = permission(&f).await;
    assert_eq!(
        request["tool_call"]["rawInput"]["method"],
        "fs/write_text_file"
    );
    assert!(!f.path.exists());
    let id = request["id"].as_str().unwrap();
    assert!(f.state.permissions.list(&f.room, "bob").is_empty());
    assert!(
        f.state
            .permissions
            .resolve(&f.room, "bob", id, Some("client_allow_once"))
            .is_err()
    );
    f.state
        .permissions
        .resolve(&f.room, "alice", id, Some("client_allow_once"))
        .unwrap();
    assert!(
        f.state
            .permissions
            .resolve(&f.room, "alice", id, Some("client_allow_once"))
            .is_err()
    );
    let read = permission(&f).await;
    assert_eq!(read["tool_call"]["rawInput"]["method"], "fs/read_text_file");
    f.state
        .permissions
        .resolve(
            &f.room,
            "alice",
            read["id"].as_str().unwrap(),
            Some("client_allow_once"),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "reviewed result");
    let messages = f.state.messages(&f.room, 100).unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["text"] == "approved write and read completed" && m["status"] == "complete")
    );
    let audit: String = f
        .state
        .db
        .lock()
        .unwrap()
        .query_row("SELECT group_concat(data) FROM audit", [], |r| r.get(0))
        .unwrap();
    assert!(!audit.contains("reviewed result"));
}
#[tokio::test]
async fn cancel_while_reviewing_removes_pending_permission_and_executes_nothing() {
    let f = fixture();
    let turn = start(&f);
    let request = permission(&f).await;
    f.state
        .running
        .lock()
        .unwrap()
        .get(&f.room)
        .unwrap()
        .send(true)
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), turn)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!f.path.exists());
    assert!(f.state.permissions.list(&f.room, "alice").is_empty());
    assert!(
        f.state
            .permissions
            .resolve(
                &f.room,
                "alice",
                request["id"].as_str().unwrap(),
                Some("client_allow_once")
            )
            .is_err()
    );
}
#[tokio::test]
async fn adding_an_unlisted_human_removes_capabilities_from_the_actual_handshake() {
    let f = fixture();
    f.state.db.lock().unwrap().execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'bob','member',0)",[&f.room]).unwrap();
    let turn = start(&f);
    tokio::time::timeout(Duration::from_secs(8), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(f.state.permissions.list(&f.room, "alice").is_empty());
    assert!(!f.path.exists());
    assert!(
        f.state
            .messages(&f.room, 100)
            .unwrap()
            .iter()
            .any(|m| m["text"] == "callbacks disabled for this audience")
    );
}

#[tokio::test]
async fn setup_callbacks_are_available_during_new_and_load_with_the_same_review_boundary() {
    let f = fixture_mode(true);
    for _ in 0..2 {
        let turn = start(&f);
        for method in ["fs/write_text_file", "fs/read_text_file"] {
            let request = permission(&f).await;
            assert_eq!(request["tool_call"]["rawInput"]["method"], method);
            assert!(f.state.permissions.list(&f.room, "bob").is_empty());
            f.state
                .permissions
                .resolve(
                    &f.room,
                    "alice",
                    request["id"].as_str().unwrap(),
                    Some("client_allow_once"),
                )
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(8), turn)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    let count:i64=f.state.db.lock().unwrap().query_row("SELECT COUNT(*) FROM audit WHERE kind='client.operation' AND json_extract(data,'$.phase')='session_setup'",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 4);
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "reviewed result");
}
#[tokio::test]
async fn cancelling_during_session_creation_does_not_approve_its_callback() {
    let f = fixture_mode(true);
    let turn = start(&f);
    let request = permission(&f).await;
    f.state
        .running
        .lock()
        .unwrap()
        .get(&f.room)
        .unwrap()
        .send(true)
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), turn)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(!f.path.exists());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        f.state
            .permissions
            .resolve(
                &f.room,
                "alice",
                request["id"].as_str().unwrap(),
                Some("client_allow_once")
            )
            .is_err()
    );
}

#[tokio::test]
async fn ambient_room_uses_optional_host_offer_without_a_prompt_on_hold() {
    for action in ["hold", "engage"] {
        let f = fixture();
        let script = f._temp.path().join("agent.py");
        std::fs::write(&script,format!(r#"import sys,json,pathlib
root=pathlib.Path(__file__).parent
def emit(v):print(json.dumps({{'jsonrpc':'2.0',**v}}),flush=True)
for line in sys.stdin:
 r=json.loads(line);m=r.get('method');p=r.get('params',{{}});i=r.get('id')
 if m=='initialize':result={{'protocolVersion':1,'agentCapabilities':{{}},'_meta':{{'com.microsoft.amplifier.attention':{{'version':1,'enabled':True,'method':'_amplifier/attention'}}}}}}
 elif m=='session/new':result={{'sessionId':'session'}}
 elif m=='_amplifier/attention':
  assert p['context']['trigger_id']=='trigger';assert p['occurredAt']>0
  result={{'action':'{action}','id':'event','ticket':'ticket'}}
 elif m=='session/prompt':
  assert '{action}'=='engage';assert p['_meta']['com.microsoft.amplifier.attention']['ticket']=='ticket'
  (root/'prompted').write_text('yes')
  emit({{'method':'session/update','params':{{'sessionId':'session','update':{{'sessionUpdate':'agent_message_chunk','content':{{'type':'text','text':'Room response'}}}}}}}})
  result={{'stopReason':'end_turn'}}
 else:result={{}}
 if i is not None:emit({{'id':i,'result':result}})
"#)).unwrap();
        f.state.db.lock().unwrap().execute("INSERT INTO room_members(conversation_id,username,role,joined_at) VALUES(?1,'bob','member',0)",[&f.room]).unwrap();
        f.state.db.lock().unwrap().execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('trigger',?1,'user','What are you all working on?','complete',?2,?2,'alice')",rusqlite::params![f.room,agent_conversations::state::now_ms()]).unwrap();
        let (tx, _) = tokio::sync::watch::channel(false);
        f.state.running.lock().unwrap().insert(f.room.clone(), tx);
        driver::deliver(
            f.state.clone(),
            input(&f, "trigger", "What are you all working on?", "ambient"),
        )
        .await
        .unwrap();
        assert_eq!(f._temp.path().join("prompted").exists(), action == "engage");
        let messages = f.state.messages(&f.room, 100).unwrap();
        assert_eq!(
            messages.iter().filter(|m| m["role"] == "assistant").count(),
            if action == "engage" { 1 } else { 0 }
        );
    }
}
