//! Thin browser controls for the existing native installation lifecycle.
//! One private atomic ledger binds requests to operations before any worker is
//! launched. HTTP retries reconcile it; they never allocate another transaction.
use crate::{
    auth,
    config::Config,
    error::{ApiError, ApiResult},
    native_update,
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const DIRECTORY: &str = "native-browser-update";
const LEDGER: &str = "control.json";
const MAX_LEDGER_BYTES: usize = 4 * 1024 * 1024;
const MAX_HISTORY_BYTES: usize = MAX_LEDGER_BYTES + 1024;
pub(crate) const WORKER_LOCK: &str = "native-browser-worker.lock";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryAction {
    Resume,
    Rollback,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckInput {
    pub package_path: PathBuf,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyInput {
    pub request_id: String,
    pub check_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoverInput {
    pub request_id: String,
    pub operation_id: String,
    pub action: RecoveryAction,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusQuery {
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RequestPayload {
    Apply {
        check_id: String,
    },
    Recover {
        operation_id: String,
        action: RecoveryAction,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestRecord {
    payload: RequestPayload,
    operation_id: String,
    attempt_id: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckedPackage {
    id: String,
    plan: Value,
    helper: Value,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OperationKind {
    Apply,
    Rollback,
    Existing,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OperationState {
    Accepted,
    Running,
    Completed,
    Deferred,
    Failed,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    id: String,
    action: Option<RecoveryAction>,
    claimed: bool,
    handoff_nonce: String,
    launch: Option<crate::native_update_launch::PreparedLaunch>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    id: String,
    request_id: Option<String>,
    transaction_id: String,
    kind: OperationKind,
    previous_transaction: Option<String>,
    plan: Value,
    helper: Value,
    attempt: Attempt,
    state: OperationState,
    outcome: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema: u32,
    installation_id: String,
    current_check: Option<String>,
    current_operation: Option<String>,
    checks: BTreeMap<String, CheckedPackage>,
    operations: BTreeMap<String, Operation>,
    requests: BTreeMap<String, RequestRecord>,
}
impl Ledger {
    fn empty(id: String) -> Self {
        Self {
            schema: 1,
            installation_id: id,
            current_check: None,
            current_operation: None,
            checks: BTreeMap::new(),
            operations: BTreeMap::new(),
            requests: BTreeMap::new(),
        }
    }
}

fn uuid(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.hyphenated().to_string() == value,
        "Use a canonical request UUID"
    );
    Ok(())
}
fn conflict(message: &'static str) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}
fn standalone(home: &Path) -> ApiResult<()> {
    if home.join("native-update-owner.json").exists() {
        return Err(conflict(
            "Use this installation's existing enrolled updater controls",
        ));
    }
    Ok(())
}
pub(crate) fn ordinary(path: &Path, directory: bool) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink()
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
        "Update control storage must be ordinary private data"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Update control storage cannot be a reparse point"
        );
    }
    Ok(metadata)
}
fn load(home: &Path) -> Result<Ledger> {
    let id = Config::load(home)?.id;
    let directory = home.join(DIRECTORY);
    if !directory.exists() {
        return Ok(Ledger::empty(id));
    }
    ordinary(&directory, true)?;
    let path = directory.join(LEDGER);
    if !path.exists() {
        return Ok(Ledger::empty(id));
    }
    ensure!(
        ordinary(&path, false)?.len() <= MAX_LEDGER_BYTES as u64,
        "Update control ledger exceeds its bound"
    );
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() <= MAX_LEDGER_BYTES,
        "Update control ledger exceeds its bound"
    );
    let value: Ledger = serde_json::from_slice(&bytes)?;
    ensure!(
        value.schema == 1 && value.installation_id == id,
        "Update control identity changed"
    );
    Ok(value)
}
fn history_path(home: &Path, kind: &str, id: &str) -> Result<PathBuf> {
    uuid(id)?;
    ensure!(
        matches!(kind, "operations" | "requests" | "checks" | "transactions"),
        "Invalid retained record kind"
    );
    Ok(home
        .join(DIRECTORY)
        .join("history")
        .join(kind)
        .join(format!("{id}.json")))
}
fn history_read<T: serde::de::DeserializeOwned>(
    home: &Path,
    installation: &str,
    kind: &str,
    id: &str,
) -> Result<Option<T>> {
    let path = history_path(home, kind, id)?;
    for directory in [
        home.join(DIRECTORY),
        home.join(DIRECTORY).join("history"),
        path.parent().unwrap().to_owned(),
    ] {
        if !directory.exists() && !directory.is_symlink() {
            return Ok(None);
        }
        ordinary(&directory, true)?;
    }
    if !path.exists() && !path.is_symlink() {
        return Ok(None);
    }
    ensure!(
        ordinary(&path, false)?.len() <= MAX_HISTORY_BYTES as u64,
        "Retained control record exceeds its bound"
    );
    let bytes = fs::read(&path)?;
    ensure!(
        bytes.len() <= MAX_HISTORY_BYTES,
        "Retained control record exceeds its bound"
    );
    let envelope: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        envelope["schema"] == 1
            && envelope["installation_id"] == installation
            && envelope["id"] == id,
        "Retained control record identity changed"
    );
    Ok(Some(serde_json::from_value(envelope["value"].clone())?))
}
fn history_write<T: Serialize>(
    home: &Path,
    installation: &str,
    kind: &str,
    id: &str,
    value: &T,
) -> Result<()> {
    let path = history_path(home, kind, id)?;
    let bytes = serde_json::to_vec(
        &json!({"schema":1,"installation_id":installation,"id":id,"value":value}),
    )?;
    ensure!(
        bytes.len() <= MAX_HISTORY_BYTES,
        "Retained control record exceeds its bound"
    );
    for directory in [
        home.join(DIRECTORY),
        home.join(DIRECTORY).join("history"),
        path.parent().unwrap().to_owned(),
    ] {
        amplifier_native_platform::private_dir(&directory)?;
    }
    if path.exists() || path.is_symlink() {
        ensure!(
            ordinary(&path, false)?.len() == bytes.len() as u64 && fs::read(&path)? == bytes,
            "Immutable retained control record differs; no history was overwritten"
        );
        // A previous publication may have been interrupted before the active
        // ledger was committed. Reconfirm exact bytes and durability first.
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?
            .sync_all()?;
        #[cfg(unix)]
        fs::File::open(path.parent().unwrap())?.sync_all()?;
        return Ok(());
    }
    amplifier_native_platform::private_create(&path, &bytes)
}
fn verified_terminal(operation: &Operation) -> bool {
    terminal(operation)
        && operation
            .outcome
            .as_deref()
            .is_some_and(|outcome| matches!(outcome, "unchanged" | "applied" | "rolled_back"))
}
fn get_request(home: &Path, ledger: &Ledger, id: &str) -> Result<Option<RequestRecord>> {
    if let Some(value) = ledger.requests.get(id) {
        return Ok(Some(value.clone()));
    }
    history_read(home, &ledger.installation_id, "requests", id)
}
fn get_operation(home: &Path, ledger: &Ledger, id: &str) -> Result<Option<Operation>> {
    if let Some(value) = ledger.operations.get(id) {
        return Ok(Some(value.clone()));
    }
    archived_operation(home, &ledger.installation_id, id)
}
fn archived_operation(home: &Path, installation: &str, id: &str) -> Result<Option<Operation>> {
    let value: Option<Operation> = history_read(home, installation, "operations", id)?;
    if let Some(value) = &value {
        ensure!(
            value.id == id && verified_terminal(value),
            "Archived operation is not a verified terminal record"
        );
    }
    Ok(value)
}
fn get_check(home: &Path, ledger: &Ledger, id: &str) -> Result<Option<CheckedPackage>> {
    if let Some(value) = ledger.checks.get(id) {
        return Ok(Some(value.clone()));
    }
    let value: Option<CheckedPackage> = history_read(home, &ledger.installation_id, "checks", id)?;
    if let Some(value) = &value {
        ensure!(value.id == id, "Archived check identity changed");
    }
    Ok(value)
}
fn compact(home: &Path, value: &Ledger) -> Result<Ledger> {
    let mut active = value.clone();
    let completed: Vec<String> = value
        .operations
        .values()
        .filter(|operation| {
            value.current_operation.as_deref() != Some(&operation.id)
                && verified_terminal(operation)
        })
        .map(|operation| operation.id.clone())
        .collect();
    for id in completed {
        let operation = &value.operations[&id];
        history_write(home, &value.installation_id, "operations", &id, operation)?;
        history_write(
            home,
            &value.installation_id,
            "transactions",
            &operation.transaction_id,
            &json!({"operation_id":id,"transaction_id":operation.transaction_id}),
        )?;
        for (request_id, record) in value
            .requests
            .iter()
            .filter(|(_, record)| record.operation_id == id)
        {
            history_write(home, &value.installation_id, "requests", request_id, record)?;
            active.requests.remove(request_id);
        }
        active.operations.remove(&id);
    }
    for (id, check) in &value.checks {
        if value.current_check.as_deref() != Some(id) {
            history_write(home, &value.installation_id, "checks", id, check)?;
            active.checks.remove(id);
        }
    }
    Ok(active)
}
fn save(home: &Path, value: &Ledger) -> Result<()> {
    // Immutable history is durable before publishing the smaller active ledger.
    // A crash between these writes leaves identical duplicate records, never a
    // missing old request or permission to allocate its transaction again.
    let active = compact(home, value)?;
    let bytes = serde_json::to_vec(&active)?;
    ensure!(
        bytes.len() <= MAX_LEDGER_BYTES,
        "Unresolved update control state exceeds its bound"
    );
    amplifier_native_platform::private_dir(&home.join(DIRECTORY))?;
    amplifier_native_platform::private_write_data(&home.join(DIRECTORY).join(LEDGER), &bytes)
}

/// The platform owner implements only this fixed transport. It receives no
/// browser-provided command, environment or executable path. The executable is
/// derived from a verified installation-owned immutable package.
#[derive(Clone, Debug)]
pub(crate) struct FixedWorker {
    pub executable: PathBuf,
    pub home: PathBuf,
    pub operation_id: String,
    pub attempt_id: String,
}
pub(crate) trait WorkerLauncher: Send + Sync {
    fn unavailable_reason(&self, home: &Path) -> Option<&'static str>;
    fn prepare(
        &self,
        _: &FixedWorker,
    ) -> Result<Option<crate::native_update_launch::PreparedLaunch>> {
        Ok(None)
    }
    fn launch(
        &self,
        worker: &FixedWorker,
        prepared: Option<&crate::native_update_launch::PreparedLaunch>,
    ) -> Result<()>;
}
struct NativeLauncher;
impl WorkerLauncher for NativeLauncher {
    fn unavailable_reason(&self, _: &Path) -> Option<&'static str> {
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            None
        } else {
            Some("A supported native service manager is required for this update.")
        }
    }
    fn prepare(
        &self,
        worker: &FixedWorker,
    ) -> Result<Option<crate::native_update_launch::PreparedLaunch>> {
        Ok(Some(crate::native_update_launch::prepare_worker(
            &worker.home,
            &Config::load(&worker.home)?.id,
            &worker.operation_id,
            &worker.attempt_id,
            &worker.executable,
        )?))
    }
    fn launch(
        &self,
        worker: &FixedWorker,
        prepared: Option<&crate::native_update_launch::PreparedLaunch>,
    ) -> Result<()> {
        ensure!(
            prepared == self.prepare(worker)?.as_ref(),
            "Prepared worker launch changed"
        );
        match crate::native_update_launch::launch(prepared.context("Missing fixed worker launch")?)?
        {
            crate::native_update_launch::LaunchReceipt::Accepted => Ok(()),
            _ => anyhow::bail!("Fixed worker dispatch was not acknowledged"),
        }
    }
}
#[cfg(test)]
struct UnavailableLauncher;
#[cfg(test)]
impl WorkerLauncher for UnavailableLauncher {
    fn unavailable_reason(&self, _: &Path) -> Option<&'static str> {
        Some("Native launch is unavailable for this fixture.")
    }
    fn launch(
        &self,
        _: &FixedWorker,
        _: Option<&crate::native_update_launch::PreparedLaunch>,
    ) -> Result<()> {
        anyhow::bail!("Native launch is unavailable")
    }
}
pub(crate) fn launcher() -> &'static dyn WorkerLauncher {
    &NativeLauncher
}

pub(crate) fn worker_active(home: &Path) -> Result<bool> {
    if !home.join(WORKER_LOCK).exists() {
        return Ok(false);
    }
    ordinary(&home.join(WORKER_LOCK), false)?;
    match native_update::lock(home, WORKER_LOCK) {
        Ok(guard) => {
            drop(guard);
            Ok(false)
        }
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            }) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}
fn terminal(operation: &Operation) -> bool {
    matches!(
        operation.state,
        OperationState::Completed | OperationState::Deferred | OperationState::Failed
    )
}
fn settle(operation: &mut Operation, journal: Option<&native_update::RecoverySnapshot>) {
    if operation.state == OperationState::Failed {
        return;
    }
    if let Some(journal) =
        journal.filter(|j| j.transaction == operation.transaction_id && j.phase == "restored")
    {
        operation.outcome = journal
            .outcome
            .clone()
            .or_else(|| Some(journal.direction.clone()));
        operation.state = if journal.rollback_aborted {
            OperationState::Failed
        } else if journal.direction == "unchanged" {
            OperationState::Deferred
        } else {
            OperationState::Completed
        };
    }
}
fn current_operation(ledger: &Ledger) -> Result<Option<&Operation>> {
    ledger
        .current_operation
        .as_ref()
        .map(|id| {
            ledger
                .operations
                .get(id)
                .context("Missing retained operation")
        })
        .transpose()
}
fn settle_current(home: &Path, ledger: &mut Ledger) -> Result<()> {
    if let Some(id) = &ledger.current_operation {
        let journal = native_update::recovery_snapshot(home)?;
        let operation = ledger
            .operations
            .get_mut(id)
            .context("Missing retained operation")?;
        settle(operation, journal.as_ref());
        ensure!(
            terminal(operation),
            "Another browser update remains unresolved"
        );
    }
    Ok(())
}
fn summary(
    operation: &Operation,
    journal: Option<&native_update::RecoverySnapshot>,
    active: bool,
) -> Value {
    let mut value = operation.clone();
    settle(&mut value, journal);
    let phase = if terminal(&value) {
        match value.state {
            OperationState::Deferred => "deferred",
            OperationState::Failed => "failed",
            _ => "completed",
        }
    } else if let Some(journal) = journal.filter(|j| j.transaction == value.transaction_id) {
        journal.phase.as_str()
    } else if active {
        "running"
    } else {
        match value.state {
            OperationState::Accepted => "accepted",
            _ => "unknown",
        }
    };
    let message = match phase {
        "completed" => "The native operation completed.",
        "deferred" => {
            "The update was deferred while existing work was active. Nothing was discarded."
        }
        "failed" => {
            "The requested change failed; the existing lifecycle retained its verified outcome."
        }
        "accepted" => "The request is retained. Waiting for its fixed native worker.",
        _ if active => "The native worker owns this operation.",
        _ => "This operation needs explicit recovery. Its transaction and evidence are retained.",
    };
    json!({"id":value.id,"request_id":value.request_id,"transaction_id":value.transaction_id,
        "phase":phase,"outcome":value.outcome,"message":message})
}

pub(crate) fn status(
    home: &Path,
    mode: &str,
    request_id: Option<&str>,
    launch: &dyn WorkerLauncher,
) -> ApiResult<Value> {
    if let Some(id) = request_id {
        uuid(id)?;
    }
    let ledger = load(home)?;
    let installed = native_update::status(home)?;
    let enrolled = home.join("native-update-owner.json").exists();
    let journal = native_update::recovery_snapshot(home)?;
    let active = worker_active(home)?;
    let record = request_id
        .map(|id| get_request(home, &ledger, id))
        .transpose()?
        .flatten();
    let selected_value = if let Some(request) = &record {
        Some(
            get_operation(home, &ledger, &request.operation_id)?
                .context("Missing requested operation")?,
        )
    } else {
        current_operation(&ledger)?.cloned()
    };
    let selected = selected_value.as_ref();
    let foreign_pending = record.is_none()
        && journal.as_ref().is_some_and(|j| {
            j.phase != "restored" && selected.is_none_or(|op| op.transaction_id != j.transaction)
        });
    let operation = if let Some(operation) = selected.filter(|_| !foreign_pending) {
        summary(operation, journal.as_ref(), active)
    } else if let Some(journal) = &journal {
        json!({"id":journal.transaction,"request_id":null,"transaction_id":journal.transaction,
            "phase":if journal.phase == "restored" { "completed" } else { &journal.phase },
            "outcome":journal.outcome,"message":"Retained native installation transaction."})
    } else {
        Value::Null
    };
    let checked = ledger
        .current_check
        .as_ref()
        .map(|id| ledger.checks.get(id).context("Missing checked package"))
        .transpose()?;
    let check = checked.map(|checked| json!({"id":checked.id,"available":checked.plan["available"],
        "version":checked.plan["version"],"summary":"Complete service, private runtime, bridge and support bundle verified. Configuration and conversation data are retained."}));
    // Request-specific reads can show an older receipt. Global admission and
    // action eligibility always come from the authoritative current operation.
    let current = current_operation(&ledger)?;
    let mut current_settled = current.cloned();
    if let Some(value) = &mut current_settled {
        settle(value, journal.as_ref());
    }
    let current_pending = current_settled
        .as_ref()
        .is_some_and(|value| !terminal(value));
    let pending = current_pending || journal.as_ref().is_some_and(|j| j.phase != "restored");
    let reason = if enrolled {
        Some("Use this installation's existing enrolled updater controls.")
    } else {
        launch.unavailable_reason(home)
    };
    let can_launch = reason.is_none() && !active;
    let same_current = record.is_none() || selected.map(|op| &op.id) == current.map(|op| &op.id);
    let same_native = current
        .map(|value| {
            journal
                .as_ref()
                .is_some_and(|j| j.transaction == value.transaction_id)
        })
        .unwrap_or(true);
    let before_intent = current_pending && journal.as_ref().is_none_or(|j| j.phase == "restored");
    let resumable = pending
        && same_current
        && (same_native || before_intent || foreign_pending)
        && journal.as_ref().is_none_or(|j| j.standalone);
    let rollback = same_current
        && (same_native || foreign_pending)
        && journal.as_ref().is_some_and(|j| {
            j.standalone
                && (!j.restore_started && j.phase != "restored"
                    || j.phase == "restored" && j.direction == "applied")
        });
    let request = request_id.map(|id| {
        json!({"id":id,"state":if record.is_some() { "recorded" } else { "not_recorded" },
        "operation_id":record.as_ref().map(|r| &r.operation_id)})
    });
    Ok(
        json!({"supported":true,"owner":if enrolled { "enrolled" } else { "standalone" },"mode":mode,
        "current":{"version":installed["version"]},"check":check,"operation":operation,"request":request,
        "actions":{"check":!enrolled && mode == "normal" && !pending && !active,
            "apply":can_launch && mode == "normal" && !pending && checked.is_some_and(|c| c.plan["available"] == true),
            "recover":can_launch && resumable,"rollback":can_launch && rollback},"reason":reason}),
    )
}

pub(crate) fn check(
    home: &Path,
    input: CheckInput,
    launch: &dyn WorkerLauncher,
) -> ApiResult<Value> {
    standalone(home)?;
    if !input.package_path.is_absolute() || input.package_path.as_os_str().len() > 4096 {
        return Err(ApiError::bad(
            "Choose the full path to an extracted complete native package",
        ));
    }
    crate::native_browser_cleanup::before_check(home)?;
    let package = crate::desktop::checked_package_folder(&input.package_path)?;
    let plan = native_update::plan(home, &package)?;
    if !plan["version"]
        .as_str()
        .is_some_and(|version| !version.is_empty() && version.len() <= 128)
    {
        return Err(ApiError::bad(
            "The complete package has an unsupported version label",
        ));
    }
    {
        let _guard = native_update::lock(home, "native-update.lock")?;
        standalone(home)?;
        if worker_active(home)? {
            return Err(conflict("A native update worker is still running"));
        }
        let mut ledger = load(home)?;
        settle_current(home, &mut ledger)?;
        native_update::validate_frozen_plan(home, &plan)?;
        let id = uuid::Uuid::new_v4().to_string();
        ledger.checks.insert(
            id.clone(),
            CheckedPackage {
                id: id.clone(),
                helper: plan["previous"].clone(),
                plan,
            },
        );
        ledger.current_check = Some(id);
        save(home, &ledger)?;
    }
    status(home, "normal", None, launch)
}

fn duplicate(home: &Path, ledger: &Ledger, id: &str, payload: &RequestPayload) -> ApiResult<bool> {
    if let Some(previous) = get_request(home, ledger, id)? {
        if &previous.payload != payload {
            return Err(conflict(
                "This request ID already names a different immutable request",
            ));
        }
        return Ok(true);
    }
    Ok(false)
}
fn launch_spec(home: &Path, operation: &Operation) -> Result<FixedWorker> {
    let executable = native_update::verify_helper_package(&operation.helper)?;
    ensure!(
        dunce::canonicalize(&executable)?
            != dunce::canonicalize(amplifier_native_platform::installed_binary(
                &crate::native_bundle::APPLICATION,
                home
            ))?,
        "The update worker must be outside the replaceable service executable"
    );
    Ok(FixedWorker {
        executable,
        home: dunce::canonicalize(home)?,
        operation_id: operation.id.clone(),
        attempt_id: operation.attempt.id.clone(),
    })
}
fn dispatch(home: &Path, operation: &Operation, launch: &dyn WorkerLauncher) -> Result<()> {
    let spec = launch_spec(home, operation)?;
    // All request/native locks are dropped before the OS runner can execute.
    // A failed launch acknowledgement is uncertain, not permission to retry.
    if launch
        .launch(&spec, operation.attempt.launch.as_ref())
        .is_err()
    {
        let _guard = native_update::lock(home, "native-update.lock")?;
        let mut ledger = load(home)?;
        if let Some(current) = ledger.operations.get_mut(&operation.id)
            && current.attempt.id == operation.attempt.id
            && !terminal(current)
        {
            current.state = OperationState::Unknown;
            save(home, &ledger)?;
        }
    }
    Ok(())
}
pub(crate) fn apply(
    home: &Path,
    input: ApplyInput,
    launch: &dyn WorkerLauncher,
) -> ApiResult<Value> {
    uuid(&input.request_id)?;
    uuid(&input.check_id)?;
    let payload = RequestPayload::Apply {
        check_id: input.check_id.clone(),
    };
    let operation = {
        let _guard = native_update::lock(home, "native-update.lock")?;
        let mut ledger = load(home)?;
        if duplicate(home, &ledger, &input.request_id, &payload)? {
            drop(_guard);
            return status(home, "normal", Some(&input.request_id), launch);
        }
        if get_operation(home, &ledger, &input.request_id)?.is_some() {
            return Err(conflict(
                "This identifier already names a retained operation",
            ));
        }
        standalone(home)?;
        if let Some(reason) = launch.unavailable_reason(home) {
            return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, reason.into()));
        }
        if worker_active(home)? {
            return Err(conflict("A native update worker is still running"));
        }
        settle_current(home, &mut ledger)?;
        if native_update::recovery_snapshot(home)?.is_some_and(|j| j.phase != "restored") {
            return Err(conflict(
                "A retained native transaction needs recovery first",
            ));
        }
        let checked =
            get_check(home, &ledger, &input.check_id)?.context("Checked package is unavailable")?;
        native_update::validate_frozen_plan(home, &checked.plan)?;
        if checked.plan["available"] != true {
            return Err(conflict("That complete bundle is already selected"));
        }
        let mut operation = Operation {
            id: input.request_id.clone(),
            request_id: Some(input.request_id.clone()),
            transaction_id: uuid::Uuid::new_v4().to_string(),
            kind: OperationKind::Apply,
            previous_transaction: None,
            plan: checked.plan.clone(),
            helper: checked.helper.clone(),
            attempt: Attempt {
                id: input.request_id.clone(),
                action: None,
                claimed: false,
                handoff_nonce: auth::random_token(),
                launch: None,
            },
            state: OperationState::Accepted,
            outcome: None,
        };
        let spec = launch_spec(home, &operation)?;
        operation.attempt.launch = launch.prepare(&spec)?;
        ledger.requests.insert(
            input.request_id.clone(),
            RequestRecord {
                payload,
                operation_id: operation.id.clone(),
                attempt_id: operation.attempt.id.clone(),
            },
        );
        ledger.current_operation = Some(operation.id.clone());
        ledger
            .operations
            .insert(operation.id.clone(), operation.clone());
        save(home, &ledger)?;
        operation
    };
    dispatch(home, &operation, launch)?;
    status(home, "normal", Some(&input.request_id), launch)
}

