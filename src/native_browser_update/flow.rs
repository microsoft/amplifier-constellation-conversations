//! One retained user intent across package preparation and the existing native
//! worker. The short flow lock never surrounds hashing, copying or a host call.
//! Lock order when both are needed is native-update -> flow. GET only observes.
use super::*;
use std::time::Instant;
#[cfg(test)]
mod cancelled_owner_tests;
mod reconcile;
#[cfg(test)]
mod tests;

pub(super) fn reconcile_current(home: &Path) -> Result<()> {
    reconcile::current(home).map(|_| ())
}

macro_rules! require_api {
    ($condition:expr, $message:literal) => {
        if !$condition {
            return Err(conflict($message));
        }
    };
}

const FILE: &str = "update-flow.json";
const LOCK: &str = "native-browser-flow.lock";
const PREPARATION_LOCK: &str = "native-browser-preparation.lock";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlInput {
    pub request_id: String,
    pub control_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Accepted,
    Preparing,
    Waiting,
    NeedsAttention,
    Installing,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    action: String,
    outcome: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttemptRecord {
    id: String,
    operation_id: Option<String>,
    state: State,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    id: String,
    input: Input,
    package: PathBuf,
    version: Option<String>,
    attempt_id: String,
    owner: Value,
    operation_id: Option<String>,
    state: State,
    phase: String,
    revision: u64,
    created_at: i64,
    updated_at: i64,
    cancel_requested: bool,
    controls: BTreeMap<String, Control>,
    prior_attempts: Vec<AttemptRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    schema: u32,
    installation_id: String,
    current: Option<Update>,
}

fn load_store(home: &Path) -> Result<Store> {
    let id = Config::load(home)?.id;
    let path = home.join(DIRECTORY).join(FILE);
    if !path.exists() && !path.is_symlink() {
        return Ok(Store {
            schema: 1,
            installation_id: id,
            current: None,
        });
    }
    ordinary(&home.join(DIRECTORY), true)?;
    ensure!(
        ordinary(&path, false)?.len() <= MAX_LEDGER_BYTES as u64,
        "Update request exceeds its bound"
    );
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() <= MAX_LEDGER_BYTES,
        "Update request exceeds its bound"
    );
    let value: Store = serde_json::from_slice(&bytes)?;
    ensure!(
        value.schema == 1 && value.installation_id == id,
        "Update installation identity changed"
    );
    if let Some(current) = &value.current {
        uuid(&current.id)?;
        uuid(&current.attempt_id)?;
        ensure!(
            current.id == current.input.request_id,
            "Update request identity changed"
        );
    }
    Ok(value)
}

fn save_store(home: &Path, store: &Store) -> Result<()> {
    let bytes = serde_json::to_vec(store)?;
    ensure!(
        bytes.len() <= MAX_LEDGER_BYTES,
        "Update request exceeds its bound"
    );
    amplifier_native_platform::private_dir(&home.join(DIRECTORY))?;
    amplifier_native_platform::private_write_data(&home.join(DIRECTORY).join(FILE), &bytes)
}

fn get(home: &Path, store: &Store, id: &str) -> Result<Option<Update>> {
    uuid(id)?;
    if let Some(current) = &store.current
        && current.id == id
    {
        return Ok(Some(current.clone()));
    }
    history_read(home, &store.installation_id, "updates", id)
}

fn finished(update: &Update) -> bool {
    matches!(
        update.state,
        State::Completed | State::Cancelled | State::Failed
    )
}

fn touch(update: &mut Update, state: State, phase: &str) {
    update.state = state;
    update.phase = phase.into();
    update.revision += 1;
    update.updated_at = crate::state::now_ms();
}

