//! Real private SQLite/package fixtures and a disposable owner process. These
//! tests never invoke an OS service manager or execute an update worker.
use super::super::tests::Fixture;
use super::*;
use std::process::{Command, Stdio};

fn request(f: &Fixture) -> Input {
    Input {
        request_id: uuid::Uuid::new_v4().to_string(),
        offer_id: None,
        package_path: Some(f.root.path().join("never-opened-cancelled-package")),
    }
}

fn cancel(id: &str) -> ControlInput {
    ControlInput {
        request_id: id.into(),
        control_id: uuid::Uuid::new_v4().to_string(),
    }
}

fn bytes(home: &Path) -> Vec<u8> {
    fs::read(home.join(DIRECTORY).join(FILE)).unwrap()
}

#[test]
#[ignore = "disposable subprocess fixture; invoked by departed_owner_cancellation_settles_without_verification_or_dispatch"]
fn owner_process_waits_for_eof() {
    assert_eq!(
        std::env::var("COMPANION_CANCEL_OWNER_FIXTURE").as_deref(),
        Ok("1")
    );
    let mut input = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut input).unwrap();
}

fn departed_owner() -> Value {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_browser_update::flow::cancelled_owner_tests::owner_process_waits_for_eof",
            "--ignored",
        ])
        .env("COMPANION_CANCEL_OWNER_FIXTURE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let identity = amplifier_native_platform::process::identity(child.id());
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());
    let identity = identity.unwrap().unwrap();
    assert!(!amplifier_native_platform::process::matches(&identity).unwrap());
    identity
}

#[test]
fn departed_owner_cancellation_settles_without_verification_or_dispatch() {
    let f = Fixture::new();
    let input = request(&f);
    let attempt = accept(&f.home, input.clone()).unwrap().unwrap();
    let departed = departed_owner();
    change(&f.home, &input.request_id, &attempt, |u| {
        u.owner = departed;
        touch(u, State::Preparing, "preparing");
        Ok(())
    })
    .unwrap();
    let installed = fs::read(f.home.join("native-installation.json")).unwrap();
    let control_id = cancel(&input.request_id);
    let (result, scans) =
        crate::native_bundle::count_verifications(|| control(&f.home, &control_id, "cancel"));
    assert!(result.unwrap().is_none());
    assert_eq!(scans, 0);
    let saved = load_store(&f.home).unwrap().current.unwrap();
    assert_eq!(saved.state, State::Cancelled);
    assert!(saved.cancel_requested);
    assert_eq!(saved.attempt_id, attempt);
    assert!(saved.operation_id.is_none());
    assert_eq!(saved.controls[&control_id.control_id].outcome, "accepted");
    let stable = bytes(&f.home);
    preparation_failed(&f.home, &input.request_id, &attempt).unwrap();
    assert_eq!(
        bytes(&f.home),
        stable,
        "late failure must preserve cancellation"
    );
    assert!(control(&f.home, &control_id, "cancel").unwrap().is_none());
    assert_eq!(bytes(&f.home), stable);
    assert!(load(&f.home).unwrap().operations.is_empty());
    assert!(!f.home.join("native-update.json").exists());
    assert!(!input.package_path.unwrap().exists());
    assert_eq!(
        fs::read(f.home.join("native-installation.json")).unwrap(),
        installed
    );
}

#[test]
fn live_preparation_keeps_cancellation_as_intent() {
    for held in [false, true] {
        let f = Fixture::new();
        let input = request(&f);
        let attempt = accept(&f.home, input.clone()).unwrap().unwrap();
        change(&f.home, &input.request_id, &attempt, |u| {
            touch(u, State::Preparing, "preparing");
            Ok(())
        })
        .unwrap();
        let _preparation = held.then(|| native_update::lock(&f.home, PREPARATION_LOCK).unwrap());
        control(&f.home, &cancel(&input.request_id), "cancel").unwrap();
        let saved = load_store(&f.home).unwrap().current.unwrap();
        assert_eq!(saved.state, State::Preparing);
        assert!(saved.cancel_requested);
        assert!(saved.operation_id.is_none());
        assert!(load(&f.home).unwrap().operations.is_empty());
    }
}

