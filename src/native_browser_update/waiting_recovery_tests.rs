//! The same pre-Stop recovery route used by worker(), with real private plans,
//! journals and admission rows. No manager, executable or listener is started.
use super::tests::Fixture;
use super::*;
use sha2::{Digest, Sha256};

struct WaitingFixture {
    fixture: Fixture,
    operation: Operation,
    update_id: String,
}
impl WaitingFixture {
    fn new(phase: &str, action: RecoveryAction, cancel: bool) -> Self {
        let fixture = Fixture::new();
        let plan = native_update::plan(&fixture.home, &fixture.candidate).unwrap();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let transaction = uuid::Uuid::new_v4().to_string();
        let update_id = uuid::Uuid::new_v4().to_string();
        let update_attempt = uuid::Uuid::new_v4().to_string();
        let context = json!({"id":transaction,"owner":"standalone","checked_plan":plan});
        let journal = json!({"schema":1,"installation_id":Config::load(&fixture.home).unwrap().id,
            "transaction_id":transaction,"context":context,
            "context_hash":format!("{:x}",Sha256::digest(serde_json::to_vec(&context).unwrap())),
            "phase":phase,"previous":plan["previous"],"candidate":null,
            "direction":"unchanged","selected":"previous","original_running":null,
            "stop_requested":false,"launch_nonce":null,"restore_started":false});
        write(&fixture.home.join("native-update.json"), &journal);
        let update = json!({"schema":1,"installation_id":Config::load(&fixture.home).unwrap().id,
            "current":{"id":update_id,"input":{"request_id":update_id,"package_path":fixture.candidate},
                "package":fixture.candidate,"version":"fixture-b","attempt_id":update_attempt,
                "owner":null,"operation_id":operation_id,"state":"waiting","phase":"waiting",
                "revision":7,"created_at":1,"updated_at":2,"cancel_requested":cancel,
                "controls":{},"prior_attempts":[]}});
        write(
            &fixture.home.join(DIRECTORY).join("update-flow.json"),
            &update,
        );
        let operation = Operation {
            id: operation_id.clone(),
            request_id: Some(operation_id),
            transaction_id: transaction,
            kind: OperationKind::Apply,
            previous_transaction: None,
            helper: plan["candidate"].clone(),
            plan,
            attempt: Attempt {
                id: uuid::Uuid::new_v4().to_string(),
                action: Some(action),
                claimed: true,
                handoff_nonce: "test-retained-recovery-nonce".into(),
                launch: None,
                worker_process: None,
            },
            state: OperationState::Running,
            outcome: None,
        };
        Self {
            fixture,
            operation,
            update_id,
        }
    }
    fn snapshot(&self) -> native_update::RecoverySnapshot {
        native_update::recovery_snapshot(&self.fixture.home)
            .unwrap()
            .unwrap()
    }
    fn selected(&self) -> WaitingRecovery {
        waiting_recovery(&self.fixture.home, &self.operation, &self.snapshot())
            .unwrap()
            .unwrap()
    }
    fn journal(&self) -> Value {
        serde_json::from_slice(&fs::read(self.fixture.home.join("native-update.json")).unwrap())
            .unwrap()
    }
    fn flow(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.fixture.home.join(DIRECTORY).join("update-flow.json")).unwrap(),
        )
        .unwrap()
    }
    fn busy(&self) {
        let db = rusqlite::Connection::open(self.fixture.home.join("state.sqlite3")).unwrap();
        db.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note) VALUES('retained-work','work',1,1,'working','')",[]).unwrap();
    }
}
fn write(path: &Path, value: &Value) {
    amplifier_native_platform::private_dir(path.parent().unwrap()).unwrap();
    amplifier_native_platform::private_write_data(path, &serde_json::to_vec(value).unwrap())
        .unwrap();
}