fn preparation_active(home: &Path) -> Result<bool> {
    if !home.join(PREPARATION_LOCK).exists() {
        return Ok(false);
    }
    ordinary(&home.join(PREPARATION_LOCK), false)?;
    match native_update::lock(home, PREPARATION_LOCK) {
        Ok(guard) => {
            drop(guard);
            Ok(false)
        }
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            }) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn settled_for_assistant_close(home: &Path) -> Result<bool> {
    if preparation_active(home)? || worker_active(home)? {
        return Ok(false);
    }
    let store = load_store(home)?;
    let Some(current) = store.current.as_ref() else {
        return Ok(true);
    };
    if finished(current) {
        return Ok(true);
    }
    Ok(
        reconcile::observed(home, current, &store.installation_id)?.is_some_and(|(state, _)| {
            matches!(state, State::Completed | State::Failed | State::Cancelled)
        }),
    )
}

/// A long-running legacy control must not bypass an admitted single Update.
/// Call this under the existing native installation lock, never under flow lock.
pub(super) fn require_available(home: &Path, allowed: Option<&str>) -> Result<()> {
    let store = load_store(home)?;
    if let Some(current) = store.current {
        ensure!(
            finished(&current) || allowed == Some(current.id.as_str()),
            "An Update request already owns preparation or recovery; observe that request"
        );
    }
    Ok(())
}

pub(super) fn require_recovery(home: &Path, operation_id: &str) -> Result<()> {
    let store = load_store(home)?;
    if let Some(current) = store.current.filter(|u| !finished(u)) {
        ensure!(
            current.operation_id.as_deref() == Some(operation_id),
            "The retained Update owns a different operation; observe its exact recovery state"
        );
    }
    Ok(())
}

fn owner() -> Result<Value> {
    amplifier_native_platform::process::identity(std::process::id())?
        .context("Cannot establish the Update owner generation")
}

fn same_owner(update: &Update) -> Result<bool> {
    amplifier_native_platform::process::matches(&update.owner)
}

fn can_continue(home: &Path, update: &Update) -> Result<bool> {
    if preparation_active(home)? || worker_active(home)? {
        return Ok(false);
    }
    if let Some(operation_id) = &update.operation_id {
        let ledger = load(home)?;
        let Some(mut operation) = get_operation(home, &ledger, operation_id)? else {
            // Dispatch intent with no authoritative operation remains uncertain.
            return Ok(false);
        };
        let journal = native_update::recovery_snapshot(home)?;
        settle(&mut operation, journal.as_ref());
        return Ok(verified_terminal(&operation)
            && operation.outcome.as_deref() == Some("unchanged")
            && matches!(update.state, State::NeedsAttention | State::Failed));
    }
    Ok(
        matches!(update.state, State::Failed | State::NeedsAttention)
            || matches!(update.state, State::Accepted | State::Preparing) && !same_owner(update)?,
    )
}

fn message(state: &str, phase: &str, cancelled: bool) -> &'static str {
    if cancelled
        && matches!(
            state,
            "accepted" | "preparing" | "waiting" | "needs_attention"
        )
    {
        return "Stopping the update preparation. Waiting for current file operations to finish.";
    }
    match state {
        "accepted" => "Update requested. You can keep using Companion while it prepares.",
        "preparing" => match phase {
            "check_previous_verify" => "Checking the current installation.",
            "check_stage" => "Verifying and preparing the complete update package.",
            "check_launcher" | "check_binding_validation" => {
                "Confirming the prepared package and installation."
            }
            _ => "Preparing the update. Your current version is still running.",
        },
        "waiting" if phase == "starting" => "Starting the updater. Companion remains available.",
        "waiting" => "Waiting for active calls and work to finish. Companion remains available.",
        "needs_attention" => {
            "Earlier work or an installation change needs attention before updating."
        }
        "installing" => {
            "Installing and verifying the update. This page will reconnect to the same operation."
        }
        "completed" if phase == "already_current" => "This version is already installed.",
        "completed" => "Companion is updated and available.",
        "cancelled" => "Update cancelled before installation. Your current version is unchanged.",
        "failed" if phase == "rolled_back" => {
            "The update failed. Your previous version is running with your current data."
        }
        "failed" => "The update could not be prepared. Your current version is unchanged.",
        "interrupted" => "Preparation was interrupted. Continue to verify the package again.",
        _ => "The update outcome is not confirmed. Observe the saved operation before recovery.",
    }
}