#[test]
fn departed_generation_with_held_execution_or_uncertain_identity_stays_pending() {
    for kind in ["preparation", "worker", "native", "identity"] {
        let f = Fixture::new();
        let input = request(&f);
        let attempt = accept(&f.home, input.clone()).unwrap().unwrap();
        let departed = departed_owner();
        change(&f.home, &input.request_id, &attempt, |u| {
            u.owner = if kind == "identity" {
                json!({"pid":std::process::id()})
            } else {
                departed
            };
            touch(u, State::Preparing, "preparing");
            Ok(())
        })
        .unwrap();
        let _held = match kind {
            "preparation" => Some(native_update::lock(&f.home, PREPARATION_LOCK).unwrap()),
            "worker" => Some(native_update::lock(&f.home, WORKER_LOCK).unwrap()),
            "native" => Some(native_update::lock(&f.home, "native-update.lock").unwrap()),
            _ => None,
        };
        let repeated_control = cancel(&input.request_id);
        control(&f.home, &repeated_control, "cancel").unwrap();
        let saved = load_store(&f.home).unwrap().current.unwrap();
        assert_eq!(saved.state, State::Preparing, "{kind}");
        assert!(saved.cancel_requested);
        assert!(saved.operation_id.is_none());
        let control_before = serde_json::to_value(&saved.controls).unwrap();
        drop(_held);
        // Reconcile a lost reply with the SAME control. Released ownership can
        // now prove cancellation; unknown identity still cannot. No new job.
        assert!(
            control(&f.home, &repeated_control, "cancel")
                .unwrap()
                .is_none()
        );
        let after = load_store(&f.home).unwrap().current.unwrap();
        assert_eq!(
            after.state,
            if kind == "identity" {
                State::Preparing
            } else {
                State::Cancelled
            },
            "{kind}"
        );
        assert_eq!(
            serde_json::to_value(&after.controls).unwrap(),
            control_before
        );
        assert_eq!(after.controls.len(), 1);
        assert!(load(&f.home).unwrap().operations.is_empty());
    }
}

#[test]
fn released_orphan_cancellation_is_observed_without_post_and_archived_before_new_request() {
    let f = Fixture::new();
    let input = request(&f);
    let attempt = accept(&f.home, input.clone()).unwrap().unwrap();
    let departed = departed_owner();
    change(&f.home, &input.request_id, &attempt, |u| {
        u.owner = departed;
        touch(u, State::Preparing, "preparing");
        Ok(())
    })
    .unwrap();
    let held = native_update::lock(&f.home, PREPARATION_LOCK).unwrap();
    let control_id = cancel(&input.request_id);
    control(&f.home, &control_id, "cancel").unwrap();
    assert_eq!(
        load_store(&f.home).unwrap().current.unwrap().state,
        State::Preparing
    );
    drop(held);
    let before = bytes(&f.home);
    let ((), scans) = crate::native_bundle::count_verifications(|| {
        let mut result = json!({"actions":{},"current":{},"owner":"standalone","mode":"normal"});
        augment(
            &f.home,
            &mut result,
            Some(&input.request_id),
            None,
            Some(&control_id.control_id),
        )
        .unwrap();
        assert_eq!(result["update"]["state"], "cancelled");
        assert_eq!(result["control"]["status"], "recorded");
        assert_eq!(result["update"]["can_continue"], false);
        assert_eq!(bytes(&f.home), before, "GET must not persist or dispatch");
        let next = request(&f);
        assert!(accept(&f.home, next.clone()).unwrap().is_some());
        let store = load_store(&f.home).unwrap();
        assert_eq!(store.current.as_ref().unwrap().id, next.request_id);
        let prior = get(&f.home, &store, &input.request_id).unwrap().unwrap();
        assert_eq!(prior.state, State::Cancelled);
        assert_eq!(prior.attempt_id, attempt);
        assert_eq!(prior.controls.len(), 1);
        assert_eq!(prior.controls[&control_id.control_id].outcome, "accepted");
    });
    assert_eq!(scans, 0);
    assert!(load(&f.home).unwrap().operations.is_empty());
    assert!(!f.home.join("native-update.json").exists());
}