pub(crate) fn recover(
    home: &Path,
    mode: &str,
    input: RecoverInput,
    launch: &dyn WorkerLauncher,
) -> ApiResult<Value> {
    uuid(&input.request_id)?;
    uuid(&input.operation_id)?;
    let payload = RequestPayload::Recover {
        operation_id: input.operation_id.clone(),
        action: input.action,
    };
    let operation = {
        let _guard = native_update::lock(home, "native-update.lock")?;
        let mut ledger = load(home)?;
        if duplicate(home, &ledger, &input.request_id, &payload)? {
            drop(_guard);
            return status(home, mode, Some(&input.request_id), launch);
        }
        standalone(home)?;
        if let Some(reason) = launch.unavailable_reason(home) {
            return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, reason.into()));
        }
        let _worker = native_update::lock(home, WORKER_LOCK)
            .map_err(|_| conflict("The native worker has not released this operation"))?;
        let journal = native_update::recovery_snapshot(home)?;
        if let Some(current) = current_operation(&ledger)?
            && current.id != input.operation_id
        {
            let mut settled = current.clone();
            settle(&mut settled, journal.as_ref());
            if !terminal(&settled) {
                return Err(conflict("A different browser operation remains unresolved"));
            }
        }
        let mut operation =
            if let Some(existing) = get_operation(home, &ledger, &input.operation_id)? {
                existing
            } else {
                // Transaction UUIDs are foreign-adoption identifiers only when
                // no browser operation already owns that transaction.
                if ledger
                    .operations
                    .values()
                    .any(|operation| operation.transaction_id == input.operation_id)
                    || history_read::<Value>(
                        home,
                        &ledger.installation_id,
                        "transactions",
                        &input.operation_id,
                    )?
                    .is_some()
                {
                    return Err(conflict(
                        "Use the retained browser operation ID for this transaction",
                    ));
                }
                let journal = journal
                    .as_ref()
                    .filter(|j| j.transaction == input.operation_id && j.standalone)
                    .context("No matching standalone transaction can be recovered")?;
                Operation {
                    id: journal.transaction.clone(),
                    request_id: None,
                    transaction_id: journal.transaction.clone(),
                    kind: OperationKind::Existing,
                    previous_transaction: None,
                    plan: Value::Null,
                    helper: native_update::recovery_package(home)?,
                    attempt: Attempt {
                        id: input.request_id.clone(),
                        action: Some(input.action),
                        claimed: false,
                        handoff_nonce: auth::random_token(),
                        launch: None,
                    },
                    state: OperationState::Unknown,
                    outcome: None,
                }
            };
        let matching = journal
            .as_ref()
            .filter(|j| j.transaction == operation.transaction_id);
        if terminal(&operation)
            && !matching.is_some_and(|j| {
                j.phase == "restored"
                    && j.direction == "applied"
                    && input.action == RecoveryAction::Rollback
            })
        {
            return Err(conflict("This retained operation is already settled"));
        }
        if let Some(journal) = matching {
            if !journal.standalone {
                return Err(conflict(
                    "A different native transaction owns this installation",
                ));
            }
            settle(&mut operation, Some(journal));
            if journal.phase == "restored" {
                if input.action != RecoveryAction::Rollback || journal.direction != "applied" {
                    return Err(conflict("This operation is already settled"));
                }
                if get_operation(home, &ledger, &input.request_id)?.is_some() {
                    return Err(conflict(
                        "This identifier already names a retained operation",
                    ));
                }
                ledger
                    .operations
                    .insert(operation.id.clone(), operation.clone());
                operation.previous_transaction = Some(journal.transaction.clone());
                operation.transaction_id = uuid::Uuid::new_v4().to_string();
                operation.id = input.request_id.clone();
                operation.request_id = Some(input.request_id.clone());
                operation.kind = OperationKind::Rollback;
                operation.helper = native_update::verify_installed(home)?["package"].clone();
                operation.outcome = None;
            } else if journal.restore_started && input.action == RecoveryAction::Rollback {
                return Err(conflict(
                    "Restoration already began; only finishing restoration is allowed",
                ));
            }
        } else {
            if journal.as_ref().is_some_and(|j| j.phase != "restored") {
                return Err(conflict(
                    "A different native transaction owns this installation",
                ));
            }
            if input.action == RecoveryAction::Rollback || operation.kind == OperationKind::Existing
            {
                return Err(conflict(
                    "There is no retained native transaction to roll back",
                ));
            }
            // A crashed dispatch before native intent has no gate or switch.
            // Explicit recovery can begin its original checked transaction only
            // after revoking the late attempt and revalidating the baseline.
            if operation.kind == OperationKind::Apply {
                native_update::validate_frozen_plan(home, &operation.plan)?;
            } else if !journal.as_ref().is_some_and(|j| {
                Some(&j.transaction) == operation.previous_transaction.as_ref()
                    && j.phase == "restored"
                    && j.direction == "applied"
            }) {
                return Err(conflict("The retained rollback baseline changed"));
            }
        }
        operation.attempt = Attempt {
            id: input.request_id.clone(),
            action: Some(input.action),
            claimed: false,
            handoff_nonce: auth::random_token(),
            launch: None,
        };
        operation.state = OperationState::Accepted;
        let spec = launch_spec(home, &operation)?;
        operation.attempt.launch = launch.prepare(&spec)?;
        ledger.requests.insert(
            input.request_id.clone(),
            RequestRecord {
                payload,
                operation_id: operation.id.clone(),
                attempt_id: operation.attempt.id.clone(),
            },
        );
        ledger.current_operation = Some(operation.id.clone());
        ledger
            .operations
            .insert(operation.id.clone(), operation.clone());
        save(home, &ledger)?;
        operation
    };
    dispatch(home, &operation, launch)?;
    status(home, mode, Some(&input.request_id), launch)
}

pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/companion/update", get(http_status))
        .route("/api/v1/companion/update/check", post(http_check))
        .route("/api/v1/companion/update/apply", post(http_apply))
        .route("/api/v1/companion/update/recover", post(http_recover))
}
async fn http_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<StatusQuery>,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", false)?;
    Ok(Json(status(
        &state.home,
        "normal",
        query.request_id.as_deref(),
        launcher(),
    )?))
}
async fn http_check(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<CheckInput>,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", true)?;
    let home = state.home.clone();
    blocking_control(move || check(&home, input, launcher())).await
}
async fn http_apply(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<ApplyInput>,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", true)?;
    let home = state.home.clone();
    blocking_control(move || apply(&home, input, launcher())).await
}
async fn http_recover(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<RecoverInput>,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", true)?;
    let home = state.home.clone();
    blocking_control(move || recover(&home, "normal", input, launcher())).await
}

async fn blocking_control(
    action: impl FnOnce() -> ApiResult<Value> + Send + 'static,
) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The retained control request could not be observed".into(),
            )
        })?
        .map(Json)
}

pub(crate) fn assets<S: Clone + Send + Sync + 'static>() -> Router<S> {
    use axum::{http::header, response::Html};
    Router::new()
        .route(
            "/updates",
            get(|| async { Html(include_str!("../ui/native-updates.html")) }),
        )
        .route(
            "/native-updates.html",
            get(|| async { Html(include_str!("../ui/native-updates.html")) }),
        )
        .route(
            "/native-updates.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../ui/native-updates.mjs"),
                )
            }),
        )
        .route(
            "/native-updates.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/native-updates.css"),
                )
            }),
        )
}