/// Add request-bound, cheap saved state to the existing observational response.
pub(super) fn augment(
    home: &Path,
    value: &mut Value,
    request_id: Option<&str>,
    offer_id: Option<&str>,
    control_id: Option<&str>,
) -> Result<()> {
    let store = load_store(home)?;
    let mut update = match request_id {
        Some(id) => get(home, &store, id)?,
        None => store.current.clone(),
    };
    let current_finished = match store.current.as_ref() {
        None => true,
        Some(current) if finished(current) => true,
        Some(current) => {
            reconcile::observed(home, current, &store.installation_id)?.is_some_and(|(state, _)| {
                matches!(state, State::Completed | State::Failed | State::Cancelled)
            })
        }
    };
    if let Some(current) = update.as_mut()
        && let Some((state, phase)) = reconcile::observed(home, current, &store.installation_id)?
    {
        current.state = state;
        current.phase = phase.into();
    }
    value["actions"]["request"] = json!(
        value["owner"] == "standalone"
            && value["mode"] == "normal"
            && current_finished
            && value["actions"]["check"] == true
    );
    if !current_finished {
        value["actions"]["check"] = json!(false);
        value["actions"]["apply"] = json!(false);
    }
    value["update"] = Value::Null;
    if let Some(update) = update {
        let is_current = store.current.as_ref().is_some_and(|u| u.id == update.id);
        let mut state = serde_json::to_value(&update.state)?
            .as_str()
            .unwrap()
            .to_string();
        let mut phase = update.phase.clone();
        if let Some(op_id) = &update.operation_id {
            let ledger = load(home)?;
            if let Some(mut op) = get_operation(home, &ledger, op_id)? {
                let journal = native_update::recovery_snapshot(home)?;
                settle(&mut op, journal.as_ref());
                // An explicit recovery/rollback request selects its own native
                // operation. Do not hide it behind an older parent Update.
                if request_id.is_none() || request_id == Some(update.id.as_str()) {
                    value["operation"] = summary(&op, journal.as_ref(), worker_active(home)?);
                }
                if verified_terminal(&op) {
                    match op.outcome.as_deref() {
                        Some("applied") => {
                            state = "completed".into();
                            phase = "completed".into();
                        }
                        Some("rolled_back") => {
                            state = "failed".into();
                            phase = "rolled_back".into();
                        }
                        _ => {}
                    }
                } else if !worker_active(home)?
                    && (update.state == State::Installing
                        || matches!(op.state, OperationState::Unknown | OperationState::Running))
                {
                    state = "unknown".into();
                } else if op.state == OperationState::Accepted {
                    phase = "starting".into();
                }
            } else {
                state = "unknown".into();
            }
        } else if matches!(update.state, State::Accepted | State::Preparing)
            && !same_owner(&update)?
            && !preparation_active(home)?
        {
            state = "interrupted".into();
        }
        let cancellation = is_current
            && matches!(
                update.state,
                State::Accepted | State::Preparing | State::Waiting | State::NeedsAttention
            )
            && !update.cancel_requested;
        value["update"] = json!({"id":update.id,"state":state,"phase":phase,
            "message":message(&state,&phase,update.cancel_requested),"version":update.version,
            "revision":update.revision,"created_at":update.created_at,"updated_at":update.updated_at,
            "operation_id":update.operation_id,"cancel_requested":update.cancel_requested,
            "can_cancel":cancellation,"can_continue":is_current && can_continue(home,&update)?});
        if request_id == Some(&update.id) {
            value["request"] =
                json!({"id":update.id,"state":"recorded","operation_id":update.operation_id});
        }
        if let Some(id) = control_id {
            uuid(id)?;
            value["control"] = match update.controls.get(id) {
                Some(c) => {
                    json!({"id":id,"action":c.action,"status":"recorded","outcome":c.outcome})
                }
                None => json!({"id":id,"status":"not_recorded"}),
            };
        }
    }
    let offer = if let Some(id) = offer_id {
        Some(crate::native_update_offer::read_offer(home, id)?)
    } else {
        crate::native_update_offer::latest_offer(home)?
    };
    let offer = offer.filter(|o| {
        crate::native_update_offer::newer(
            &o.target_version,
            value["current"]["version"].as_str().unwrap_or(""),
        )
    });
    value["offer"] = offer
        .map(|o| {
            json!({"id":o.id,"version":o.target_version,
        "current_version":value["current"]["version"],"verified":false})
        })
        .unwrap_or(Value::Null);
    Ok(())
}

