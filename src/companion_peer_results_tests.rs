//! Local database regressions: peer continuation text is not voice output.
//! No process, transport, model or external service is started.
use super::*;
use crate::config::{Agent, Config};

struct Fixture {
    _home: tempfile::TempDir,
    s: Arc<AppState>,
    call: String,
    room: String,
}

fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    Config {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Quiet peer fixture".into(),
        owner: "owner".into(),
        allowed_users: vec![],
        embed_origins: vec![],
        bind: "127.0.0.1".into(),
        port: 0,
        public_url: "https://fixture.test".into(),
        pam_service: "fixture".into(),
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
    let s = AppState::open(home.path().into()).unwrap();
    let room = s.create("Private call").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call = uuid::Uuid::new_v4().to_string();
    let endpoint =
        auth::digest(&serde_json::to_string(&s.agent("default").unwrap().unwrap()).unwrap());
    {
        let db = s.db.lock().unwrap();
        db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?1,'owner',?2,'fixture','default',?3,'connected',?4,?4,?5)",params![call,uuid::Uuid::new_v4().to_string(),room,now_ms(),endpoint]).unwrap();
        db.execute("INSERT INTO companion_surfaces(call_id,browser_id,session_hash,camera_enabled,capture_consent,user_speaking,agent_speaking,updated_at,playback) VALUES(?1,'browser','session',0,0,0,0,?2,?3)",params![call,now_ms(),json!({"state":"ready","floor":"open"}).to_string()]).unwrap();
    }
    Fixture {
        _home: home,
        s,
        call,
        room,
    }
}

fn answer(f: &Fixture, id: &str, text: &str) {
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,'Coach','default')",params![id,f.room,text,now_ms()]).unwrap();
}

fn delivery(f: &Fixture, input: &str, target: &str, peer: bool) {
    let db = f.s.db.lock().unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,?3,'A retained source','complete',?4,?4,?5)",params![input,f.room,if peer {"system"} else {"user"},now_ms(),if peer {"Peer"} else {"owner"}]).unwrap();
    db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,target_turn,updated_at) VALUES(?1,?1,?2,'default','direct','completed',?3,?4)",params![input,f.room,target,now_ms()]).unwrap();
    if peer {
        db.execute(
            "INSERT INTO huddle_peer_deliveries(input_id,message_id,binding) VALUES(?1,?2,?3)",
            params![
                input,
                input,
                json!({"call_id":f.call,"parent_id":f.room}).to_string()
            ],
        )
        .unwrap();
    }
}