pub(crate) fn authorize_release(
    home: &Path,
    transaction: &str,
    input: &crate::native_recovery::Release,
) -> ApiResult<()> {
    let _guard = native_update::lock(home, "native-update.lock")?;
    let ledger = load(home)?;
    let operation = current_operation(&ledger)?.ok_or_else(ApiError::forbidden)?;
    if operation.id != input.operation_id
        || operation.transaction_id != transaction
        || operation.attempt.id != input.attempt_id
        || !operation.attempt.claimed
        || !auth::constant_equal(&operation.attempt.handoff_nonce, &input.nonce)
        || !worker_active(home)?
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

fn claim(
    home: &Path,
    operation_id: &str,
    attempt_id: &str,
    executable: &Path,
) -> Result<Operation> {
    uuid(operation_id)?;
    uuid(attempt_id)?;
    ensure!(
        !home.join("native-update-owner.json").exists(),
        "This installation has an enrolled owner"
    );
    let _guard = native_update::lock(home, "native-update.lock")?;
    let mut ledger = load(home)?;
    ensure!(
        ledger.current_operation.as_deref() == Some(operation_id),
        "This operation is no longer current"
    );
    let operation = ledger
        .operations
        .get_mut(operation_id)
        .context("Missing retained worker operation")?;
    ensure!(
        operation.attempt.id == attempt_id && !operation.attempt.claimed && !terminal(operation),
        "This worker attempt is already claimed or revoked"
    );
    let spec = launch_spec(home, operation)?;
    ensure!(
        dunce::canonicalize(executable)? == dunce::canonicalize(spec.executable)?,
        "Only the exact verified immutable helper can claim this attempt"
    );
    operation.attempt.claimed = true;
    operation.state = OperationState::Running;
    let operation = operation.clone();
    save(home, &ledger)?;
    Ok(operation)
}

/// The fixed manager profile invokes this command once. A duplicate manager
/// start, delayed revoked worker or restarted process cannot reclaim its attempt.
/// No process is spawned here and no work is implicitly retried after a crash.
pub async fn worker(home: &Path, operation_id: &str, attempt_id: &str) -> Result<()> {
    let _execution = native_update::lock(home, WORKER_LOCK)?;
    let operation = claim(home, operation_id, attempt_id, &std::env::current_exe()?)?;
    let result = async {
        crate::native_recovery::yield_listener(
            home,
            &operation.transaction_id,
            crate::native_recovery::Release {
                operation_id: operation.id.clone(),
                attempt_id: operation.attempt.id.clone(),
                nonce: operation.attempt.handoff_nonce.clone(),
            },
        )
        .await?;
        let journal = native_update::recovery_snapshot(home)?;
        if let Some(journal) = journal
            .as_ref()
            .filter(|j| j.transaction == operation.transaction_id)
        {
            ensure!(
                operation.attempt.action.is_some(),
                "The original dispatch already has native intent; use explicit recovery"
            );
            ensure!(
                journal.standalone,
                "Another owner controls this transaction"
            );
            native_update::recover(
                home,
                &operation.transaction_id,
                operation.attempt.action == Some(RecoveryAction::Rollback),
            )
            .await
        } else {
            ensure!(
                journal.as_ref().is_none_or(|j| j.phase == "restored"),
                "A different native transaction owns this installation"
            );
            match operation.kind {
                OperationKind::Apply => {
                    native_update::run_frozen(home, &operation.transaction_id, &operation.plan)
                        .await
                }
                OperationKind::Rollback => {
                    native_update::rollback_with_transaction(
                        home,
                        &operation.transaction_id,
                        operation
                            .previous_transaction
                            .as_deref()
                            .context("Missing rollback baseline")?,
                    )
                    .await
                }
                OperationKind::Existing => {
                    anyhow::bail!("The retained native transaction is no longer available")
                }
            }
        }
    }
    .await;
    {
        let _guard = native_update::lock(home, "native-update.lock")?;
        let mut ledger = load(home)?;
        let current = ledger
            .operations
            .get_mut(operation_id)
            .context("Missing retained worker operation")?;
        ensure!(
            current.attempt.id == attempt_id,
            "Worker attempt changed while executing"
        );
        let journal = native_update::recovery_snapshot(home)?;
        settle(current, journal.as_ref());
        if result.is_err() {
            current.state = if terminal(current) {
                OperationState::Failed
            } else {
                OperationState::Unknown
            };
        } else if !terminal(current) {
            current.state = OperationState::Unknown;
        }
        save(home, &ledger)?;
    }
    result.map(|_| ())
}

/// Distinguish a managed browser restoration from legacy native participant
/// startup without imposing the browser execution lock on another owner.
pub(crate) fn owns_pending_transaction(home: &Path, transaction: &str) -> Result<bool> {
    let ledger = load(home)?;
    Ok(current_operation(&ledger)?
        .is_some_and(|op| op.transaction_id == transaction && op.attempt.claimed))
}

/// A cleanup snapshot contains only identities with a terminal verified native
/// outcome. Private plans, package paths and startup nonces remain in the ledger.
pub(crate) struct CleanupAuthority {
    pub(crate) home: PathBuf,
    pub(crate) installation_id: String,
    pub(crate) workers: std::collections::BTreeSet<(String, String)>,
    pub(crate) transactions: std::collections::BTreeSet<String>,
    pub current_settled: bool,
}
impl CleanupAuthority {
    pub fn worker(&self, operation: &str, attempt: &str) -> Result<bool> {
        if self
            .workers
            .contains(&(operation.to_owned(), attempt.to_owned()))
        {
            return Ok(true);
        }
        let Some(operation) = archived_operation(&self.home, &self.installation_id, operation)?
        else {
            return Ok(false);
        };
        let request: Option<RequestRecord> =
            history_read(&self.home, &self.installation_id, "requests", attempt)?;
        Ok(request.is_some_and(|request| {
            request.operation_id == operation.id && request.attempt_id == attempt
        }))
    }
    pub fn replacement(&self, transaction: &str) -> Result<bool> {
        if self.transactions.contains(transaction) {
            return Ok(true);
        }
        let Some(index): Option<Value> = history_read(
            &self.home,
            &self.installation_id,
            "transactions",
            transaction,
        )?
        else {
            return Ok(false);
        };
        ensure!(
            index["transaction_id"] == transaction,
            "Archived transaction identity changed"
        );
        let id = index["operation_id"]
            .as_str()
            .context("Missing archived operation identity")?;
        let operation = archived_operation(&self.home, &self.installation_id, id)?
            .context("Missing archived terminal operation")?;
        ensure!(
            operation.transaction_id == transaction,
            "Archived operation transaction changed"
        );
        Ok(true)
    }
}
pub(crate) fn cleanup_authority(home: &Path) -> Result<CleanupAuthority> {
    let ledger = load(home)?;
    let journal = native_update::recovery_snapshot(home)?;
    let mut authority = CleanupAuthority {
        home: home.to_owned(),
        installation_id: ledger.installation_id.clone(),
        workers: Default::default(),
        transactions: Default::default(),
        current_settled: ledger.current_operation.is_none(),
    };
    for original in ledger.operations.values() {
        let mut operation = original.clone();
        settle(&mut operation, journal.as_ref());
        let verified = verified_terminal(&operation);
        if ledger.current_operation.as_deref() == Some(&operation.id) {
            authority.current_settled = verified;
        }
        if verified {
            authority
                .transactions
                .insert(operation.transaction_id.clone());
            for request in ledger
                .requests
                .values()
                .filter(|request| request.operation_id == operation.id)
            {
                authority
                    .workers
                    .insert((operation.id.clone(), request.attempt_id.clone()));
            }
        }
    }
    Ok(authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config, native_bundle};
    use sha2::{Digest, Sha256};
    use std::{
        io::Read,
        sync::atomic::{AtomicUsize, Ordering},
    };
    fn digest(path: &Path) -> String {
        let mut input = fs::File::open(path).unwrap();
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = input.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        format!("{:x}", hash.finalize())
    }
    fn read_json(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
    fn write_json(path: &Path, value: &Value) {
        config::private_write(path, &serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }
    fn native_header() -> Vec<u8> {
        // Same bounded architecture/header fixture as native_bundle's own tests.
        // It exercises integrity/selection, never executable startup qualification.
        let mut bytes = vec![0u8; 128];
        let arm = cfg!(target_arch = "aarch64");
        if cfg!(target_os = "linux") {
            bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
            bytes[16] = 2;
            bytes[18..20].copy_from_slice(&(if arm { 183u16 } else { 62u16 }).to_le_bytes());
            bytes[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
            bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
            bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
            bytes[56] = 1;
            bytes[64] = 1;
            bytes[68] = 5;
            bytes[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
            bytes[96..104].copy_from_slice(&128u64.to_le_bytes());
            bytes[104..112].copy_from_slice(&128u64.to_le_bytes());
        } else if cfg!(target_os = "macos") {
            bytes[..4].copy_from_slice(b"\xcf\xfa\xed\xfe");
            bytes[4..8]
                .copy_from_slice(&(if arm { 0x0100000cu32 } else { 0x01000007u32 }).to_le_bytes());
            bytes[12] = 2;
        } else if cfg!(windows) {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60] = 64;
            bytes[64..68].copy_from_slice(b"PE\0\0");
            bytes[68..70].copy_from_slice(&(if arm { 0xaa64u16 } else { 0x8664u16 }).to_le_bytes());
            bytes[86] = 2;
            bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        }
        bytes
    }
    fn package(root: &Path, version: &str) {
        let service = if cfg!(windows) {
            "agent-conversations.exe"
        } else {
            "agent-conversations"
        };
        let node = if cfg!(windows) {
            "runtime/node.exe"
        } else {
            "runtime/node"
        };
        let paths = [service, node, "bridge/live.mjs", "bridge/companion-mcp.mjs"];
        let mut files = Vec::new();
        for relative in paths {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let executable = relative == service || relative == node;
            if executable {
                let mut bytes = native_header();
                bytes.extend_from_slice(format!("\nfixture-{version}-{relative}\n").as_bytes());
                fs::write(&path, bytes).unwrap();
            } else {
                fs::write(&path, format!("// fixture {version} {relative}\n")).unwrap();
            }
            files.push(json!({
                "path":relative,"sha256":digest(&path),
                "bytes":fs::metadata(&path).unwrap().len(),"executable":executable
            }));
        }
        write_json(
            &root.join("manifest.json"),
            &json!({"schema_version":1,"component":"conversations","version":version,
            "source_revision":"a".repeat(40),"target":native_bundle::target().unwrap(),
            "entrypoints":{"service":service,"node":node,"bridge":paths[2],"mcp":paths[3]},
            "files":files}),
        );
    }

    struct Fixture {
        root: tempfile::TempDir,
        home: PathBuf,
        candidate: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("private-home");
            fs::create_dir(&home).unwrap();
            Config::local(&home, "Fixture", 54127)
                .unwrap()
                .save(&home)
                .unwrap();
            drop(AppState::open(home.clone()).unwrap());
            let original = root.path().join("original");
            package(&original, "fixture-a");
            native_update::bootstrap(&home, &original).unwrap();
            let candidate = root.path().join("candidate");
            package(&candidate, "fixture-b");
            Self {
                root,
                home,
                candidate,
            }
        }
        fn check(&self, launch: &dyn WorkerLauncher) -> String {
            check(
                &self.home,
                CheckInput {
                    package_path: self.candidate.clone(),
                },
                launch,
            )
            .unwrap()["check"]["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    }
    #[derive(Default)]
    struct LostReply {
        calls: AtomicUsize,
    }
    impl WorkerLauncher for LostReply {
        fn unavailable_reason(&self, _: &Path) -> Option<&'static str> {
            None
        }
        fn launch(
            &self,
            worker: &FixedWorker,
            _: Option<&crate::native_update_launch::PreparedLaunch>,
        ) -> Result<()> {
            assert!(worker.home.is_absolute());
            uuid(&worker.operation_id)?;
            uuid(&worker.attempt_id)?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("simulated lost manager reply")
        }
    }
    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn journal(home: &Path, tx: &str, phase: &str) {
        let installed = native_update::verify_installed(home).unwrap();
        write_json(
            &home.join("native-update.json"),
            &json!({"schema":1,"installation_id":Config::load(home).unwrap().id,
            "transaction_id":tx,"phase":phase,"direction":"applied","restore_started":phase=="restored",
            "restore_outcome":if phase=="restored" {Some("applied")} else {None}, "context":{"owner":"standalone"},"previous":installed["package"]}),
        );
    }

    #[tokio::test]
    async fn unobserved_blocking_completion_is_uncertain_not_rejected() {
        let result =
            blocking_control(|| -> ApiResult<Value> { panic!("fixture lost task completion") })
                .await;
        assert_eq!(result.unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn blocking_manager_dispatch_does_not_block_authenticated_status_requests() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        struct BarrierLaunch {
            entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl WorkerLauncher for BarrierLaunch {
            fn unavailable_reason(&self, _: &Path) -> Option<&'static str> {
                None
            }
            fn launch(
                &self,
                _: &FixedWorker,
                _: Option<&crate::native_update_launch::PreparedLaunch>,
            ) -> Result<()> {
                self.entered
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(3))?;
                Ok(())
            }
        }
        let fixture = Fixture::new();
        let check = fixture.check(&LostReply::default());
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let launch = BarrierLaunch {
            entered: std::sync::Mutex::new(Some(entered)),
            resume: std::sync::Mutex::new(resume),
        };
        let home = fixture.home.clone();
        let mutation = tokio::spawn(blocking_control(move || {
            apply(
                &home,
                ApplyInput {
                    request_id: id(),
                    check_id: check,
                },
                &launch,
            )
        }));
        waiting.await.unwrap();
        let state = AppState::open(fixture.home.clone()).unwrap();
        let token = fs::read_to_string(fixture.home.join("secrets/control-token")).unwrap();
        let request = Request::builder()
            .uri("/api/v1/companion/update")
            .header("host", "127.0.0.1:54127")
            .header("authorization", format!("Bearer {}", token.trim()))
            .body(Body::empty())
            .unwrap();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            router().with_state(state).oneshot(request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        release.send(()).unwrap();
        assert!(mutation.await.unwrap().is_ok());
    }
    #[test]
    fn lost_ack_duplicate_and_revoked_worker_keep_one_transaction() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let check_id = fixture.check(&launch);
        let request_id = id();
        let input = ApplyInput {
            request_id: request_id.clone(),
            check_id: check_id.clone(),
        };
        let first = apply(&fixture.home, input.clone(), &launch).unwrap();
        let tx = first["operation"]["transaction_id"].clone();
        assert_eq!(first["request"]["state"], "recorded");
        assert_eq!(first["actions"]["recover"], true);
        assert_eq!(
            apply(&fixture.home, input, &launch).unwrap()["operation"]["transaction_id"],
            tx
        );
        assert_eq!(launch.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            apply(
                &fixture.home,
                ApplyInput {
                    request_id: request_id.clone(),
                    check_id: id()
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let next = id();
        let resumed = recover(
            &fixture.home,
            "normal",
            RecoverInput {
                request_id: next.clone(),
                operation_id: request_id.clone(),
                action: RecoveryAction::Resume,
            },
            &launch,
        )
        .unwrap();
        assert_eq!(resumed["operation"]["transaction_id"], tx);
        assert_eq!(launch.calls.load(Ordering::SeqCst), 2);
        let ledger = load(&fixture.home).unwrap();
        let helper = launch_spec(&fixture.home, &ledger.operations[&request_id])
            .unwrap()
            .executable;
        let _execution = native_update::lock(&fixture.home, WORKER_LOCK).unwrap();
        assert!(claim(&fixture.home, &request_id, &request_id, &helper).is_err());
        assert!(claim(&fixture.home, &request_id, &next, &helper).is_ok());
        assert!(claim(&fixture.home, &request_id, &next, &helper).is_err());
        let serialized = resumed.to_string();
        assert!(!serialized.contains(fixture.root.path().to_str().unwrap()));
        assert!(!serialized.contains(&ledger.operations[&request_id].attempt.handoff_nonce));
        assert!(!serialized.contains("source_revision"));
    }
    #[test]
    fn older_request_cannot_hide_new_pending_dispatch_or_enable_mutations() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let old = id();
        apply(
            &fixture.home,
            ApplyInput {
                request_id: old.clone(),
                check_id: checked.clone(),
            },
            &launch,
        )
        .unwrap();
        let mut ledger = load(&fixture.home).unwrap();
        ledger.operations.get_mut(&old).unwrap().state = OperationState::Completed;
        save(&fixture.home, &ledger).unwrap();
        let next = id();
        apply(
            &fixture.home,
            ApplyInput {
                request_id: next.clone(),
                check_id: checked,
            },
            &launch,
        )
        .unwrap();
        let old_receipt = status(&fixture.home, "normal", Some(&old), &launch).unwrap();
        assert_eq!(old_receipt["operation"]["id"], old);
        for action in ["check", "apply", "recover", "rollback"] {
            assert_eq!(old_receipt["actions"][action], false);
        }
        assert_eq!(
            status(&fixture.home, "normal", Some(&next), &launch).unwrap()["actions"]["recover"],
            true
        );
        assert!(
            recover(
                &fixture.home,
                "normal",
                RecoverInput {
                    request_id: id(),
                    operation_id: old,
                    action: RecoveryAction::Resume
                },
                &launch
            )
            .is_err()
        );
    }
    #[test]
    fn prior_terminal_journal_does_not_strand_pre_intent_dispatch() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        journal(&fixture.home, &id(), "restored");
        let request = id();
        let accepted = apply(
            &fixture.home,
            ApplyInput {
                request_id: request.clone(),
                check_id: checked,
            },
            &launch,
        )
        .unwrap();
        assert_eq!(accepted["actions"]["recover"], true);
        let resumed = recover(
            &fixture.home,
            "normal",
            RecoverInput {
                request_id: id(),
                operation_id: request,
                action: RecoveryAction::Resume,
            },
            &launch,
        )
        .unwrap();
        assert_eq!(
            resumed["operation"]["transaction_id"],
            accepted["operation"]["transaction_id"]
        );
    }
    #[test]
    fn completed_rollback_allocates_fresh_transaction_and_keeps_old_receipt() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let old = id();
        let accepted = apply(
            &fixture.home,
            ApplyInput {
                request_id: old.clone(),
                check_id: checked,
            },
            &launch,
        )
        .unwrap();
        let tx = accepted["operation"]["transaction_id"].as_str().unwrap();
        journal(&fixture.home, tx, "restored");
        let request = id();
        let rollback = recover(
            &fixture.home,
            "normal",
            RecoverInput {
                request_id: request.clone(),
                operation_id: old.clone(),
                action: RecoveryAction::Rollback,
            },
            &launch,
        )
        .unwrap();
        assert_ne!(rollback["operation"]["transaction_id"], tx);
        assert_eq!(rollback["operation"]["id"], request);
        let previous = status(&fixture.home, "normal", Some(&old), &launch).unwrap();
        assert_eq!(previous["operation"]["phase"], "completed");
        assert_eq!(previous["operation"]["transaction_id"], tx);
    }
    #[test]
    fn completed_old_operation_cannot_be_resumed_after_fresh_rollback() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let old = id();
        let first = apply(
            &fixture.home,
            ApplyInput {
                request_id: old.clone(),
                check_id: checked,
            },
            &launch,
        )
        .unwrap();
        journal(
            &fixture.home,
            first["operation"]["transaction_id"].as_str().unwrap(),
            "restored",
        );
        let rollback = recover(
            &fixture.home,
            "normal",
            RecoverInput {
                request_id: id(),
                operation_id: old.clone(),
                action: RecoveryAction::Rollback,
            },
            &launch,
        )
        .unwrap();
        journal(
            &fixture.home,
            rollback["operation"]["transaction_id"].as_str().unwrap(),
            "restored",
        );
        let mut ledger = load(&fixture.home).unwrap();
        settle_current(&fixture.home, &mut ledger).unwrap();
        save(&fixture.home, &ledger).unwrap();
        let before = fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap();
        let calls = launch.calls.load(Ordering::SeqCst);
        assert_eq!(
            recover(
                &fixture.home,
                "normal",
                RecoverInput {
                    request_id: id(),
                    operation_id: old,
                    action: RecoveryAction::Resume
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            before
        );
        assert_eq!(launch.calls.load(Ordering::SeqCst), calls);
    }
    #[test]
    fn adopted_foreign_operation_identifier_cannot_be_overwritten_by_new_request() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let request = id();
        let accepted = apply(
            &fixture.home,
            ApplyInput {
                request_id: request.clone(),
                check_id: checked.clone(),
            },
            &launch,
        )
        .unwrap();
        let tx = accepted["operation"]["transaction_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut ledger = load(&fixture.home).unwrap();
        let mut adopted = ledger.operations.remove(&request).unwrap();
        adopted.id = tx.clone();
        adopted.kind = OperationKind::Existing;
        adopted.state = OperationState::Completed;
        adopted.request_id = None;
        ledger.operations.insert(tx.clone(), adopted);
        ledger.current_operation = Some(tx.clone());
        ledger.requests.clear();
        save(&fixture.home, &ledger).unwrap();
        journal(&fixture.home, &tx, "restored");
        let before = fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap();
        let calls = launch.calls.load(Ordering::SeqCst);
        assert_eq!(
            apply(
                &fixture.home,
                ApplyInput {
                    request_id: tx.clone(),
                    check_id: checked
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            recover(
                &fixture.home,
                "normal",
                RecoverInput {
                    request_id: tx.clone(),
                    operation_id: tx,
                    action: RecoveryAction::Rollback
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            before
        );
        assert_eq!(launch.calls.load(Ordering::SeqCst), calls);
    }

    fn completed_record(fixture: &Fixture, request: &str, check: &str) -> Operation {
        let ledger = load(&fixture.home).unwrap();
        let checked = ledger.checks.get(check).unwrap();
        Operation {
            id: request.into(),
            request_id: Some(request.into()),
            transaction_id: id(),
            kind: OperationKind::Apply,
            previous_transaction: None,
            plan: checked.plan.clone(),
            helper: checked.helper.clone(),
            attempt: Attempt {
                id: request.into(),
                action: None,
                claimed: true,
                handoff_nonce: "private archive nonce".into(),
                launch: None,
            },
            state: OperationState::Completed,
            outcome: Some("applied".into()),
        }
    }

    #[test]
    fn owned_transaction_alias_is_rejected_before_archival_and_canonical_rollback_still_works() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let operation = id();
        let accepted = apply(
            &fixture.home,
            ApplyInput {
                request_id: operation.clone(),
                check_id: checked,
            },
            &launch,
        )
        .unwrap();
        let tx = accepted["operation"]["transaction_id"].as_str().unwrap();
        journal(&fixture.home, tx, "restored");
        let mut ledger = load(&fixture.home).unwrap();
        settle_current(&fixture.home, &mut ledger).unwrap();
        save(&fixture.home, &ledger).unwrap();
        let before = fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap();
        let calls = launch.calls.load(Ordering::SeqCst);
        assert_eq!(
            recover(
                &fixture.home,
                "normal",
                RecoverInput {
                    request_id: id(),
                    operation_id: tx.into(),
                    action: RecoveryAction::Rollback
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            before
        );
        assert!(!fixture.home.join(DIRECTORY).join("history").exists());
        assert_eq!(launch.calls.load(Ordering::SeqCst), calls);
        let accepted = recover(
            &fixture.home,
            "normal",
            RecoverInput {
                request_id: id(),
                operation_id: operation.clone(),
                action: RecoveryAction::Rollback,
            },
            &launch,
        )
        .unwrap();
        assert_ne!(accepted["operation"]["transaction_id"], tx);
        let active = load(&fixture.home).unwrap();
        assert_eq!(
            get_operation(&fixture.home, &active, &operation)
                .unwrap()
                .unwrap()
                .transaction_id,
            tx
        );
    }
    #[test]
    fn malformed_or_nonterminal_archives_never_grant_request_or_cleanup_authority() {
        let fixture = Fixture::new();
        let checked = fixture.check(&LostReply::default());
        let request = id();
        let operation = completed_record(&fixture, &request, &checked);
        let ledger = load(&fixture.home).unwrap();
        history_write(
            &fixture.home,
            &ledger.installation_id,
            "operations",
            &request,
            &operation,
        )
        .unwrap();
        let path = history_path(&fixture.home, "operations", &request).unwrap();
        let valid = fs::read(&path).unwrap();
        for field in ["installation_id", "id"] {
            let mut corrupt: Value = serde_json::from_slice(&valid).unwrap();
            corrupt[field] = json!(id());
            fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
            assert!(archived_operation(&fixture.home, &ledger.installation_id, &request).is_err());
        }
        let mut corrupt: Value = serde_json::from_slice(&valid).unwrap();
        corrupt["value"]["state"] = json!("unknown");
        fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        assert!(archived_operation(&fixture.home, &ledger.installation_id, &request).is_err());
        corrupt["value"]["state"] = json!("completed");
        corrupt["value"]["outcome"] = Value::Null;
        fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
        assert!(
            cleanup_authority(&fixture.home)
                .unwrap()
                .worker(&request, &request)
                .is_err()
        );
        fs::write(&path, b"not-json").unwrap();
        assert!(get_operation(&fixture.home, &ledger, &request).is_err());
    }
    #[test]
    fn completed_history_can_exceed_active_limit_without_losing_old_request_authority() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let old = id();
        let mut ledger = load(&fixture.home).unwrap();
        let mut old_operation = completed_record(&fixture, &old, &checked);
        old_operation.plan["retained_fixture"] = json!("x".repeat(128 * 1024));
        let old_tx = old_operation.transaction_id.clone();
        for n in 0..40 {
            let request = if n == 0 { old.clone() } else { id() };
            let mut operation = old_operation.clone();
            operation.id = request.clone();
            operation.request_id = Some(request.clone());
            operation.attempt.id = request.clone();
            if n != 0 {
                operation.transaction_id = id();
            }
            ledger.requests.insert(
                request.clone(),
                RequestRecord {
                    payload: RequestPayload::Apply {
                        check_id: checked.clone(),
                    },
                    operation_id: request.clone(),
                    attempt_id: request.clone(),
                },
            );
            ledger.operations.insert(request, operation);
        }
        let pending = id();
        let mut unknown = completed_record(&fixture, &pending, &checked);
        unknown.state = OperationState::Unknown;
        unknown.outcome = None;
        ledger.operations.insert(pending.clone(), unknown);
        ledger.current_operation = Some(pending.clone());
        assert!(serde_json::to_vec(&ledger).unwrap().len() > MAX_LEDGER_BYTES);
        save(&fixture.home, &ledger).unwrap();
        let active = load(&fixture.home).unwrap();
        assert_eq!(active.operations.len(), 1);
        assert_eq!(active.operations[&pending].state, OperationState::Unknown);
        assert!(
            fs::metadata(fixture.home.join(DIRECTORY).join(LEDGER))
                .unwrap()
                .len()
                < MAX_LEDGER_BYTES as u64
        );
        let historical = status(&fixture.home, "normal", Some(&old), &launch).unwrap();
        assert_eq!(historical["request"]["state"], "recorded");
        assert_eq!(historical["operation"]["transaction_id"], old_tx);
        assert_eq!(historical["actions"]["apply"], false);
        let repeated = apply(
            &fixture.home,
            ApplyInput {
                request_id: old.clone(),
                check_id: checked.clone(),
            },
            &launch,
        )
        .unwrap();
        assert_eq!(repeated["operation"]["transaction_id"], old_tx);
        assert_eq!(launch.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            apply(
                &fixture.home,
                ApplyInput {
                    request_id: old.clone(),
                    check_id: id()
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        let authority = cleanup_authority(&fixture.home).unwrap();
        assert!(authority.worker(&old, &old).unwrap());
        assert!(authority.replacement(&old_tx).unwrap());
        assert!(!authority.current_settled);
    }
    #[test]
    fn crash_before_active_publication_reuses_exact_immutable_archive_bytes() {
        let fixture = Fixture::new();
        let checked = fixture.check(&LostReply::default());
        let old = id();
        let operation = completed_record(&fixture, &old, &checked);
        let mut ledger = load(&fixture.home).unwrap();
        ledger.current_operation = Some(old.clone());
        ledger.operations.insert(old.clone(), operation.clone());
        ledger.requests.insert(
            old.clone(),
            RequestRecord {
                payload: RequestPayload::Apply {
                    check_id: checked.clone(),
                },
                operation_id: old.clone(),
                attempt_id: old.clone(),
            },
        );
        save(&fixture.home, &ledger).unwrap();
        let before = fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap();
        let next = id();
        let mut pending = completed_record(&fixture, &next, &checked);
        pending.state = OperationState::Accepted;
        pending.outcome = None;
        ledger.operations.insert(next.clone(), pending);
        ledger.current_operation = Some(next);
        compact(&fixture.home, &ledger).unwrap();
        let path = history_path(&fixture.home, "operations", &old).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            before
        );
        compact(&fixture.home, &ledger).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        save(&fixture.home, &ledger).unwrap();
        let active = load(&fixture.home).unwrap();
        assert!(!active.operations.contains_key(&old));
        assert!(!active.requests.contains_key(&old));
        assert_eq!(
            get_operation(&fixture.home, &active, &old)
                .unwrap()
                .unwrap()
                .transaction_id,
            operation.transaction_id
        );
        assert_eq!(
            get_request(&fixture.home, &active, &old)
                .unwrap()
                .unwrap()
                .operation_id,
            old
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn mismatched_existing_archive_never_overwrites_history_or_active_ledger() {
        let fixture = Fixture::new();
        let checked = fixture.check(&LostReply::default());
        let old = id();
        let mut ledger = load(&fixture.home).unwrap();
        ledger
            .operations
            .insert(old.clone(), completed_record(&fixture, &old, &checked));
        let before = fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap();
        compact(&fixture.home, &ledger).unwrap();
        let path = history_path(&fixture.home, "operations", &old).unwrap();
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["value"]["outcome"] = json!("rolled_back");
        let changed = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &changed).unwrap();
        assert!(save(&fixture.home, &ledger).is_err());
        assert_eq!(fs::read(&path).unwrap(), changed);
        assert_eq!(
            fs::read(fixture.home.join(DIRECTORY).join(LEDGER)).unwrap(),
            before
        );
    }
    #[test]
    fn archived_check_remains_valid_and_idempotent_without_loading_history_scan() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let old = fixture.check(&launch);
        let latest = fixture.check(&launch);
        assert_ne!(old, latest);
        let ledger = load(&fixture.home).unwrap();
        assert_eq!(ledger.checks.len(), 1);
        assert!(!ledger.checks.contains_key(&old));
        assert!(
            history_path(&fixture.home, "checks", &old)
                .unwrap()
                .is_file()
        );
        let request = id();
        let input = ApplyInput {
            request_id: request,
            check_id: old,
        };
        let first = apply(&fixture.home, input.clone(), &launch).unwrap();
        let repeated = apply(&fixture.home, input, &launch).unwrap();
        assert_eq!(
            first["operation"]["transaction_id"],
            repeated["operation"]["transaction_id"]
        );
        assert_eq!(launch.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn cleanup_authority_requires_verified_terminal_outcome_and_known_attempts() {
        let fixture = Fixture::new();
        let launch = LostReply::default();
        let checked = fixture.check(&launch);
        let first = id();
        let accepted = apply(
            &fixture.home,
            ApplyInput {
                request_id: first.clone(),
                check_id: checked,
            },
            &launch,
        )
        .unwrap();
        let tx = accepted["operation"]["transaction_id"].as_str().unwrap();
        let second = id();
        recover(
            &fixture.home,
            "normal",
            RecoverInput {
                request_id: second.clone(),
                operation_id: first.clone(),
                action: RecoveryAction::Resume,
            },
            &launch,
        )
        .unwrap();
        let pending = cleanup_authority(&fixture.home).unwrap();
        assert!(!pending.worker(&first, &first).unwrap());
        assert!(!pending.replacement(tx).unwrap());
        assert!(!pending.current_settled);
        journal(&fixture.home, tx, "restored");
        let settled = cleanup_authority(&fixture.home).unwrap();
        assert!(settled.worker(&first, &first).unwrap());
        assert!(settled.worker(&first, &second).unwrap());
        assert!(!settled.worker(&first, &id()).unwrap());
        assert!(settled.replacement(tx).unwrap());
        assert!(settled.current_settled);
        let mut ledger = load(&fixture.home).unwrap();
        let operation = ledger.operations.get_mut(&first).unwrap();
        operation.state = OperationState::Failed;
        operation.outcome = None;
        save(&fixture.home, &ledger).unwrap();
        assert!(
            !cleanup_authority(&fixture.home)
                .unwrap()
                .replacement(tx)
                .unwrap()
        );
    }
    #[test]
    fn unavailable_or_enrolled_never_records_or_dispatches_new_request() {
        let fixture = Fixture::new();
        let checked = fixture.check(&UnavailableLauncher);
        let request = id();
        assert_eq!(
            apply(
                &fixture.home,
                ApplyInput {
                    request_id: request.clone(),
                    check_id: checked.clone()
                },
                &UnavailableLauncher
            )
            .unwrap_err()
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status(
                &fixture.home,
                "normal",
                Some(&request),
                &UnavailableLauncher
            )
            .unwrap()["request"]["state"],
            "not_recorded"
        );
        write_json(
            &fixture.home.join("native-update-owner.json"),
            &json!({"owner":"fixture"}),
        );
        let launch = LostReply::default();
        assert_eq!(
            apply(
                &fixture.home,
                ApplyInput {
                    request_id: request,
                    check_id: checked
                },
                &launch
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(launch.calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn live_selected_listener_is_proved_during_interrupted_receipt_switch() {
        let fixture = Fixture::new();
        let tx = id();
        journal(&fixture.home, &tx, "verifying");
        let mut j = read_json(&fixture.home.join("native-update.json"));
        j["selected"] = json!("previous");
        j["launch_nonce"] = json!("retained-launch");
        write_json(&fixture.home.join("native-update.json"), &j);
        let runtime = json!({"pid":123,"process_nonce":"a".repeat(64)});
        write_json(&fixture.home.join("runtime.json"), &runtime);
        let mut status = json!({"id":Config::load(&fixture.home).unwrap().id,"pid":123,"process_nonce":"a".repeat(64),"transaction_id":tx,"launch_nonce":"retained-launch","binary_sha256":j["previous"]["binary_sha256"]});
        let stable =
            amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, &fixture.home);
        fs::write(&stable, b"interrupted stable receipt mismatch").unwrap();
        assert!(native_update::verify_installed(&fixture.home).is_err());
        assert!(native_update::verify_retained_listener(&fixture.home, &status).is_ok());
        status["pid"] = json!(124);
        assert!(native_update::verify_retained_listener(&fixture.home, &status).is_err());
        status["pid"] = json!(123);
        status["launch_nonce"] = json!("late-revoked-launch");
        assert!(native_update::verify_retained_listener(&fixture.home, &status).is_err());
        status["launch_nonce"] = json!("retained-launch");
        status["binary_sha256"] = json!("b".repeat(64));
        assert!(native_update::verify_retained_listener(&fixture.home, &status).is_err());
        assert_eq!(
            fs::read(stable).unwrap(),
            b"interrupted stable receipt mismatch"
        );
    }
    #[test]
    fn interrupted_switch_uses_retained_package_without_repairing_binary() {
        let fixture = Fixture::new();
        let tx = id();
        journal(&fixture.home, &tx, "switching");
        let stable =
            amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, &fixture.home);
        fs::write(&stable, b"interrupted replacement B").unwrap();
        assert!(native_update::verify_installed(&fixture.home).is_err());
        assert!(native_update::recovery_package(&fixture.home).is_ok());
        assert_eq!(fs::read(&stable).unwrap(), b"interrupted replacement B");
        let mut record = read_json(&fixture.home.join("native-update.json"));
        record["phase"] = json!("restoring");
        record["restore_started"] = json!(true);
        write_json(&fixture.home.join("native-update.json"), &record);
        assert!(native_update::recovery_package(&fixture.home).is_ok());
        assert_eq!(
            status(&fixture.home, "recovery", None, &LostReply::default()).unwrap()["actions"]["rollback"],
            false
        );
    }
}