fn selected(input: &Input, home: &Path) -> Result<(PathBuf, Option<String>)> {
    ensure!(
        input.offer_id.is_some() != input.package_path.is_some(),
        "Choose one offered or advanced package"
    );
    if let Some(id) = &input.offer_id {
        let offer = crate::native_update_offer::read_offer(home, id)?;
        return Ok((offer.package_folder, Some(offer.target_version)));
    }
    let package = input
        .package_path
        .clone()
        .context("Missing update package")?;
    ensure!(
        package.is_absolute() && package.as_os_str().len() <= 4096,
        "Choose an absolute extracted update package"
    );
    Ok((package, None))
}

/// Return an attempt to schedule only for a genuinely new retained request.
pub(super) fn accept(home: &Path, input: Input) -> ApiResult<Option<String>> {
    uuid(&input.request_id)?;
    standalone(home)?;
    {
        let _guard = native_update::lock(home, LOCK)?;
        let store = load_store(home)?;
        if let Some(previous) = get(home, &store, &input.request_id)? {
            require_api!(
                previous.input == input,
                "This Update ID already names a different package"
            );
            return Ok(None);
        }
    }
    // Nonblocking lock acquisition rejects competing legacy mutations promptly.
    let _native = native_update::lock(home, "native-update.lock")?;
    reconcile::current(home)?;
    let _guard = native_update::lock(home, LOCK)?;
    let mut store = load_store(home)?;
    if let Some(previous) = get(home, &store, &input.request_id)? {
        require_api!(
            previous.input == input,
            "This Update ID already names a different package"
        );
        return Ok(None);
    }
    require_api!(
        store.current.as_ref().is_none_or(finished),
        "An Update request is already retained"
    );
    require_api!(
        !worker_active(home)? && !preparation_active(home)?,
        "An update worker is still active"
    );
    let mut ledger = load(home)?;
    settle_current(home, &mut ledger)?;
    require_api!(
        current_operation(&ledger)?.is_none_or(terminal),
        "A retained update needs recovery first"
    );
    require_api!(
        native_update::recovery_snapshot(home)?.is_none_or(|j| j.phase == "restored"),
        "A retained native transaction needs recovery first"
    );
    let (package, version) = selected(&input, home)?;
    if let Some(previous) = &store.current {
        history_write(
            home,
            &store.installation_id,
            "updates",
            &previous.id,
            previous,
        )?;
    }
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let now = crate::state::now_ms();
    store.current = Some(Update {
        id: input.request_id.clone(),
        input,
        package,
        version,
        attempt_id: attempt_id.clone(),
        owner: owner()?,
        operation_id: None,
        state: State::Accepted,
        phase: "accepted".into(),
        revision: 1,
        created_at: now,
        updated_at: now,
        cancel_requested: false,
        controls: BTreeMap::new(),
        prior_attempts: Vec::new(),
    });
    save_store(home, &store)?;
    Ok(Some(attempt_id))
}

