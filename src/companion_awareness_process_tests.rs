//! Real native child/descendant cleanup without Python, shells or OS-specific
//! PID inspection. All connections are disposable loopback test witnesses.
use super::*;
use crate::config::{Agent, Config as AppConfig};

const FIXTURE: &str = "companion_awareness::process_tests::observer_process_fixture";
const MARKER: &str = "observer-process-fixture:";

fn fixture_args(mode: &str, port: u16) -> Vec<String> {
    vec![
        "--ignored".into(),
        "--exact".into(),
        FIXTURE.into(),
        "--nocapture".into(),
        // The harness ignores this nonmatching filter; the fixture reads it.
        "--skip".into(),
        format!("{MARKER}{mode}:{port}"),
    ]
}

#[test]
#[ignore = "helper subprocess for the bounded native observer test"]
fn observer_process_fixture() {
    use std::io::{Read, Write};
    let Some(marker) = std::env::args().find_map(|a| a.strip_prefix(MARKER).map(str::to_owned))
    else {
        return; // Explicitly running all ignored tests must not spawn anything.
    };
    let (mode, port) = marker.split_once(':').unwrap();
    let port: u16 = port.parse().unwrap();
    let mut witness = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    let descendant = if mode == "parent" {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        let input: Value = serde_json::from_str(&input).unwrap();
        assert_eq!(input, json!({"version":1,"cursor":null,"actions":[]}));
        witness.write_all(b"P").unwrap();
        Some(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(fixture_args("descendant", port))
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    } else {
        assert_eq!(mode, "descendant");
        witness.write_all(b"D").unwrap();
        None
    };
    // Keep the socket open until termination. A broken cleanup fails the parent
    // assertion before this independent safety stop; it cannot leave a forever helper.
    std::thread::sleep(Duration::from_secs(30));
    drop(descendant);
    drop(witness);
}

#[tokio::test]
async fn stalled_observer_and_descendant_are_stopped_without_facts() {
    let home = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut command = vec![executable.to_str().unwrap().to_owned()];
    command.extend(fixture_args("parent", port));
    AppConfig {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Native observer fixture".into(),
        owner: "owner".into(),
        allowed_users: vec![],
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
        auth_mode: Default::default(),
        transport: Default::default(),
        agent: Some(Agent {
            label: "Fixture".into(),
            command: executable.to_str().unwrap().into(),
            args: vec![],
            cwd: home.path().to_str().unwrap().into(),
            session_cwd: None,
            env: BTreeMap::new(),
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: Default::default(),
        }),
    }
    .save(home.path())
    .unwrap();
    std::fs::write(
        home.path().join("companion-awareness.json"),
        json!({
            "enabled":true,"decision_command":[executable],"agents":{"default":[{
                "id":"minecraft-fixture","label":"Fixture source","command":command,
                "owners":["owner"],"audiences":[{"kind":"public"}]
            }]}
        })
        .to_string(),
    )
    .unwrap();
    let state = AppState::open(home.path().into()).unwrap();
    let room = state.create("Native observer fixture").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call = uuid::Uuid::new_v4().to_string();
    let endpoint =
        auth::digest(&serde_json::to_string(&state.agent("default").unwrap().unwrap()).unwrap());
    state.db.lock().unwrap().execute(
        "INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)",
        params![call,uuid::Uuid::new_v4().to_string(),room,now_ms(),endpoint],
    ).unwrap();
    let query_state = state.clone();
    let query_call = call.clone();
    let started = tokio::time::Instant::now();
    let query = tokio::spawn(async move {
        connection_status(
            &query_state,
            &query_call,
            "minecraft-fixture",
            StatusView::Current,
        )
        .await
    });
    let mut witnesses = tokio::time::timeout(Duration::from_secs(8), async {
        let mut connections = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut role = [0u8; 1];
            socket.read_exact(&mut role).await.unwrap();
            connections.push((role[0], socket));
        }
        connections
    })
    .await
    .expect("Both native fixture processes must start before the deadline");
    witnesses.sort_by_key(|(role, _)| *role);
    assert_eq!(
        witnesses.iter().map(|(role, _)| *role).collect::<Vec<_>>(),
        b"DP"
    );
    let response = tokio::time::timeout(Duration::from_secs(20), query)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(11),
        "Exercise the real host deadline"
    );
    assert!(started.elapsed() < Duration::from_secs(25));
    assert_eq!(response["status"], "unavailable");
    assert_eq!(response["reports"], json!([]));
    assert_eq!(response["presence"], json!({}));
    assert_eq!(response["authority"], false);
    assert_eq!(response["read_only"], true);
    assert_eq!(response["connection_id"], "minecraft-fixture");
    for (role, mut socket) in witnesses {
        let mut bytes = Vec::new();
        let eof = tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
            .await
            .expect("Host timeout must stop helper and descendant");
        // Windows may report connection reset when its job is terminated.
        assert!(
            eof.is_ok()
                || eof.is_err_and(|e| matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                )),
            "role {role}"
        );
        assert!(bytes.is_empty());
    }
    let db = state.db.lock().unwrap();
    for table in [
        "companion_observations",
        "companion_observers",
        "companion_awareness",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "A stalled helper cannot produce facts or advance a source cursor"
        );
    }
}
