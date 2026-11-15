//! Exercise retained flow boundaries with the existing complete native bundle
//! fixture. LostReply is a launch seam, not a real OS manager or executable run.
use super::super::tests::{DIAGNOSTIC_FIXTURE_SERIAL, Fixture, LostReply};
use super::*;
use std::sync::atomic::Ordering;

fn input(package_path: PathBuf) -> Input {
    Input {
        request_id: uuid::Uuid::new_v4().to_string(),
        offer_id: None,
        package_path: Some(package_path),
    }
}

fn control_input(request_id: &str) -> ControlInput {
    ControlInput {
        request_id: request_id.to_owned(),
        control_id: uuid::Uuid::new_v4().to_string(),
    }
}

fn flow_bytes(home: &Path) -> Vec<u8> {
    fs::read(home.join(DIRECTORY).join(FILE)).unwrap()
}

fn projection(home: &Path, request_id: &str, control_id: Option<&str>) -> Value {
    let mut value = status(home, "normal", Some(request_id), &LostReply::default()).unwrap();
    augment(home, &mut value, Some(request_id), None, control_id).unwrap();
    value
}

fn no_native_child(home: &Path, launch: &LostReply) {
    let ledger = load(home).unwrap();
    assert!(ledger.current_operation.is_none());
    assert!(ledger.operations.is_empty());
    assert!(ledger.requests.is_empty());
    assert!(!home.join("native-update.json").exists());
    assert_eq!(launch.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn acceptance_is_durable_without_opening_the_candidate_or_verifying_its_contents() {
    let fixture = Fixture::new();
    let missing = fixture.root.path().join("not-created-package");
    let request = input(missing.clone());
    let installed = fs::read(fixture.home.join("native-installation.json")).unwrap();
    let (attempt, scans) = crate::native_bundle::count_verifications(|| {
        accept(&fixture.home, request.clone()).unwrap().unwrap()
    });
    assert_eq!(
        scans, 0,
        "acknowledgement must not start content verification"
    );
    assert!(!missing.exists());
    let retained = load_store(&fixture.home).unwrap().current.unwrap();
    assert_eq!(retained.input, request);
    assert_eq!(retained.attempt_id, attempt);
    assert_eq!(
        projection(&fixture.home, &request.request_id, None)["request"]["state"],
        "recorded"
    );
    no_native_child(&fixture.home, &LostReply::default());
    assert_eq!(
        fs::read(fixture.home.join("native-installation.json")).unwrap(),
        installed
    );
}

#[test]
fn repeated_exact_request_reuses_its_saved_attempt_and_conflicting_or_competing_requests_refuse() {
    let fixture = Fixture::new();
    let request = input(fixture.candidate.clone());
    let attempt = accept(&fixture.home, request.clone()).unwrap().unwrap();
    let saved = flow_bytes(&fixture.home);
    assert!(accept(&fixture.home, request.clone()).unwrap().is_none());
    assert_eq!(flow_bytes(&fixture.home), saved);
    let conflicting = Input {
        package_path: Some(fixture.root.path().join("other-package")),
        ..request.clone()
    };
    assert_eq!(
        accept(&fixture.home, conflicting).unwrap_err().0,
        StatusCode::CONFLICT
    );
    assert_eq!(flow_bytes(&fixture.home), saved);
    assert_eq!(
        accept(&fixture.home, input(fixture.candidate.clone()))
            .unwrap_err()
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(flow_bytes(&fixture.home), saved);
    assert_eq!(
        load_store(&fixture.home)
            .unwrap()
            .current
            .unwrap()
            .attempt_id,
        attempt
    );
    no_native_child(&fixture.home, &LostReply::default());
}

#[test]
fn cancellation_before_preparation_records_once_and_never_reads_or_dispatches_a_package() {
    let _diagnostic_fixture = DIAGNOSTIC_FIXTURE_SERIAL.blocking_lock();
    let fixture = Fixture::new();
    let request = input(fixture.root.path().join("absent-cancelled-package"));
    let attempt = accept(&fixture.home, request.clone()).unwrap().unwrap();
    let cancel = control_input(&request.request_id);
    let launch = LostReply::default();
    let installed = fs::read(fixture.home.join("native-installation.json")).unwrap();
    assert!(control(&fixture.home, &cancel, "cancel").unwrap().is_none());
    let requested = flow_bytes(&fixture.home);
    assert!(control(&fixture.home, &cancel, "cancel").unwrap().is_none());
    assert_eq!(flow_bytes(&fixture.home), requested);
    assert_eq!(
        control(&fixture.home, &cancel, "continue").unwrap_err().0,
        StatusCode::CONFLICT
    );
    let (result, scans) = crate::native_bundle::count_verifications(|| {
        prepare(&fixture.home, &request.request_id, &attempt, &launch)
    });
    result.unwrap();
    assert_eq!(scans, 0);
    let cancelled = flow_bytes(&fixture.home);
    assert!(control(&fixture.home, &cancel, "cancel").unwrap().is_none());
    assert_eq!(flow_bytes(&fixture.home), cancelled);
    let observed = projection(&fixture.home, &request.request_id, Some(&cancel.control_id));
    assert_eq!(observed["update"]["state"], "cancelled");
    assert_eq!(observed["control"]["status"], "recorded");
    assert_eq!(observed["control"]["action"], "cancel");
    no_native_child(&fixture.home, &launch);
    assert_eq!(
        fs::read(fixture.home.join("native-installation.json")).unwrap(),
        installed
    );
}

#[test]
fn full_preparation_dispatches_once_and_lost_acknowledgement_never_authorizes_another_attempt() {
    let _diagnostic_fixture = DIAGNOSTIC_FIXTURE_SERIAL.blocking_lock();
    let fixture = Fixture::new();
    let request = input(fixture.candidate.clone());
    let attempt = accept(&fixture.home, request.clone()).unwrap().unwrap();
    let launch = LostReply::default();
    let installed = fs::read(fixture.home.join("native-installation.json")).unwrap();
    prepare(&fixture.home, &request.request_id, &attempt, &launch).unwrap();
    assert_eq!(launch.calls.load(Ordering::SeqCst), 1);
    let retained = load_store(&fixture.home).unwrap().current.unwrap();
    let child = retained.operation_id.as_ref().unwrap();
    let ledger = load(&fixture.home).unwrap();
    assert_eq!(ledger.current_operation.as_ref(), Some(child));
    assert_eq!(ledger.operations[child].state, OperationState::Unknown);
    assert!(ledger.operations[child].outcome.is_none());
    assert_eq!(ledger.requests[child].operation_id, *child);
    let flow_before = flow_bytes(&fixture.home);
    let ledger_before = fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap();
    assert!(accept(&fixture.home, request.clone()).unwrap().is_none());
    assert!(prepare(&fixture.home, &request.request_id, &attempt, &launch).is_err());
    assert!(
        control(
            &fixture.home,
            &control_input(&request.request_id),
            "continue"
        )
        .is_err()
    );
    let observed = projection(&fixture.home, &request.request_id, None);
    assert_eq!(observed["operation"]["id"], *child);
    assert_eq!(observed["operation"]["phase"], "unknown");
    assert_eq!(observed["update"]["state"], "unknown");
    assert_eq!(observed["update"]["can_continue"], false);
    assert_eq!(flow_bytes(&fixture.home), flow_before);
    assert_eq!(
        fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap(),
        ledger_before
    );
    assert_eq!(launch.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read(fixture.home.join("native-installation.json")).unwrap(),
        installed
    );
    assert!(!fixture.home.join("native-update.json").exists());
}

#[test]
fn corrupt_candidate_refuses_before_native_child_and_preserves_the_installed_receipt() {
    let _diagnostic_fixture = DIAGNOSTIC_FIXTURE_SERIAL.blocking_lock();
    for relative in ["manifest.json", "bridge/live.mjs"] {
        let fixture = Fixture::new();
        let installed = fs::read(fixture.home.join("native-installation.json")).unwrap();
        let request = input(fixture.candidate.clone());
        let attempt = accept(&fixture.home, request.clone()).unwrap().unwrap();
        fs::write(
            fixture.candidate.join(relative),
            b"changed package contents",
        )
        .unwrap();
        let launch = LostReply::default();
        assert!(
            prepare(&fixture.home, &request.request_id, &attempt, &launch).is_err(),
            "accepted corrupt {relative}"
        );
        no_native_child(&fixture.home, &launch);
        assert!(load(&fixture.home).unwrap().checks.is_empty());
        assert!(
            load_store(&fixture.home)
                .unwrap()
                .current
                .unwrap()
                .operation_id
                .is_none()
        );
        assert_eq!(
            fs::read(fixture.home.join("native-installation.json")).unwrap(),
            installed
        );
        assert!(native_update::lock(&fixture.home, "native-update.lock").is_ok());
        assert!(native_update::lock(&fixture.home, PREPARATION_LOCK).is_ok());
    }
}

#[test]
fn request_and_control_status_project_no_package_path_secret_or_process_generation() {
    let fixture = Fixture::new();
    let package = fixture.root.path().join("private-secret-package-label");
    let request = input(package.clone());
    accept(&fixture.home, request.clone()).unwrap().unwrap();
    let cancel = control_input(&request.request_id);
    control(&fixture.home, &cancel, "cancel").unwrap();
    let saved = flow_bytes(&fixture.home);
    let observed = projection(&fixture.home, &request.request_id, Some(&cancel.control_id));
    assert_eq!(observed["request"]["id"], request.request_id);
    assert_eq!(observed["control"]["id"], cancel.control_id);
    assert_eq!(observed["update"]["can_cancel"], false);
    let public = observed.to_string();
    let token = fs::read_to_string(fixture.home.join("secrets/control-token")).unwrap();
    for excluded in [
        package.to_str().unwrap(),
        fixture.home.to_str().unwrap(),
        token.trim(),
        "private-secret-package-label",
        "package_path",
        "start_ticks",
        "boot_id",
        "creation_time_100ns",
    ] {
        assert!(
            !public.contains(excluded),
            "public status exposed {excluded}"
        );
    }
    assert_eq!(
        flow_bytes(&fixture.home),
        saved,
        "GET must not rewrite its receipt"
    );
}

#[test]
fn stale_controls_and_old_request_reads_never_mutate_a_new_parent() {
    let _diagnostic_fixture = DIAGNOSTIC_FIXTURE_SERIAL.blocking_lock();
    let fixture = Fixture::new();
    let first = input(fixture.candidate.clone());
    let attempt = accept(&fixture.home, first.clone()).unwrap().unwrap();
    let old_cancel = control_input(&first.request_id);
    control(&fixture.home, &old_cancel, "cancel").unwrap();
    let launch = LostReply::default();
    prepare(&fixture.home, &first.request_id, &attempt, &launch).unwrap();
    let next = input(fixture.candidate.clone());
    accept(&fixture.home, next.clone()).unwrap().unwrap();
    let current = flow_bytes(&fixture.home);
    for action in ["cancel", "continue"] {
        assert!(control(&fixture.home, &old_cancel, action).is_err());
        assert!(control(&fixture.home, &control_input(&first.request_id), action).is_err());
        assert_eq!(flow_bytes(&fixture.home), current);
    }
    let archived = projection(
        &fixture.home,
        &first.request_id,
        Some(&old_cancel.control_id),
    );
    assert_eq!(archived["update"]["state"], "cancelled");
    assert_eq!(archived["control"]["status"], "recorded");
    assert_eq!(archived["update"]["can_cancel"], false);
    assert_eq!(archived["update"]["can_continue"], false);
    let unknown_control = projection(
        &fixture.home,
        &next.request_id,
        Some(&old_cancel.control_id),
    );
    assert_eq!(unknown_control["update"]["id"], next.request_id);
    assert_eq!(unknown_control["control"]["status"], "not_recorded");
    assert!(accept(&fixture.home, first).unwrap().is_none());
    assert_eq!(flow_bytes(&fixture.home), current);
    no_native_child(&fixture.home, &launch);
}

#[test]
fn worker_waits_only_for_admission_classified_work_and_cancellation_still_wins() {
    use native_update::{WaitingUpdateDecision as Decision, WaitingUpdateEvent as Event};
    let fixture = Fixture::new();
    let request = input(fixture.candidate.clone());
    let attempt = accept(&fixture.home, request.clone()).unwrap().unwrap();
    let mut readiness = json!({"reason":"busy","closed":false,"ready":false,
    "waiting_for_work":true,"busy":[
        {"kind":"active_calls","count":1},
        {"kind":"retained_tool_servers","count":1},
        {"kind":"pending_or_uncertain_inputs","count":1}
    ]});
    assert_eq!(
        worker_control(
            &fixture.home,
            &request.request_id,
            &attempt,
            Event::Waiting(&readiness)
        )
        .unwrap(),
        Decision::Continue
    );
    assert_eq!(
        load_store(&fixture.home).unwrap().current.unwrap().state,
        State::Waiting
    );
    for change_value in [json!(false), Value::Null] {
        readiness["waiting_for_work"] = change_value;
        assert_eq!(
            worker_control(
                &fixture.home,
                &request.request_id,
                &attempt,
                Event::Waiting(&readiness)
            )
            .unwrap(),
            Decision::NeedsAttention
        );
    }
    readiness["waiting_for_work"] = json!(true);
    readiness["closed"] = json!(true);
    assert_eq!(
        worker_control(
            &fixture.home,
            &request.request_id,
            &attempt,
            Event::Waiting(&readiness)
        )
        .unwrap(),
        Decision::NeedsAttention
    );
    change(&fixture.home, &request.request_id, &attempt, |update| {
        update.cancel_requested = true;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        worker_control(
            &fixture.home,
            &request.request_id,
            &attempt,
            Event::Waiting(&readiness)
        )
        .unwrap(),
        Decision::Cancel
    );
    assert_eq!(
        worker_control(
            &fixture.home,
            &request.request_id,
            &attempt,
            Event::BeforeStop
        )
        .unwrap(),
        Decision::Cancel
    );
    no_native_child(&fixture.home, &LostReply::default());
}