pub(super) fn control(
    home: &Path,
    input: &ControlInput,
    action: &str,
) -> ApiResult<Option<String>> {
    uuid(&input.request_id)?;
    uuid(&input.control_id)?;
    standalone(home)?;
    require_api!(
        matches!(action, "cancel" | "continue"),
        "Unknown Update control"
    );
    let _guard = native_update::lock(home, LOCK)?;
    let mut store = load_store(home)?;
    let current = store
        .current
        .as_mut()
        .filter(|u| u.id == input.request_id)
        .context("Only the current Update can be controlled")?;
    if let Some(receipt) = current.controls.get(&input.control_id) {
        require_api!(
            receipt.action == action,
            "Control ID already names a different action"
        );
        drop(_guard);
        if action == "cancel" {
            // Re-observing the same control can settle its existing intent;
            // it never schedules or repeats preparation or a native action.
            let _ = settle_cancelled_owner(home, &input.request_id);
        }
        return Ok(None);
    }
    require_api!(
        current.controls.len() < 128 && current.prior_attempts.len() < 64,
        "Retained Update controls exceed their bound"
    );
    let next = if action == "cancel" {
        require_api!(
            matches!(
                current.state,
                State::Accepted | State::Preparing | State::Waiting | State::NeedsAttention
            ),
            "Installation has begun; observe its recovery state instead of cancelling"
        );
        current.cancel_requested = true;
        current.revision += 1;
        current.updated_at = crate::state::now_ms();
        None
    } else {
        require_api!(
            can_continue(home, current)?,
            "The previous attempt has not settled; no repeat worker was started"
        );
        current.prior_attempts.push(AttemptRecord {
            id: current.attempt_id.clone(),
            operation_id: current.operation_id.clone(),
            state: current.state.clone(),
        });
        current.attempt_id = uuid::Uuid::new_v4().to_string();
        current.owner = owner()?;
        current.operation_id = None;
        current.cancel_requested = false;
        touch(current, State::Accepted, "accepted");
        Some(current.attempt_id.clone())
    };
    current.controls.insert(
        input.control_id.clone(),
        Control {
            action: action.into(),
            outcome: "accepted".into(),
        },
    );
    save_store(home, &store)?;
    drop(_guard);
    if action == "cancel" {
        // The intent is durable already. Unavailable or conflicting evidence
        // leaves it pending; it is not evidence of a cancelled native action.
        let _ = settle_cancelled_owner(home, &input.request_id);
    }
    Ok(next)
}

/// Finish cancellation only after all relevant kernel ownership is available.
/// Do not wait or acquire native ownership while holding the short flow lock.
/// A missing/conflicting native child remains unknown, never reconstructed.
fn settle_cancelled_owner(home: &Path, id: &str) -> Result<bool> {
    let mut owners = Vec::new();
    for name in ["native-update.lock", PREPARATION_LOCK, WORKER_LOCK, LOCK] {
        match native_update::lock(home, name) {
            Ok(owner) => owners.push(owner),
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                }) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    let mut store = load_store(home)?;
    let Some(update) = store.current.as_mut().filter(|u| u.id == id) else {
        return Ok(false);
    };
    if !update.cancel_requested || finished(update) {
        return Ok(false);
    }
    let ledger = load(home)?;
    let journal = native_update::recovery_snapshot(home)?;
    if let Some(child) = &update.operation_id {
        if update.state != State::NeedsAttention
            || ledger.current_operation.as_deref() != Some(child)
        {
            return Ok(false);
        }
        let Some(operation) = get_operation(home, &ledger, child)? else {
            return Ok(false);
        };
        let Some(request) = get_request(home, &ledger, child)? else {
            return Ok(false);
        };
        ensure!(
            operation.id == *child
                && operation.request_id.as_deref() == Some(child)
                && operation.kind == OperationKind::Apply
                && operation.plan["installation_id"] == store.installation_id
                && request.operation_id == *child
                && matches!(request.payload, RequestPayload::Apply { .. }),
            "The cancelled Update child binding changed"
        );
        if !verified_terminal(&operation)
            || operation.outcome.as_deref() != Some("unchanged")
            || !journal.as_ref().is_some_and(|j| {
                j.transaction == operation.transaction_id
                    && j.phase == "restored"
                    && j.standalone
                    && j.direction == "unchanged"
                    && j.outcome.as_deref() == Some("unchanged")
                    && !j.rollback_aborted
            })
        {
            return Ok(false);
        }
    } else if !cancelled_before_child(update, &ledger, journal.as_ref())? {
        return Ok(false);
    }
    touch(update, State::Cancelled, "cancelled");
    save_store(home, &store)?;
    Ok(true)
}