fn unchanged_child(f: &Fixture) -> (Input, String) {
    let input = request(f);
    let attempt = accept(&f.home, input.clone()).unwrap().unwrap();
    let child = uuid::Uuid::new_v4().to_string();
    let transaction = uuid::Uuid::new_v4().to_string();
    let check_id = uuid::Uuid::new_v4().to_string();
    let config = Config::load(&f.home).unwrap();
    let mut ledger = load(&f.home).unwrap();
    ledger.current_operation = Some(child.clone());
    ledger.requests.insert(
        child.clone(),
        RequestRecord {
            payload: RequestPayload::Apply { check_id },
            operation_id: child.clone(),
            attempt_id: child.clone(),
        },
    );
    ledger.operations.insert(
        child.clone(),
        Operation {
            id: child.clone(),
            request_id: Some(child.clone()),
            transaction_id: transaction.clone(),
            kind: OperationKind::Apply,
            previous_transaction: None,
            plan: json!({"installation_id":config.id}),
            helper: Value::Null,
            attempt: Attempt {
                id: child.clone(),
                action: None,
                claimed: true,
                handoff_nonce: "test-only-retained-nonce".into(),
                launch: None,
                worker_process: None,
            },
            state: OperationState::Deferred,
            outcome: Some("unchanged".into()),
        },
    );
    save(&f.home, &ledger).unwrap();
    amplifier_native_platform::private_write_data(
        &f.home.join("native-update.json"),
        &serde_json::to_vec(&json!({"schema":1,"installation_id":config.id,
            "transaction_id":transaction,"phase":"restored","direction":"unchanged",
            "restore_outcome":"unchanged","context":{"owner":"standalone"}}))
        .unwrap(),
    )
    .unwrap();
    change(&f.home, &input.request_id, &attempt, |u| {
        u.operation_id = Some(child.clone());
        touch(u, State::NeedsAttention, "needs_attention");
        Ok(())
    })
    .unwrap();
    (input, child)
}

#[test]
fn settled_unchanged_attention_cancels_without_rewriting_native_evidence() {
    let f = Fixture::new();
    let (input, _) = unchanged_child(&f);
    let ledger = fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap();
    let journal = fs::read(f.home.join("native-update.json")).unwrap();
    let (result, scans) = crate::native_bundle::count_verifications(|| {
        control(&f.home, &cancel(&input.request_id), "cancel")
    });
    assert!(result.unwrap().is_none());
    assert_eq!(scans, 0);
    assert_eq!(
        load_store(&f.home).unwrap().current.unwrap().state,
        State::Cancelled
    );
    assert_eq!(
        fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap(),
        ledger
    );
    assert_eq!(
        fs::read(f.home.join("native-update.json")).unwrap(),
        journal
    );
}

#[test]
fn missing_changed_or_unknown_child_never_becomes_cancelled() {
    for kind in [
        "missing",
        "unknown",
        "wrong_current",
        "wrong_request",
        "wrong_installation",
        "wrong_transaction",
        "pending_journal",
        "missing_journal",
        "worker",
        "preparation",
    ] {
        let f = Fixture::new();
        let (input, child) = unchanged_child(&f);
        let mut ledger = load(&f.home).unwrap();
        match kind {
            "missing" => {
                ledger.operations.remove(&child);
                ledger.requests.remove(&child);
            }
            "unknown" => {
                let op = ledger.operations.get_mut(&child).unwrap();
                op.state = OperationState::Unknown;
                op.outcome = None;
            }
            "wrong_current" => ledger.current_operation = None,
            "wrong_request" => {
                ledger.requests.get_mut(&child).unwrap().operation_id =
                    uuid::Uuid::new_v4().to_string()
            }
            "wrong_installation" => {
                ledger.operations.get_mut(&child).unwrap().plan["installation_id"] = json!("other")
            }
            "wrong_transaction" => {
                ledger.operations.get_mut(&child).unwrap().transaction_id =
                    uuid::Uuid::new_v4().to_string()
            }
            "missing_journal" => fs::remove_file(f.home.join("native-update.json")).unwrap(),
            "pending_journal" => {
                let path = f.home.join("native-update.json");
                let mut j: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                j["phase"] = json!("deferred");
                amplifier_native_platform::private_write_data(
                    &path,
                    &serde_json::to_vec(&j).unwrap(),
                )
                .unwrap();
            }
            _ => {}
        }
        // Corruption fixtures retain missing/conflicting evidence rather than
        // passing through normal ledger compaction, which correctly refuses it.
        amplifier_native_platform::private_write_data(
            &f.home.join(DIRECTORY).join(LEDGER),
            &serde_json::to_vec(&ledger).unwrap(),
        )
        .unwrap();
        let _held = match kind {
            "worker" => Some(native_update::lock(&f.home, WORKER_LOCK).unwrap()),
            "preparation" => Some(native_update::lock(&f.home, PREPARATION_LOCK).unwrap()),
            _ => None,
        };
        let native_before = fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap();
        control(&f.home, &cancel(&input.request_id), "cancel").unwrap();
        let saved = load_store(&f.home).unwrap().current.unwrap();
        assert_eq!(saved.state, State::NeedsAttention, "{kind}");
        assert!(saved.cancel_requested);
        assert_eq!(
            fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            native_before,
            "{kind}"
        );
    }
}
