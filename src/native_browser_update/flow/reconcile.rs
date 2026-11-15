//! Reconcile a lost final outer-flow write from its exact retained native child.
//! Caller owns native-update.lock. This file only acquires the short flow lock;
//! it never dispatches, recovers, stops, or infers success from elapsed time.
use super::*;

pub(super) fn current(home: &Path) -> Result<bool> {
    let _flow = native_update::lock(home, LOCK)?;
    let mut store = load_store(home)?;
    let Some(update) = store.current.as_mut() else {
        return Ok(false);
    };
    let Some((state, phase)) = observed(home, update, &store.installation_id)? else {
        return Ok(false);
    };
    if update.state == state && update.phase == phase {
        return Ok(false);
    }
    touch(update, state, phase);
    save_store(home, &store)?;
    Ok(true)
}

/// Derive a terminal result from fixed saved records only. GET may use this
/// without mutation; dispatch callers must still reconcile under the native
/// lock before acting on the derived state.
pub(super) fn observed(
    home: &Path,
    update: &Update,
    installation_id: &str,
) -> Result<Option<(State, &'static str)>> {
    // A final receipt may be written before its worker returns. Do not race its
    // final outer write or admit a replacement while it retains either owner.
    if worker_active(home)? || preparation_active(home)? {
        return Ok(None);
    }
    let ledger = load(home)?;
    let journal = native_update::recovery_snapshot(home)?;
    let Some(operation_id) = &update.operation_id else {
        // Cancellation of a departed preparation owner needs no new task or
        // repeated POST. GET derives this only; mutation callers persist it
        // under native ownership before replacing the retained request.
        return Ok(cancelled_before_child(update, &ledger, journal.as_ref())?
            .then_some((State::Cancelled, "cancelled")));
    };
    if ledger.current_operation.as_deref() != Some(operation_id) {
        return Ok(None);
    }
    let Some(mut operation) = get_operation(home, &ledger, operation_id)? else {
        return Ok(None);
    };
    ensure!(
        operation.id == *operation_id
            && operation.request_id.as_deref() == Some(operation_id)
            && operation.kind == OperationKind::Apply
            && operation.plan["installation_id"] == installation_id,
        "The retained Update child binding changed"
    );
    let Some(request) = get_request(home, &ledger, operation_id)? else {
        return Ok(None);
    };
    ensure!(
        request.operation_id == *operation_id
            && matches!(request.payload, RequestPayload::Apply { .. }),
        "The retained Update child request changed"
    );
    if journal
        .as_ref()
        .is_some_and(|j| j.transaction != operation.transaction_id || j.phase != "restored")
    {
        return Ok(None);
    }
    settle(&mut operation, journal.as_ref());
    if !verified_terminal(&operation) {
        return Ok(None);
    }
    let (state, phase) = match operation.outcome.as_deref() {
        Some("applied") => (State::Completed, "completed"),
        Some("rolled_back") => (State::Failed, "rolled_back"),
        Some("unchanged") if update.cancel_requested => (State::Cancelled, "cancelled"),
        Some("unchanged") if update.state == State::NeedsAttention => {
            (State::NeedsAttention, "needs_attention")
        }
        Some("unchanged") => (State::Failed, "failed"),
        _ => return Ok(None),
    };
    ensure!(
        !finished(update) || (update.state == state && update.phase == phase),
        "Retained Update and native terminal outcomes disagree"
    );
    Ok(Some((state, phase)))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _root: tempfile::TempDir,
        home: PathBuf,
        operation: String,
    }
    impl Fixture {
        fn new(outcome: Option<&str>, state: State, cancelled: bool) -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            amplifier_native_platform::private_dir(&home).unwrap();
            let config = Config::local(&home, "Flow reconciliation", 54127).unwrap();
            config.save(&home).unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let operation = uuid::Uuid::new_v4().to_string();
            let attempt = uuid::Uuid::new_v4().to_string();
            let check_id = uuid::Uuid::new_v4().to_string();
            let plan = json!({"installation_id":config.id,"version":"fixture"});
            let mut ledger = Ledger::empty(config.id.clone());
            ledger.current_operation = Some(operation.clone());
            ledger.operations.insert(
                operation.clone(),
                Operation {
                    id: operation.clone(),
                    request_id: Some(operation.clone()),
                    transaction_id: uuid::Uuid::new_v4().to_string(),
                    kind: OperationKind::Apply,
                    previous_transaction: None,
                    plan: plan.clone(),
                    helper: Value::Null,
                    attempt: Attempt {
                        id: attempt.clone(),
                        action: None,
                        claimed: true,
                        handoff_nonce: "retained-test-nonce".into(),
                        launch: None,
                        worker_process: None,
                    },
                    state: if outcome.is_some() {
                        OperationState::Completed
                    } else {
                        OperationState::Unknown
                    },
                    outcome: outcome.map(str::to_owned),
                },
            );
            ledger.requests.insert(
                operation.clone(),
                RequestRecord {
                    payload: RequestPayload::Apply { check_id },
                    operation_id: operation.clone(),
                    attempt_id: attempt.clone(),
                },
            );
            save(&home, &ledger).unwrap();
            let update = Update {
                id: id.clone(),
                input: Input {
                    request_id: id,
                    offer_id: None,
                    package_path: Some(root.path().join("package")),
                },
                package: root.path().join("package"),
                version: Some("fixture".into()),
                attempt_id: attempt,
                owner: json!({"fixture":"no-process-observation"}),
                operation_id: Some(operation.clone()),
                state,
                phase: "lost-final-flow-write".into(),
                revision: 7,
                created_at: 1,
                updated_at: 2,
                cancel_requested: cancelled,
                controls: BTreeMap::new(),
                prior_attempts: Vec::new(),
            };
            save_store(
                &home,
                &Store {
                    schema: 1,
                    installation_id: config.id,
                    current: Some(update),
                },
            )
            .unwrap();
            Self {
                _root: root,
                home,
                operation,
            }
        }
        fn run(&self) -> Result<bool> {
            let _native = native_update::lock(&self.home, "native-update.lock")?;
            current(&self.home)
        }
        fn bytes(&self) -> Vec<u8> {
            fs::read(self.home.join(DIRECTORY).join(FILE)).unwrap()
        }
    }

    #[test]
    fn lost_final_flow_write_reconciles_exact_applied_and_rollback_once() {
        for (outcome, state, phase) in [
            ("applied", State::Completed, "completed"),
            ("rolled_back", State::Failed, "rolled_back"),
        ] {
            let f = Fixture::new(Some(outcome), State::Installing, false);
            let native_before = fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap();
            let flow_before = f.bytes();
            let store = load_store(&f.home).unwrap();
            assert_eq!(
                observed(
                    &f.home,
                    store.current.as_ref().unwrap(),
                    &store.installation_id
                )
                .unwrap(),
                Some((state.clone(), phase))
            );
            assert_eq!(f.bytes(), flow_before, "GET observation must not write");
            assert!(f.run().unwrap());
            let value = load_store(&f.home).unwrap().current.unwrap();
            assert_eq!(value.state, state);
            assert_eq!(value.phase, phase);
            assert_eq!(value.revision, 8);
            let settled = f.bytes();
            assert!(!f.run().unwrap());
            assert_eq!(f.bytes(), settled);
            assert_eq!(
                fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap(),
                native_before
            );
        }
    }

    #[test]
    fn unchanged_cancel_attention_and_failure_keep_distinct_outer_outcomes() {
        for (prior, cancelled, expected, phase) in [
            (State::Waiting, true, State::Cancelled, "cancelled"),
            (
                State::NeedsAttention,
                false,
                State::NeedsAttention,
                "needs_attention",
            ),
            (State::Unknown, false, State::Failed, "failed"),
        ] {
            let f = Fixture::new(Some("unchanged"), prior, cancelled);
            assert!(f.run().unwrap());
            let value = load_store(&f.home).unwrap().current.unwrap();
            assert_eq!(value.state, expected);
            assert_eq!(value.phase, phase);
            assert_eq!(value.cancel_requested, cancelled);
        }
    }

    #[test]
    fn exact_restored_journal_closes_an_unsettled_child_without_rewriting_it() {
        let f = Fixture::new(None, State::Installing, false);
        let ledger = load(&f.home).unwrap();
        let operation = ledger.operations.get(&f.operation).unwrap();
        let before = fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap();
        let journal = json!({"schema":1,"installation_id":ledger.installation_id,
            "transaction_id":operation.transaction_id,"phase":"restored",
            "direction":"applied","restore_outcome":"applied"});
        amplifier_native_platform::private_write_data(
            &f.home.join("native-update.json"),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        assert!(f.run().unwrap());
        assert_eq!(
            load_store(&f.home).unwrap().current.unwrap().state,
            State::Completed
        );
        assert_eq!(
            fs::read(f.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            before
        );
    }

    #[test]
    fn missing_unknown_unbound_or_active_child_never_settles() {
        for change in [
            "unknown",
            "missing",
            "not_current",
            "wrong_id",
            "wrong_installation",
            "wrong_request",
            "missing_request",
            "nonterminal_journal",
            "foreign_journal",
            "worker",
            "preparation",
        ] {
            let f = Fixture::new(Some("applied"), State::Installing, false);
            let before = f.bytes();
            let mut ledger = load(&f.home).unwrap();
            match change {
                "unknown" => {
                    let op = ledger.operations.get_mut(&f.operation).unwrap();
                    op.state = OperationState::Unknown;
                    op.outcome = None;
                }
                "missing" => {
                    ledger.operations.remove(&f.operation);
                }
                "not_current" => ledger.current_operation = None,
                "wrong_id" => {
                    ledger.operations.get_mut(&f.operation).unwrap().id =
                        uuid::Uuid::new_v4().to_string()
                }
                "wrong_installation" => {
                    ledger.operations.get_mut(&f.operation).unwrap().plan["installation_id"] =
                        json!("foreign")
                }
                "wrong_request" => {
                    ledger.requests.get_mut(&f.operation).unwrap().operation_id =
                        uuid::Uuid::new_v4().to_string()
                }
                "missing_request" => {
                    ledger.requests.remove(&f.operation);
                }
                "nonterminal_journal" | "foreign_journal" => {
                    let operation = ledger.operations.get(&f.operation).unwrap();
                    let journal = json!({"schema":1,"installation_id":ledger.installation_id,
                        "transaction_id":if change == "foreign_journal" {
                            uuid::Uuid::new_v4().to_string()
                        } else {operation.transaction_id.clone()},
                        "phase":if change == "nonterminal_journal" {"restoring"} else {"restored"},
                        "direction":"applied","restore_outcome":"applied"});
                    amplifier_native_platform::private_write_data(
                        &f.home.join("native-update.json"),
                        &serde_json::to_vec(&journal).unwrap(),
                    )
                    .unwrap();
                }
                _ => {}
            }
            // Fixed private input corruption fixtures avoid compaction, which
            // correctly requires complete records for normal production saves.
            amplifier_native_platform::private_write_data(
                &f.home.join(DIRECTORY).join(LEDGER),
                &serde_json::to_vec(&ledger).unwrap(),
            )
            .unwrap();
            let _owner = match change {
                "worker" => Some(native_update::lock(&f.home, WORKER_LOCK).unwrap()),
                "preparation" => Some(native_update::lock(&f.home, PREPARATION_LOCK).unwrap()),
                _ => None,
            };
            let result = f.run();
            assert!(
                !matches!(result, Ok(true)),
                "unexpected settlement for {change}"
            );
            assert_eq!(f.bytes(), before, "rewrote {change}");
        }
    }

    #[test]
    fn a_conflicting_terminal_flow_is_preserved_as_an_error() {
        let f = Fixture::new(Some("applied"), State::Cancelled, true);
        let before = f.bytes();
        assert!(f.run().is_err());
        assert_eq!(f.bytes(), before);
    }
}