/// Pure evidence predicate. The caller either retains preparation/worker
/// ownership or observes both free; a missing child record is never this case.
fn cancelled_before_child(
    update: &Update,
    ledger: &Ledger,
    journal: Option<&native_update::RecoverySnapshot>,
) -> Result<bool> {
    Ok(update.cancel_requested
        && update.operation_id.is_none()
        && matches!(update.state, State::Accepted | State::Preparing)
        && !same_owner(update)?
        && current_operation(ledger)?.is_none_or(verified_terminal)
        && journal.is_none_or(|j| j.phase == "restored"))
}

fn change(
    home: &Path,
    id: &str,
    attempt: &str,
    change: impl FnOnce(&mut Update) -> Result<()>,
) -> Result<()> {
    let _guard = native_update::lock(home, LOCK)?;
    let mut store = load_store(home)?;
    let current = store
        .current
        .as_mut()
        .filter(|u| u.id == id && u.attempt_id == attempt)
        .context("Update attempt was superseded")?;
    change(current)?;
    save_store(home, &store)
}

fn cancellation(home: &Path, id: &str, attempt: &str) -> Result<bool> {
    let store = load_store(home)?;
    let current = store
        .current
        .as_ref()
        .filter(|u| u.id == id && u.attempt_id == attempt)
        .context("Update attempt was superseded")?;
    Ok(current.cancel_requested)
}

pub(super) fn spawn(home: PathBuf, id: String, attempt: String) {
    // The HTTP observer does not own the job lifetime. The durable identity was
    // committed before this task is scheduled; a crash never auto-resubmits it.
    tokio::spawn(async move {
        let h = home.clone();
        let i = id.clone();
        let a = attempt.clone();
        let result = tokio::task::spawn_blocking(move || prepare(&h, &i, &a, launcher())).await;
        if !matches!(result, Ok(Ok(()))) {
            let _ = preparation_failed(&home, &id, &attempt);
        }
    });
}

fn preparation_failed(home: &Path, id: &str, attempt: &str) -> Result<()> {
    change(home, id, attempt, |u| {
        // A late task continuation cannot replace a proved terminal result,
        // including cancellation settled after its execution ownership ended.
        if !finished(u) {
            let state = if u.operation_id.is_some() {
                State::Unknown
            } else {
                State::Failed
            };
            touch(u, state, "preparation_failed");
        }
        Ok(())
    })
}