fn events(f: &Fixture) -> Vec<Value> {
    let db = f.s.db.lock().unwrap();
    let mut q = db
        .prepare("SELECT data FROM companion_events WHERE call_id=?1 ORDER BY seq")
        .unwrap();
    q.query_map([&f.call], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}

#[test]
fn quiet_peer_completion_is_neither_card_nor_automatic_voice_and_is_not_rewritten() {
    let f = fixture();
    answer(&f, "peer-answer", "A routine private coordination detail.");
    delivery(&f, "peer-input", "peer-answer", true);
    for _ in 0..2 {
        observe_results(&f.s, &f.call, &f.room, true).unwrap();
        // Defense in depth also applies to direct publication callers.
        publish_result(
            &f.s,
            &f.call,
            &f.room,
            "peer-answer",
            "A routine private coordination detail.",
        )
        .unwrap();
    }
    assert!(events(&f).is_empty());
    let db = f.s.db.lock().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT text FROM messages WHERE id='peer-answer'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "A routine private coordination detail."
    );
    assert_eq!(
        db.query_row(
            "SELECT state FROM input_deliveries WHERE message_id='peer-input'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "completed"
    );
}

#[test]
fn quiet_turns_do_not_consume_the_ordinary_result_batch_or_hide_a_human_answer() {
    let f = fixture();
    for index in 0..20 {
        let target = format!("quiet-answer-{index}");
        answer(&f, &target, "Routine coordination");
        delivery(&f, &format!("quiet-input-{index}"), &target, true);
    }
    answer(&f, "ordinary-answer", "The answer you requested.");
    delivery(&f, "human-input", "ordinary-answer", false);
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let records = events(&f);
    assert_eq!(
        records
            .iter()
            .filter(|row| row["message_id"] == "ordinary-answer"
                && row["text"] == "The answer you requested.")
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("Routine coordination")
    );
    let db = f.s.db.lock().unwrap();
    let batches: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(batches, 1);
}

#[test]
fn a_real_human_input_in_the_same_target_retains_ordinary_result_presentation() {
    let f = fixture();
    answer(&f, "combined-answer", "Your clarification is answered.");
    delivery(&f, "peer-input", "combined-answer", true);
    delivery(&f, "human-followup", "combined-answer", false);
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    assert!(
        events(&f)
            .iter()
            .any(|row| row["message_id"] == "combined-answer"
                && row["request_id"] == "human-followup")
    );
}

#[test]
fn an_explicit_huddle_outcome_still_returns_after_a_quiet_peer_turn() {
    let f = fixture();
    answer(&f, "quiet-answer", "Working through a routine peer detail.");
    delivery(&f, "quiet-input", "quiet-answer", true);
    let huddle = f.s.create("Review").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = uuid::Uuid::new_v4().to_string();
    let post = uuid::Uuid::new_v4().to_string();
    let source_text = "Review this together and tell me the outcome.";
    let text = "The requested review is complete; one question remains.";
    let at = now_ms();
    let origin = json!({"kind":"conversation","conversation_id":f.room,"request_id":source,"message_id":source,"source_revision":auth::digest(source_text),"occurred_at":at,"actor":{"id":"owner","kind":"human"}});
    let payload = json!({"request_id":post,"text":text,"target_agent_ids":[],"kind":"outcome","source":null,"outcome":{"kind":"result","evidence":[]}});
    let receipt = json!({"version":1,"status":"saved","message_id":post,"request_id":post,"room_id":huddle,"kind":"outcome","agent_id":"default","origin":origin,"revision":auth::digest(&payload.to_string()),"text_sha256":auth::digest(text),"inspect_url":format!("https://fixture.test/?conversation={huddle}")});
    {
        let mut db = f.s.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,?2,'user',?3,'complete',?4,?4,'owner')",params![source,f.room,source_text,at]).unwrap();
        tx.execute("UPDATE conversations SET shared=1 WHERE id=?1", [&huddle])
            .unwrap();
        tx.execute("INSERT INTO huddles(room_id,request_id,purpose,origin,fingerprint) VALUES(?1,?2,'Review',?3,'fixture')",params![huddle,uuid::Uuid::new_v4().to_string(),origin.to_string()]).unwrap();
        tx.execute("INSERT INTO huddle_participants(room_id,agent_id,label,connection_id,token_hash) VALUES(?1,'default','Coach','fixture','private-test-hash')",[&huddle]).unwrap();
        tx.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES(?1,?2,'assistant',?3,'complete',?4,?4,'Coach','default')",params![post,huddle,text,at]).unwrap();
        let seq = tx.last_insert_rowid();
        tx.execute("INSERT INTO huddle_posts(message_id,room_id,agent_id,payload,receipt) VALUES(?1,?2,'default',?3,?4)",params![post,huddle,payload.to_string(),receipt.to_string()]).unwrap();
        let row = json!({"huddle_id":huddle,"inspect_url":receipt["inspect_url"],"origin":origin,"message":{"id":post,"seq":seq,"agent_id":"default","actor":"Coach","text":text,"created_at":at},"outcome":payload["outcome"],"revision":receipt["revision"]});
        assert_eq!(
            crate::companion_huddles::record_return(&f.s, &tx, &row).unwrap()["state"],
            "stored"
        );
        tx.commit().unwrap();
    }
    observe_results(&f.s, &f.call, &f.room, true).unwrap();
    let records = events(&f);
    assert!(
        records
            .iter()
            .any(|row| row["text"] == text && row["huddle_id"] == huddle)
    );
    assert!(
        !serde_json::to_string(&records)
            .unwrap()
            .contains("Working through a routine peer detail")
    );
    let db = f.s.db.lock().unwrap();
    let raw: String = db
        .query_row(
            "SELECT updates FROM companion_voice_batches WHERE call_id=?1",
            [&f.call],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw.contains(text));
    assert!(!raw.contains("Working through a routine peer detail"));
}
