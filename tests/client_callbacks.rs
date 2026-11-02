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
        agent: Agent {
            label: "ACP fixture".into(),
            command: "/usr/bin/python3".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: workspace.to_string_lossy().into_owned(),
            session_cwd: None,
            env,
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: ClientPolicy::default(),
        },
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
fn start(f: &Fixture) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let (tx, _) = tokio::sync::watch::channel(false);
    f.state.running.lock().unwrap().insert(f.room.clone(), tx);
    tokio::spawn(driver::round(
        f.state.clone(),
        f.room.clone(),
        "alice".into(),
        "trigger".into(),
        "Please write and read the reviewed result".into(),
        vec!["default".into()],
        vec![],
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