fn prepare(home: &Path, id: &str, attempt: &str, launch: &dyn WorkerLauncher) -> Result<()> {
    let _execution = native_update::lock(home, PREPARATION_LOCK)?;
    let mut package = PathBuf::new();
    change(home, id, attempt, |u| {
        ensure!(
            u.state == State::Accepted,
            "This preparation attempt is already claimed"
        );
        ensure!(same_owner(u)?, "Preparation owner changed");
        package = u.package.clone();
        touch(u, State::Preparing, "preparing");
        Ok(())
    })?;
    if cancellation(home, id, attempt)? {
        return change(home, id, attempt, |u| {
            touch(u, State::Cancelled, "cancelled");
            Ok(())
        });
    }
    let h = home.to_owned();
    let i = id.to_owned();
    let a = attempt.to_owned();
    let callback = move |stage: check_diagnostics::Stage| {
        let _ = change(&h, &i, &a, |u| {
            if u.state == State::Preparing {
                touch(u, State::Preparing, stage.name());
            }
            Ok(())
        });
    };
    let check_id = check_diagnostics::with_progress(callback, || {
        check_diagnostics::run(home, Instant::now(), || {
            check_diagnostics::observe(Stage::Cleanup, || {
                crate::native_browser_cleanup::before_update(home, id)
                    .map_err(|e| anyhow::anyhow!(e.1))
            })?;
            let selected = crate::desktop::selected_package_folder(&package)?;
            let checked = native_update::prepare_plan(home, &selected.bundle)?;
            selected.verify_launcher(&checked)?;
            checked.validate_current()?;
            let plan = checked.value();
            let retained = load_store(home)?
                .current
                .context("Missing retained Update")?;
            ensure!(
                retained.id == id && retained.attempt_id == attempt,
                "The retained package choice changed during preparation"
            );
            if retained.input.offer_id.is_some() {
                ensure!(
                    plan["version"].as_str() == retained.version.as_deref(),
                    "The verified package version differs from the downloaded offer"
                );
                ensure!(
                    plan["available"] == false
                        || crate::native_update_offer::newer(
                            plan["version"].as_str().unwrap_or(""),
                            plan["previous"]["bundle"]["version"].as_str().unwrap_or("")
                        ),
                    "This download is no longer newer than the installed version"
                );
            }
            let check_id = uuid::Uuid::new_v4().to_string();
            let mut ledger = load(home)?;
            standalone(home).map_err(|e| anyhow::anyhow!(e.1))?;
            require_available(home, Some(id))?;
            ensure!(!worker_active(home)?, "An update worker is already running");
            settle_current(home, &mut ledger)?;
            ensure!(
                current_operation(&ledger)?.is_none_or(terminal),
                "A retained update needs recovery"
            );
            if cancellation(home, id, attempt)? {
                change(home, id, attempt, |u| {
                    touch(u, State::Cancelled, "cancelled");
                    Ok(())
                })?;
                return Ok(None);
            }
            change(home, id, attempt, |u| {
                u.version = plan["version"].as_str().map(str::to_owned);
                if plan["available"] == false {
                    touch(u, State::Completed, "already_current");
                }
                Ok(())
            })?;
            if plan["available"] == false {
                return Ok(None);
            }
            // This new package owns the new single-Update worker code. Existing
            // installed helper bytes need not understand this additive flow.
            ledger.checks.insert(
                check_id.clone(),
                CheckedPackage {
                    id: check_id.clone(),
                    plan: plan.clone(),
                    helper: plan["candidate"].clone(),
                },
            );
            ledger.current_check = Some(check_id.clone());
            save(home, &ledger)?;
            Ok::<_, anyhow::Error>(Some(check_id))
        })
    })?;
    let Some(check_id) = check_id else {
        return Ok(());
    };
    let operation_id = uuid::Uuid::new_v4().to_string();
    // Promotion is not another browser action. Existing native dispatch keeps
    // its immutable child identity and one-shot launch/unknown semantics.
    apply_for_update(
        home,
        ApplyInput {
            request_id: operation_id,
            check_id,
        },
        launch,
        Some((id, attempt)),
    )
    .map_err(|e| anyhow::anyhow!(e.1))?;
    Ok(())
}