#[tokio::test]
async fn resumed_waiting_cancel_and_rollback_settle_same_transaction_without_stop_or_dispatch() {
    for phase in ["intent", "deferred"] {
        for (action, cancel) in [
            (RecoveryAction::Resume, true),
            (RecoveryAction::Rollback, false),
        ] {
            let f = WaitingFixture::new(phase, action, cancel);
            f.busy();
            let installed = fs::read(f.fixture.home.join("native-installation.json")).unwrap();
            let _worker = native_update::lock(&f.fixture.home, WORKER_LOCK).unwrap();
            let result = f
                .selected()
                .run(&f.fixture.home, &f.operation)
                .await
                .unwrap();
            assert_eq!(result["cancelled"], true);
            assert_eq!(result["outcome"], "unchanged");
            let journal = f.journal();
            assert_eq!(journal["transaction_id"], f.operation.transaction_id);
            assert_eq!(journal["phase"], "restored");
            assert_eq!(journal["stop_requested"], false);
            assert!(journal["original_running"].is_null());
            assert!(journal["candidate"].is_null());
            assert_eq!(
                crate::update_admission::status(&f.fixture.home).unwrap()["closed"],
                false
            );
            assert_eq!(
                fs::read(f.fixture.home.join("native-installation.json")).unwrap(),
                installed
            );
            assert!(
                !f.fixture.home.join(DIRECTORY).join(LEDGER).exists(),
                "Recovery must not allocate or dispatch another operation"
            );
            let db = rusqlite::Connection::open(f.fixture.home.join("state.sqlite3")).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT status FROM conversations WHERE id='retained-work'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "working"
            );
        }
    }
}

#[test]
fn resumed_before_stop_uses_saved_cancel_and_durable_installing_fence() {
    for cancelled in [false, true] {
        let f = WaitingFixture::new("intent", RecoveryAction::Resume, cancelled);
        let journal_before = fs::read(f.fixture.home.join("native-update.json")).unwrap();
        let _owner = native_update::lock(&f.fixture.home, "native-update.lock").unwrap();
        let decision = f
            .selected()
            .decision(
                &f.fixture.home,
                native_update::WaitingUpdateEvent::BeforeStop,
            )
            .unwrap();
        if cancelled {
            assert_eq!(decision, native_update::WaitingUpdateDecision::Cancel);
            assert_eq!(f.flow()["current"]["state"], "waiting");
        } else {
            assert_eq!(decision, native_update::WaitingUpdateDecision::Continue);
            assert_eq!(f.flow()["current"]["state"], "installing");
            assert!(
                flow::control(
                    &f.fixture.home,
                    &UpdateControlInput {
                        request_id: f.update_id.clone(),
                        control_id: uuid::Uuid::new_v4().to_string()
                    },
                    "cancel"
                )
                .is_err()
            );
        }
        assert_eq!(
            fs::read(f.fixture.home.join("native-update.json")).unwrap(),
            journal_before
        );
    }
}

#[tokio::test]
async fn phase_label_alone_does_not_reopen_a_crossed_stop_boundary() {
    let f = WaitingFixture::new("intent", RecoveryAction::Resume, true);
    let mut journal = f.journal();
    journal["stop_requested"] = json!(true);
    write(&f.fixture.home.join("native-update.json"), &journal);
    let before = fs::read(f.fixture.home.join("native-update.json")).unwrap();
    let flow_before = f.flow();
    assert!(
        f.selected()
            .run(&f.fixture.home, &f.operation)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(f.fixture.home.join("native-update.json")).unwrap(),
        before
    );
    assert_eq!(f.flow(), flow_before);
}

#[test]
fn only_exact_unified_pre_stop_apply_uses_waiting_recovery() {
    let f = WaitingFixture::new("intent", RecoveryAction::Resume, false);
    for phase in [
        "preparing",
        "prepared",
        "switching",
        "restoring",
        "restored",
    ] {
        let mut snapshot = f.snapshot();
        snapshot.phase = phase.into();
        assert!(
            waiting_recovery(&f.fixture.home, &f.operation, &snapshot)
                .unwrap()
                .is_none()
        );
    }
    let mut operation = f.operation.clone();
    operation.kind = OperationKind::Rollback;
    assert!(
        waiting_recovery(&f.fixture.home, &operation, &f.snapshot())
            .unwrap()
            .is_none()
    );
    operation = f.operation.clone();
    operation.id = uuid::Uuid::new_v4().to_string();
    assert!(
        waiting_recovery(&f.fixture.home, &operation, &f.snapshot())
            .unwrap()
            .is_none()
    );
    let mut snapshot = f.snapshot();
    snapshot.transaction = uuid::Uuid::new_v4().to_string();
    assert!(
        waiting_recovery(&f.fixture.home, &f.operation, &snapshot)
            .unwrap()
            .is_none()
    );
    snapshot = f.snapshot();
    snapshot.standalone = false;
    assert!(
        waiting_recovery(&f.fixture.home, &f.operation, &snapshot)
            .unwrap()
            .is_none()
    );
    operation = f.operation.clone();
    operation.attempt.action = None;
    assert!(waiting_recovery(&f.fixture.home, &operation, &f.snapshot()).is_err());
}