/// Called under native-update.lock after all expensive validation, immediately
/// before the child native operation is durably published. Cancellation and this
/// fence serialize on the short flow lock; no cancelled request can dispatch.
pub(super) fn bind_operation(
    home: &Path,
    id: &str,
    attempt: &str,
    operation: &Operation,
    publish: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    let _guard = native_update::lock(home, LOCK)?;
    let mut store = load_store(home)?;
    let current = store
        .current
        .as_mut()
        .filter(|u| u.id == id && u.attempt_id == attempt)
        .context("Update attempt changed before dispatch")?;
    ensure!(
        current.state == State::Preparing && current.operation_id.is_none(),
        "Update dispatch already recorded"
    );
    if current.cancel_requested {
        touch(current, State::Cancelled, "cancelled");
        save_store(home, &store)?;
        return Ok(false);
    }
    // Save the child relation before dispatch. An interrupted two-file publish
    // is reconciled as unknown; it cannot silently cause another native Start.
    current.operation_id = Some(operation.id.clone());
    touch(current, State::Waiting, "waiting");
    save_store(home, &store)?;
    publish()?;
    Ok(true)
}

pub(super) fn worker_update(home: &Path, operation_id: &str) -> Result<Option<(String, String)>> {
    let store = load_store(home)?;
    Ok(store
        .current
        .filter(|u| u.operation_id.as_deref() == Some(operation_id))
        .map(|u| (u.id, u.attempt_id)))
}

pub(super) fn active_waiting(home: &Path, transaction: &str) -> Result<bool> {
    let store = load_store(home)?;
    let Some(update) = store.current.filter(|u| u.state == State::Waiting) else {
        return Ok(false);
    };
    let Some(id) = update.operation_id else {
        return Ok(false);
    };
    let ledger = load(home)?;
    let Some(op) = get_operation(home, &ledger, &id)? else {
        return Ok(false);
    };
    if ledger.current_operation.as_deref() != Some(&id)
        || op.transaction_id != transaction
        || !op.attempt.claimed
        || terminal(&op)
        || !worker_active(home)?
    {
        return Ok(false);
    }
    let Some(generation) = op.attempt.worker_process else {
        return Ok(false);
    };
    amplifier_native_platform::process::matches(&generation)
}

pub(super) fn worker_control(
    home: &Path,
    id: &str,
    attempt: &str,
    event: native_update::WaitingUpdateEvent<'_>,
) -> Result<native_update::WaitingUpdateDecision> {
    use native_update::{WaitingUpdateDecision as Decision, WaitingUpdateEvent as Event};
    let mut decision = Decision::Continue;
    change(home, id, attempt, |u| {
        if u.cancel_requested {
            decision = Decision::Cancel;
            return Ok(());
        }
        match event {
            Event::BeforeStop => touch(u, State::Installing, "installing"),
            Event::Waiting(readiness) => {
                let ordinary = readiness["reason"] == "busy"
                    && readiness["closed"] != true
                    && readiness["waiting_for_work"] == true
                    && readiness["busy"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty());
                if ordinary {
                    touch(u, State::Waiting, "waiting");
                } else {
                    decision = Decision::NeedsAttention;
                    touch(u, State::NeedsAttention, "needs_attention");
                }
            }
        }
        Ok(())
    })?;
    Ok(decision)
}

pub(super) fn worker_finished(
    home: &Path,
    operation_id: &str,
    result: &Result<Value>,
) -> Result<()> {
    let Some((id, attempt)) = worker_update(home, operation_id)? else {
        return Ok(());
    };
    let ledger = load(home)?;
    let operation =
        get_operation(home, &ledger, operation_id)?.context("Missing native operation")?;
    change(home, &id, &attempt, |u| {
        match result {
            Ok(value) if value["cancelled"] == true => touch(u, State::Cancelled, "cancelled"),
            Ok(value) if value["needs_attention"] == true => {
                touch(u, State::NeedsAttention, "needs_attention")
            }
            _ if operation.outcome.as_deref() == Some("applied") => {
                touch(u, State::Completed, "completed")
            }
            _ if operation.outcome.as_deref() == Some("rolled_back") => {
                touch(u, State::Failed, "rolled_back")
            }
            _ if verified_terminal(&operation) => touch(u, State::Failed, "failed"),
            _ => touch(u, State::Unknown, "unknown"),
        }
        Ok(())
    })
}
