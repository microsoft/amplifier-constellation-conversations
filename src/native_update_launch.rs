//! Fixed, one-attempt OS launches for the browser updater and its replacement.
//!
//! The caller first persists the prepared profile in its private operation or
//! native journal. This module then creates a separate durable dispatch marker
//! before touching the manager. An existing marker is observation-only, including
//! after a lost reply or a crash before registration. Manager acceptance does not
//! prove that the worker entered or that replacement startup completed.
use crate::{config::Config, native_bundle::APPLICATION};
use amplifier_native_platform::{
    installation::{self, ActionOutcome, Service, ServiceAction, ServiceManager},
    locking::InstallationLock,
    registration::{self, Registration, RegistrationOutcome, RetirementOutcome},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Identity {
    Worker { operation: String, attempt: String },
    Replacement { transaction: String, nonce: String },
}

/// Private journal data, never an API request or browser response. Replacement
/// profiles contain the native startup nonce. Deserialized profiles are rebuilt
/// from their fixed identity before any manager operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparedLaunch {
    schema: u32,
    home: PathBuf,
    installation_id: String,
    identity: Identity,
    service: Service,
    profile: Registration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchReceipt {
    /// The manager accepted Start; independently prove process entry/readiness.
    Accepted,
    Refused {
        code: String,
    },
    Unknown {
        code: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchRecord {
    prepared: PreparedLaunch,
    // False is durable proof that this producer never entered the manager.
    // Old records lacked the field and must retain their uncertain authority.
    #[serde(default = "legacy_manager_attempted")]
    manager_attempted: bool,
    receipt: Option<LaunchReceipt>,
}
fn legacy_manager_attempted() -> bool {
    true
}

/// Private manager evidence, including the complete definition and process
/// generation. Do not expose this snapshot or its arguments to the browser.
#[derive(Clone, Debug)]
pub struct LaunchObservation {
    pub receipt: Option<LaunchReceipt>,
    pub profile_matches: bool,
    pub snapshot: Value,
}

/// Non-secret identity used by the caller's settled-operation eligibility check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchIdentity {
    Worker { operation: String, attempt: String },
    Replacement { transaction: String },
}

/// Private enumeration result, not a browser response. The prepared replacement
/// still contains its startup nonce and must remain in protected storage.
#[derive(Clone, Debug)]
pub struct RetainedLaunch {
    pub identity: LaunchIdentity,
    pub prepared: PreparedLaunch,
    pub launch_receipt: Option<LaunchReceipt>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetirementReceipt {
    Retired { removed: bool },
    Refused { code: String },
    Unknown { code: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementRecord {
    prepared: PreparedLaunch,
    expected: Value,
    receipt: Option<RetirementReceipt>,
}

const MAX_HOME_ENTRIES: usize = 4096;
const MAX_OUTSTANDING_LAUNCHES: usize = 256;
const MAX_INDEX_BYTES: usize = 128 * 1024;
const MAX_LAUNCH_BYTES: usize = 128 * 1024;
const MAX_ENUMERATION_BYTES: usize = 8 * 1024 * 1024;
const MAX_RETIREMENT_BYTES: usize = 1024 * 1024;

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    ordinary(path, false)?;
    ensure!(
        fs::metadata(path)?.len() <= limit as u64,
        "Fixed control record exceeds observation limit"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "Fixed control record grew beyond observation limit"
    );
    Ok(bytes)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutstandingIndex {
    schema: u32,
    installation_id: String,
    // The digest binds the immutable prepared profile, not its mutable receipt.
    launches: BTreeMap<String, String>,
}

fn index_path(home: &Path) -> PathBuf {
    home.join("native-browser-outstanding.json")
}
fn index_lock(home: &Path) -> Result<Option<InstallationLock>> {
    Ok(InstallationLock::try_acquire(
        &home.join("native-browser-outstanding.lock"),
    )?)
}
fn profile_lock(plan: &PreparedLaunch) -> Result<Option<InstallationLock>> {
    Ok(InstallationLock::try_acquire(&plan.home.join(format!(
        "native-browser-dispatch-{}.lock",
        plan.service.name
    )))?)
}
fn marker_name(plan: &PreparedLaunch) -> String {
    format!("native-browser-launch-{}.json", plan.service.name)
}
fn profile_digest(plan: &PreparedLaunch) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(plan)?)))
}
fn validate_index(index: &OutstandingIndex, id: &str) -> Result<()> {
    ensure!(
        index.schema == 1 && index.installation_id == id,
        "Outstanding launch index belongs to another installation"
    );
    ensure!(
        index.launches.len() <= MAX_OUTSTANDING_LAUNCHES,
        "Too many outstanding registrations"
    );
    for (name, digest) in &index.launches {
        ensure!(
            name.starts_with("native-browser-launch-")
                && name.ends_with(".json")
                && name.len() <= 256
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._".contains(&b))
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid outstanding launch identity"
        );
    }
    Ok(())
}
fn read_index(home: &Path, id: &str) -> Result<Option<OutstandingIndex>> {
    let path = index_path(home);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let index: OutstandingIndex = serde_json::from_slice(&read_bounded(&path, MAX_INDEX_BYTES)?)?;
    validate_index(&index, id)?;
    Ok(Some(index))
}

// Only an installation with no prior dispatch evidence can initialize an index.
// Browser updates have not shipped with the earlier marker-only protocol. Its
// retained fixture evidence must be inspected explicitly, never guessed away.
fn require_no_legacy_markers(home: &Path) -> Result<()> {
    for (index, entry) in fs::read_dir(home)?.enumerate() {
        ensure!(
            index < MAX_HOME_ENTRIES,
            "Installation directory exceeds initial inventory limit"
        );
        let name = entry?.file_name();
        if let Some(name) = name.to_str() {
            ensure!(
                !(name.starts_with("native-browser-launch-") && name.ends_with(".json")),
                "Legacy pre-index launch markers require explicit inspection"
            );
        }
    }
    Ok(())
}
fn initialize_index(home: &Path, id: &str) -> Result<OutstandingIndex> {
    if let Some(index) = read_index(home, id)? {
        return Ok(index);
    }
    require_no_legacy_markers(home)?;
    let index = OutstandingIndex {
        schema: 1,
        installation_id: id.into(),
        launches: BTreeMap::new(),
    };
    amplifier_native_platform::private_create(&index_path(home), &serde_json::to_vec(&index)?)?;
    Ok(index)
}

// This small storage seam lets the regression cover both failure before rename
// and a committed atomic write whose acknowledgment is lost.
trait IndexWriter {
    fn write(&self, home: &Path, bytes: &[u8]) -> Result<()>;
}
struct PrivateIndexWriter;
impl IndexWriter for PrivateIndexWriter {
    fn write(&self, home: &Path, bytes: &[u8]) -> Result<()> {
        amplifier_native_platform::private_write_data(&index_path(home), bytes)
    }
}
fn save_index(home: &Path, index: &OutstandingIndex, writer: &dyn IndexWriter) -> Result<()> {
    validate_index(index, &index.installation_id)?;
    let bytes = serde_json::to_vec(index)?;
    ensure!(
        bytes.len() <= MAX_INDEX_BYTES,
        "Outstanding index exceeds storage bound"
    );
    writer.write(home, &bytes)
}
fn require_membership(index: &OutstandingIndex, plan: &PreparedLaunch) -> Result<()> {
    ensure!(
        index.launches.get(&marker_name(plan)) == Some(&profile_digest(plan)?),
        "Outstanding index does not bind this fixed launch"
    );
    Ok(())
}
fn remove_membership(plan: &PreparedLaunch, writer: &dyn IndexWriter) -> Result<()> {
    let _lock = index_lock(&plan.home)?.context("Outstanding launch index is busy")?;
    let mut index = read_index(&plan.home, &plan.installation_id)?
        .context("Missing outstanding launch index; retained evidence requires inspection")?;
    // A previous removal may have committed even though its reply was lost.
    if index.launches.contains_key(&marker_name(plan)) {
        require_membership(&index, plan)?;
        index.launches.remove(&marker_name(plan));
        save_index(&plan.home, &index, writer)?;
    }
    Ok(())
}

/// Read only; observes the bounded outstanding index and its exact dispatch
/// records. Retired historical markers remain untouched and do not consume the
/// bound. Missing legacy inventory is a refusal, never an empty success.
pub fn enumerate(home: &Path) -> Result<Vec<RetainedLaunch>> {
    ordinary(home, true)?;
    let home = dunce::canonicalize(home)?;
    let id = Config::load(&home)?.id;
    let Some(index) = read_index(&home, &id)? else {
        require_no_legacy_markers(&home)?;
        return Ok(Vec::new());
    };
    let mut total = 0;
    let mut launches = Vec::with_capacity(index.launches.len());
    for (name, digest) in index.launches {
        let path = home.join(name);
        let bytes = read_bounded(&path, MAX_LAUNCH_BYTES)?;
        total += bytes.len();
        ensure!(
            total <= MAX_ENUMERATION_BYTES,
            "Outstanding launch data exceeds enumeration limit"
        );
        let record: DispatchRecord = serde_json::from_slice(&bytes)?;
        let plan = record.prepared;
        plan.validate()?;
        ensure!(
            plan.home == home && plan.marker() == path && profile_digest(&plan)? == digest,
            "Launch marker belongs to another home or fixed identity"
        );
        let identity = match &plan.identity {
            Identity::Worker { operation, attempt } => LaunchIdentity::Worker {
                operation: operation.clone(),
                attempt: attempt.clone(),
            },
            Identity::Replacement { transaction, .. } => LaunchIdentity::Replacement {
                transaction: transaction.clone(),
            },
        };
        launches.push(RetainedLaunch {
            identity,
            prepared: plan,
            launch_receipt: record.receipt,
        });
    }
    Ok(launches)
}

trait RetirementManager: Manager {
    fn is_retired(&self, plan: &PreparedLaunch) -> Result<bool>;
    fn retire(&self, plan: &PreparedLaunch, expected: &Value) -> Result<RetirementOutcome>;
}
impl RetirementManager for NativeManager {
    fn is_retired(&self, plan: &PreparedLaunch) -> Result<bool> {
        registration::is_retired(&plan.service)
    }
    fn retire(&self, plan: &PreparedLaunch, expected: &Value) -> Result<RetirementOutcome> {
        registration::retire(&plan.service, &plan.profile, expected)
    }
}

fn retirement_path(plan: &PreparedLaunch) -> PathBuf {
    plan.home.join(format!(
        "native-browser-retirement-{}.json",
        plan.service.name
    ))
}
fn read_retirement(plan: &PreparedLaunch) -> Result<Option<RetirementRecord>> {
    let path = retirement_path(plan);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let record: RetirementRecord =
        serde_json::from_slice(&read_bounded(&path, MAX_RETIREMENT_BYTES)?)?;
    ensure!(
        record.prepared == *plan,
        "Retirement marker binds a different fixed profile"
    );
    Ok(Some(record))
}
fn retirement_unknown(code: &str) -> RetirementReceipt {
    RetirementReceipt::Unknown { code: code.into() }
}
fn retirement_observation(
    plan: &PreparedLaunch,
    manager: &dyn RetirementManager,
    previous: Option<RetirementRecord>,
) -> Result<Option<RetirementReceipt>> {
    if manager.is_retired(plan)? {
        return Ok(Some(RetirementReceipt::Retired { removed: false }));
    }
    Ok(previous.map(|record| match record.receipt {
        Some(receipt @ RetirementReceipt::Refused { .. }) => receipt,
        _ => retirement_unknown("retirement_unconfirmed"),
    }))
}

/// Observe confirmed manager AND definition absence without retrying cleanup.
/// False/uninspectable native presence never becomes deletion authority.
pub fn observe_retirement(prepared: &PreparedLaunch) -> Result<Option<RetirementReceipt>> {
    observe_retirement_with(prepared, &NativeManager)
}
fn observe_retirement_with(
    plan: &PreparedLaunch,
    manager: &dyn RetirementManager,
) -> Result<Option<RetirementReceipt>> {
    plan.validate()?;
    ensure!(
        read_dispatch(plan)?.is_some(),
        "Missing original dispatch evidence"
    );
    retirement_observation(plan, manager, read_retirement(plan)?)
}

/// Caller must first prove a terminal owned operation/transaction under its
/// lifecycle locks and authorize explicit cleanup. This wrapper never stops a
/// process or removes evidence. The shared candidate rechecks the entire native
/// snapshot/profile and owns all manager/definition retirement.
pub fn retire(prepared: &PreparedLaunch) -> Result<RetirementReceipt> {
    retire_with(prepared, &NativeManager)
}
fn retire_with(
    plan: &PreparedLaunch,
    manager: &dyn RetirementManager,
) -> Result<RetirementReceipt> {
    retire_with_writer(plan, manager, &PrivateIndexWriter)
}
fn retire_with_writer(
    plan: &PreparedLaunch,
    manager: &dyn RetirementManager,
    writer: &dyn IndexWriter,
) -> Result<RetirementReceipt> {
    plan.validate()?;
    let Some(_profile_lock) = profile_lock(plan)? else {
        return Ok(retirement_unknown("dispatch_or_retirement_in_progress"));
    };
    ensure!(
        read_dispatch(plan)?.is_some(),
        "Missing original dispatch evidence"
    );
    let index = read_index(&plan.home, &plan.installation_id)?
        .context("Missing outstanding launch index; retained evidence requires inspection")?;
    let previous = read_retirement(plan)?;
    if manager.is_retired(plan)? {
        return Ok(match remove_membership(plan, writer) {
            Ok(()) => RetirementReceipt::Retired { removed: false },
            Err(_) => retirement_unknown("retirement_index_unconfirmed"),
        });
    }
    if previous.is_some() {
        return Ok(previous
            .map(|record| match record.receipt {
                Some(receipt @ RetirementReceipt::Refused { .. }) => receipt,
                _ => retirement_unknown("retirement_unconfirmed"),
            })
            .expect("Existing retirement yields an observation"));
    }
    require_membership(&index, plan)?;
    let expected = manager.snapshot(plan)?;
    // Ordinary active/transitioning/mismatched registrations get no retirement
    // intent marker, so a later explicitly authorized settled cleanup can run.
    // The shared primitive additionally checks every manager-specific condition.
    if !manager.matches(plan, &expected)? {
        return Ok(RetirementReceipt::Refused {
            code: "fixed_profile_mismatch".into(),
        });
    }
    if expected["active"] != false
        || expected["transitioning"] != false
        || expected["masked"] != false
        || expected["start_allowed"] != true
        || !expected["processes"].as_array().is_some_and(Vec::is_empty)
    {
        return Ok(RetirementReceipt::Refused {
            code: "registration_not_settled".into(),
        });
    }
    let mut record = RetirementRecord {
        prepared: plan.clone(),
        expected,
        receipt: None,
    };
    let bytes = serde_json::to_vec(&record)?;
    ensure!(
        bytes.len() <= MAX_RETIREMENT_BYTES,
        "Retirement intent exceeds storage bound"
    );
    if amplifier_native_platform::private_create(&retirement_path(plan), &bytes).is_err() {
        return Ok(retirement_unknown("retirement_intent_unconfirmed"));
    }
    let result = match manager.retire(plan, &record.expected) {
        Ok(RetirementOutcome::Retired { removed }) => RetirementReceipt::Retired { removed },
        Ok(RetirementOutcome::Refused { code, .. }) => RetirementReceipt::Refused { code },
        Ok(RetirementOutcome::Unknown { code, .. }) => RetirementReceipt::Unknown { code },
        Err(_) => retirement_unknown("retirement_unconfirmed"),
    };
    record.receipt = Some(result.clone());
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() > MAX_RETIREMENT_BYTES
        || amplifier_native_platform::private_write_data(&retirement_path(plan), &bytes).is_err()
    {
        return Ok(retirement_unknown("retirement_receipt_unconfirmed"));
    }
    if matches!(result, RetirementReceipt::Retired { .. })
        && remove_membership(plan, writer).is_err()
    {
        return Ok(retirement_unknown("retirement_index_unconfirmed"));
    }
    Ok(result)
}

fn uuid(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "Use a canonical launch UUID"
    );
    Ok(())
}

fn ordinary(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink()
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
        "Launch paths must be ordinary installation entries"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Reparse launch paths are not supported"
        );
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("Launch path must be representable as text")?;
    ensure!(
        !value.chars().any(char::is_control),
        "Launch path contains a control character"
    );
    Ok(value.to_owned())
}

fn fixed_service(home: &Path, name: &str) -> Result<Service> {
    let (manager, name, definition_path) = match std::env::consts::OS {
        "linux" => {
            let name = format!("{name}.service");
            let path = amplifier_native_platform::user_home()?
                .join(".config/systemd/user")
                .join(&name);
            (ServiceManager::SystemdUser, name, path)
        }
        "macos" => (
            ServiceManager::LaunchAgent,
            name.to_owned(),
            amplifier_native_platform::user_home()?
                .join("Library/LaunchAgents")
                .join(format!("{name}.plist")),
        ),
        "windows" => (
            ServiceManager::ScheduledTask,
            name.to_owned(),
            home.join(format!("{name}.xml")),
        ),
        _ => anyhow::bail!("Native update launches are unsupported on this platform"),
    };
    Ok(Service {
        manager,
        name,
        definition_path: Some(definition_path),
    })
}

fn prepare(home: &Path, id: &str, identity: Identity, executable: &Path) -> Result<PreparedLaunch> {
    ensure!(
        home.is_absolute() && executable.is_absolute(),
        "Launch paths must be absolute"
    );
    ordinary(home, true)?;
    ordinary(executable, false)?;
    let home = dunce::canonicalize(home)?;
    let executable = dunce::canonicalize(executable)?;
    ordinary(&home.join("config.json"), false)?;
    ensure!(
        Config::load(&home)?.id == id,
        "Launch installation identity changed"
    );
    uuid::Uuid::parse_str(id)?;
    let installed = amplifier_native_platform::installed_binary(&APPLICATION, &home);
    let (role, arguments) = match &identity {
        Identity::Worker { operation, attempt } => {
            uuid(operation)?;
            uuid(attempt)?;
            // The caller verifies the complete retained package manifest before
            // preparation. This also excludes the replaceable home/bin path.
            let relative = executable
                .strip_prefix(home.join("packages"))
                .context("Worker must come from this installation's immutable packages")?;
            let mut parts = relative.components();
            let digest = parts
                .next()
                .context("Missing worker package identity")?
                .as_os_str()
                .to_str()
                .context("Invalid worker package identity")?;
            ensure!(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    && parts.next().is_some(),
                "Worker package identity must be a retained manifest digest"
            );
            (
                "worker",
                vec![
                    "native-update-browser-worker".into(),
                    "--operation".into(),
                    operation.clone(),
                    "--attempt".into(),
                    attempt.clone(),
                ],
            )
        }
        Identity::Replacement { transaction, nonce } => {
            uuid(transaction)?;
            ensure!(
                !nonce.is_empty()
                    && nonce.len() <= 256
                    && nonce
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
                "Invalid native startup nonce"
            );
            ensure!(
                executable == dunce::canonicalize(installed)?,
                "Replacement must use the selected installed executable"
            );
            (
                "replacement",
                vec!["serve-update".into(), transaction.clone(), nonce.clone()],
            )
        }
    };
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(id, &home, &identity))?)
    );
    let name = format!("agent-conversations-{role}-{key}");
    let mut args = vec!["--home".into(), path_text(&home)?];
    args.extend(arguments);
    Ok(PreparedLaunch {
        schema: 1,
        service: fixed_service(&home, &name)?,
        profile: Registration {
            executable,
            args,
            cwd: home.clone(),
            description: format!("Companion native update {role}"),
            autostart: false,
        },
        home,
        installation_id: id.to_owned(),
        identity,
    })
}

pub fn prepare_worker(
    home: &Path,
    id: &str,
    operation: &str,
    attempt: &str,
    immutable_executable: &Path,
) -> Result<PreparedLaunch> {
    prepare(
        home,
        id,
        Identity::Worker {
            operation: operation.into(),
            attempt: attempt.into(),
        },
        immutable_executable,
    )
}

pub fn prepare_replacement(
    home: &Path,
    id: &str,
    transaction: &str,
    nonce: &str,
    installed_executable: &Path,
) -> Result<PreparedLaunch> {
    prepare(
        home,
        id,
        Identity::Replacement {
            transaction: transaction.into(),
            nonce: nonce.into(),
        },
        installed_executable,
    )
}

impl PreparedLaunch {
    fn validate(&self) -> Result<()> {
        ensure!(
            *self
                == prepare(
                    &self.home,
                    &self.installation_id,
                    self.identity.clone(),
                    &self.profile.executable
                )?,
            "Prepared fixed launch profile changed"
        );
        Ok(())
    }

    fn marker(&self) -> PathBuf {
        // Name derives only from installation + fixed operation/attempt identity;
        // changing the executable or arguments cannot obtain another dispatch.
        self.home
            .join(format!("native-browser-launch-{}.json", self.service.name))
    }
}

trait Manager {
    fn register(&self, plan: &PreparedLaunch) -> Result<RegistrationOutcome>;
    fn snapshot(&self, plan: &PreparedLaunch) -> Result<Value>;
    fn matches(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<bool>;
    fn start(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<ActionOutcome>;
}

struct NativeManager;
impl Manager for NativeManager {
    fn register(&self, plan: &PreparedLaunch) -> Result<RegistrationOutcome> {
        registration::register(&plan.service, &plan.profile)
    }
    fn snapshot(&self, plan: &PreparedLaunch) -> Result<Value> {
        installation::snapshot(&plan.service)
    }
    fn matches(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<bool> {
        registration::profile_matches(&plan.service, &plan.profile, snapshot)
    }
    fn start(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<ActionOutcome> {
        installation::request_state(&plan.service, ServiceAction::Start, snapshot)
    }
}

fn unknown(code: &str) -> LaunchReceipt {
    LaunchReceipt::Unknown { code: code.into() }
}

fn read_dispatch(plan: &PreparedLaunch) -> Result<Option<DispatchRecord>> {
    let path = plan.marker();
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let record: DispatchRecord = serde_json::from_slice(&read_bounded(&path, MAX_LAUNCH_BYTES)?)?;
    ensure!(
        record.prepared == *plan,
        "Dispatch marker binds a different fixed profile"
    );
    Ok(Some(record))
}

/// Dispatch at most once. The caller must persist `prepared` before calling and
/// release native/service locks before the manager can start the worker.
pub fn launch(prepared: &PreparedLaunch) -> Result<LaunchReceipt> {
    launch_with(prepared, &NativeManager)
}

fn launch_with(plan: &PreparedLaunch, manager: &dyn Manager) -> Result<LaunchReceipt> {
    launch_with_writer(plan, manager, &PrivateIndexWriter)
}
fn launch_with_writer(
    plan: &PreparedLaunch,
    manager: &dyn Manager,
    writer: &dyn IndexWriter,
) -> Result<LaunchReceipt> {
    plan.validate()?;
    // The same fixed profile lock spans retirement and confirmed index removal.
    // A paused original launch cannot register after cleanup observed absence.
    let Some(_profile_lock) = profile_lock(plan)? else {
        return Ok(unknown("dispatch_or_retirement_in_progress"));
    };
    if read_dispatch(plan)?.is_some() {
        return Ok(unknown("dispatch_already_attempted"));
    }
    let mut record = DispatchRecord {
        prepared: plan.clone(),
        manager_attempted: false,
        receipt: None,
    };
    {
        let Some(_index_lock) = index_lock(&plan.home)? else {
            return Ok(unknown("outstanding_index_busy"));
        };
        let mut index = initialize_index(&plan.home, &plan.installation_id)?;
        ensure!(
            index.launches.len() < MAX_OUTSTANDING_LAUNCHES,
            "Too many outstanding registrations"
        );
        ensure!(
            !index.launches.contains_key(&marker_name(plan)),
            "Outstanding launch marker is missing"
        );
        // Create-only publication wins races. Even a failed flush can follow
        // publication; it never permits a manager call or a later redispatch.
        if amplifier_native_platform::private_create(&plan.marker(), &serde_json::to_vec(&record)?)
            .is_err()
        {
            return Ok(unknown("dispatch_intent_unconfirmed"));
        }
        index
            .launches
            .insert(marker_name(plan), profile_digest(plan)?);
        if save_index(&plan.home, &index, writer).is_err() {
            return Ok(unknown("dispatch_index_unconfirmed"));
        }
        // False can remain outside the index only if no manager call was
        // possible. An ambiguous true publication remains indexed and unknown.
        record.manager_attempted = true;
        if amplifier_native_platform::private_write_data(
            &plan.marker(),
            &serde_json::to_vec(&record)?,
        )
        .is_err()
        {
            return Ok(unknown("dispatch_authority_unconfirmed"));
        }
    }
    let result = (|| -> Result<LaunchReceipt> {
        match manager.register(plan)? {
            RegistrationOutcome::Registered { .. } => {}
            RegistrationOutcome::Refused { code, .. } => {
                return Ok(LaunchReceipt::Refused { code });
            }
            RegistrationOutcome::Unknown { code, .. } => {
                return Ok(LaunchReceipt::Unknown { code });
            }
        }
        let snapshot = manager.snapshot(plan)?;
        if !manager.matches(plan, &snapshot)? {
            return Ok(LaunchReceipt::Refused {
                code: "fixed_profile_mismatch".into(),
            });
        }
        // Existing running/transitioning instances are observation-only. Starting
        // an already exited one is also forbidden by the durable dispatch marker.
        if snapshot["active"] != false || snapshot["transitioning"] != false {
            return Ok(unknown("existing_or_transitioning_instance"));
        }
        Ok(match manager.start(plan, &snapshot)? {
            ActionOutcome::Accepted => LaunchReceipt::Accepted,
            ActionOutcome::Refused { code, .. } => LaunchReceipt::Refused { code },
            ActionOutcome::Unknown { code, .. } => LaunchReceipt::Unknown { code },
        })
    })();
    let receipt = result.unwrap_or_else(|_| unknown("manager_dispatch_unconfirmed"));
    record.receipt = Some(receipt.clone());
    if amplifier_native_platform::private_write_data(&plan.marker(), &serde_json::to_vec(&record)?)
        .is_err()
    {
        return Ok(unknown("dispatch_receipt_unconfirmed"));
    }
    Ok(receipt)
}

/// Read only. A missing receipt, observation failure, or exited process never
/// permits replay. Worker/native startup receipts supply application completion.
pub fn observe(prepared: &PreparedLaunch) -> Result<LaunchObservation> {
    observe_with(prepared, &NativeManager)
}

fn observe_with(plan: &PreparedLaunch, manager: &dyn Manager) -> Result<LaunchObservation> {
    plan.validate()?;
    let receipt = read_dispatch(plan)?.and_then(|record| record.receipt);
    let snapshot = manager.snapshot(plan)?;
    let profile_matches = manager.matches(plan, &snapshot)?;
    Ok(LaunchObservation {
        receipt,
        profile_matches,
        snapshot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Fixture {
        _directory: tempfile::TempDir,
        home: PathBuf,
        id: String,
        worker: PathBuf,
        installed: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::Builder::new()
                .prefix("Companion launch café 子 $% ")
                .tempdir()
                .unwrap();
            let home = dunce::canonicalize(directory.path()).unwrap();
            let config = Config::local(&home, "Launch fixture", 54127).unwrap();
            config.save(&home).unwrap();
            let worker =
                home.join("packages")
                    .join("a".repeat(64))
                    .join("bin")
                    .join(if cfg!(windows) {
                        "agent-conversations.exe"
                    } else {
                        "agent-conversations"
                    });
            let installed = amplifier_native_platform::installed_binary(&APPLICATION, &home);
            for path in [&worker, &installed] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, b"fixture executable bytes").unwrap();
            }
            Self {
                _directory: directory,
                home,
                id: config.id,
                worker,
                installed,
            }
        }
        fn plan(&self) -> PreparedLaunch {
            prepare_worker(
                &self.home,
                &self.id,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                &self.worker,
            )
            .unwrap()
        }
    }

    struct FakeManager {
        registration: RegistrationOutcome,
        snapshot: Value,
        matches: bool,
        action: ActionOutcome,
        registrations: AtomicUsize,
        starts: AtomicUsize,
        observations: AtomicUsize,
    }
    impl FakeManager {
        fn new(action: ActionOutcome) -> Self {
            Self {
                registration: RegistrationOutcome::Registered { created: true },
                snapshot: json!({"active":false,"transitioning":false,"enabled":true,"masked":false,"definition":{"sha256":"exact-definition"},"processes":[{"identity":{"pid":123,"birth":"generation"}}]}),
                matches: true,
                action,
                registrations: AtomicUsize::new(0),
                starts: AtomicUsize::new(0),
                observations: AtomicUsize::new(0),
            }
        }
    }
    impl Manager for FakeManager {
        fn register(&self, plan: &PreparedLaunch) -> Result<RegistrationOutcome> {
            let index = read_index(&plan.home, &plan.installation_id)?.unwrap();
            require_membership(&index, plan)?;
            assert!(read_dispatch(plan)?.unwrap().manager_attempted);
            // Another profile (including the replacement) must be able to
            // publish while this profile is inside its manager call.
            let _index_lock = index_lock(&plan.home)?.expect("Index lock released before manager");
            self.registrations.fetch_add(1, Ordering::SeqCst);
            Ok(self.registration.clone())
        }
        fn snapshot(&self, _: &PreparedLaunch) -> Result<Value> {
            self.observations.fetch_add(1, Ordering::SeqCst);
            Ok(self.snapshot.clone())
        }
        fn matches(&self, _: &PreparedLaunch, snapshot: &Value) -> Result<bool> {
            assert_eq!(*snapshot, self.snapshot);
            Ok(self.matches)
        }
        fn start(&self, _: &PreparedLaunch, snapshot: &Value) -> Result<ActionOutcome> {
            // The launch adapter must pass the complete observation through;
            // active-bool/PID-only checks cannot stand in for shared snapshot CAS.
            assert_eq!(*snapshot, self.snapshot);
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(self.action.clone())
        }
    }

    #[test]
    fn preparation_is_read_only_and_worker_and_replacements_have_separate_fixed_profiles() {
        let fixture = Fixture::new();
        let before = fs::read_dir(&fixture.home).unwrap().count();
        let worker = fixture.plan();
        let tx = uuid::Uuid::new_v4().to_string();
        let first = prepare_replacement(
            &fixture.home,
            &fixture.id,
            &tx,
            "first-nonce",
            &fixture.installed,
        )
        .unwrap();
        let second = prepare_replacement(
            &fixture.home,
            &fixture.id,
            &tx,
            "second-nonce",
            &fixture.installed,
        )
        .unwrap();
        assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), before);
        assert!(!worker.profile.autostart && !first.profile.autostart);
        assert_eq!(worker.profile.executable, fixture.worker);
        assert_eq!(worker.profile.args[1], path_text(&fixture.home).unwrap());
        assert_eq!(worker.profile.args[2], "native-update-browser-worker");
        assert_eq!(&worker.profile.args[3], "--operation");
        assert_eq!(&worker.profile.args[5], "--attempt");
        assert_eq!(worker.profile.args.len(), 7);
        assert_eq!(
            first.profile.args[2..],
            ["serve-update", &tx, "first-nonce"]
        );
        assert_ne!(worker.service.name, first.service.name);
        assert_ne!(first.service.name, second.service.name);
        assert_ne!(first.marker(), second.marker());
        assert!(!first.service.name.contains("first-nonce"));
        assert!(prepare_worker(&fixture.home, &fixture.id, &tx, &tx, &fixture.installed).is_err());
        assert!(
            prepare_replacement(&fixture.home, &fixture.id, &tx, "nonce", &fixture.worker).is_err()
        );
    }

    #[test]
    fn deserialized_command_or_identity_changes_refuse_before_publication() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let manager = FakeManager::new(ActionOutcome::Accepted);
        let mut malicious = serde_json::to_value(&plan).unwrap();
        malicious["profile"]["args"] = json!(["arbitrary-command"]);
        let malicious: PreparedLaunch = serde_json::from_value(malicious).unwrap();
        assert!(launch_with(&malicious, &manager).is_err());
        assert!(!plan.marker().exists());
        let mut changed = plan.clone();
        changed.installation_id = uuid::Uuid::new_v4().to_string();
        assert!(launch_with(&changed, &manager).is_err());
        assert_eq!(manager.registrations.load(Ordering::SeqCst), 0);
        assert_eq!(manager.starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn another_valid_package_cannot_replace_an_attempts_recorded_executable() {
        let fixture = Fixture::new();
        let original = fixture.plan();
        let manager = FakeManager::new(ActionOutcome::Accepted);
        assert_eq!(
            launch_with(&original, &manager).unwrap(),
            LaunchReceipt::Accepted
        );
        let changed_path = fixture
            .home
            .join("packages")
            .join("b".repeat(64))
            .join("other-service");
        fs::create_dir_all(changed_path.parent().unwrap()).unwrap();
        fs::write(&changed_path, b"another verified package candidate").unwrap();
        let changed = prepare(
            &fixture.home,
            &fixture.id,
            original.identity.clone(),
            &changed_path,
        )
        .unwrap();
        assert_eq!(changed.marker(), original.marker());
        assert!(launch_with(&changed, &manager).is_err());
        assert!(observe_with(&changed, &manager).is_err());
        assert_eq!(manager.registrations.load(Ordering::SeqCst), 1);
        assert_eq!(manager.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lost_acknowledgment_and_observation_never_redispatch() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let manager = FakeManager::new(ActionOutcome::Unknown {
            code: "lost_reply".into(),
            message: "private manager detail".into(),
        });
        assert_eq!(launch_with(&plan, &manager).unwrap(), unknown("lost_reply"));
        let before = fs::read(plan.marker()).unwrap();
        assert_eq!(
            launch_with(&plan, &manager).unwrap(),
            unknown("dispatch_already_attempted")
        );
        let observation = observe_with(&plan, &manager).unwrap();
        assert_eq!(observation.receipt, Some(unknown("lost_reply")));
        assert!(observation.profile_matches);
        assert_eq!(observation.snapshot, manager.snapshot);
        assert_eq!(fs::read(plan.marker()).unwrap(), before);
        assert_eq!(manager.registrations.load(Ordering::SeqCst), 1);
        assert_eq!(manager.starts.load(Ordering::SeqCst), 1);
        assert!(
            !String::from_utf8(before)
                .unwrap()
                .contains("private manager detail")
        );
    }

    #[test]
    fn crash_after_intent_and_exited_accepted_attempt_remain_observation_only() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let manager = FakeManager::new(ActionOutcome::Accepted);
        record_intent(&plan, None);
        assert!(matches!(
            launch_with(&plan, &manager).unwrap(),
            LaunchReceipt::Unknown { .. }
        ));
        assert!(observe_with(&plan, &manager).unwrap().receipt.is_none());
        assert_eq!(manager.registrations.load(Ordering::SeqCst), 0);
        let fresh = fixture.plan();
        assert_eq!(
            launch_with(&fresh, &manager).unwrap(),
            LaunchReceipt::Accepted
        );
        assert_eq!(
            observe_with(&fresh, &manager).unwrap().snapshot["active"],
            false
        );
        assert!(matches!(
            launch_with(&fresh, &manager).unwrap(),
            LaunchReceipt::Unknown { .. }
        ));
        assert_eq!(manager.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn independent_definition_and_availability_refusals_never_start() {
        let fixture = Fixture::new();
        let mut manager = FakeManager::new(ActionOutcome::Accepted);
        manager.registration = RegistrationOutcome::Refused {
            code: "independently_disabled".into(),
            message: "preserved".into(),
        };
        assert!(matches!(
            launch_with(&fixture.plan(), &manager).unwrap(),
            LaunchReceipt::Refused { .. }
        ));
        assert_eq!(manager.observations.load(Ordering::SeqCst), 0);
        manager.registration = RegistrationOutcome::Registered { created: false };
        manager.matches = false;
        assert_eq!(
            launch_with(&fixture.plan(), &manager).unwrap(),
            LaunchReceipt::Refused {
                code: "fixed_profile_mismatch".into()
            }
        );
        manager.matches = true;
        manager.snapshot["active"] = json!(true);
        assert_eq!(
            launch_with(&fixture.plan(), &manager).unwrap(),
            unknown("existing_or_transitioning_instance")
        );
        assert_eq!(manager.starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn simultaneous_dispatchers_issue_one_manager_start() {
        let fixture = Fixture::new();
        let plan = Arc::new(fixture.plan());
        let manager = Arc::new(FakeManager::new(ActionOutcome::Accepted));
        let barrier = Arc::new(Barrier::new(3));
        let threads = (0..2)
            .map(|_| {
                let (plan, manager, barrier) = (plan.clone(), manager.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    launch_with(&plan, manager.as_ref()).unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let receipts = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            receipts
                .iter()
                .filter(|receipt| **receipt == LaunchReceipt::Accepted)
                .count(),
            1
        );
        assert_eq!(manager.registrations.load(Ordering::SeqCst), 1);
        assert_eq!(manager.starts.load(Ordering::SeqCst), 1);
    }

    struct FakeRetirement {
        manager: FakeManager,
        absent: AtomicBool,
        calls: AtomicUsize,
        outcome: RetirementOutcome,
    }
    impl FakeRetirement {
        fn new(outcome: RetirementOutcome) -> Self {
            let mut manager = FakeManager::new(ActionOutcome::Accepted);
            manager.snapshot["processes"] = json!([]);
            manager.snapshot["start_allowed"] = json!(true);
            Self {
                manager,
                absent: AtomicBool::new(false),
                calls: AtomicUsize::new(0),
                outcome,
            }
        }
    }
    impl Manager for FakeRetirement {
        fn register(&self, _: &PreparedLaunch) -> Result<RegistrationOutcome> {
            panic!("Retirement must never register a service")
        }
        fn snapshot(&self, plan: &PreparedLaunch) -> Result<Value> {
            self.manager.snapshot(plan)
        }
        fn matches(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<bool> {
            self.manager.matches(plan, snapshot)
        }
        fn start(&self, _: &PreparedLaunch, _: &Value) -> Result<ActionOutcome> {
            panic!("Retirement must never start a service")
        }
    }
    impl RetirementManager for FakeRetirement {
        fn is_retired(&self, _: &PreparedLaunch) -> Result<bool> {
            Ok(self.absent.load(Ordering::SeqCst))
        }
        fn retire(&self, plan: &PreparedLaunch, expected: &Value) -> Result<RetirementOutcome> {
            assert_eq!(*expected, self.manager.snapshot);
            let retained = read_retirement(plan)?.expect("Durable intent must precede retirement");
            assert_eq!(retained.expected, *expected);
            assert!(retained.receipt.is_none());
            self.calls.fetch_add(1, Ordering::SeqCst);
            if matches!(self.outcome, RetirementOutcome::Retired { .. }) {
                self.absent.store(true, Ordering::SeqCst);
            }
            Ok(self.outcome.clone())
        }
    }
    fn record_intent(plan: &PreparedLaunch, receipt: Option<LaunchReceipt>) {
        let _lock = index_lock(&plan.home).unwrap().unwrap();
        let mut index = initialize_index(&plan.home, &plan.installation_id).unwrap();
        let record = DispatchRecord {
            prepared: plan.clone(),
            manager_attempted: true,
            receipt,
        };
        amplifier_native_platform::private_create(
            &plan.marker(),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        index
            .launches
            .insert(marker_name(plan), profile_digest(plan).unwrap());
        save_index(&plan.home, &index, &PrivateIndexWriter).unwrap();
    }
    fn recorded(plan: &PreparedLaunch) {
        record_intent(plan, Some(LaunchReceipt::Accepted));
    }

    #[test]
    fn enumeration_is_read_only_binds_filenames_and_excludes_nonce_from_identity() {
        let fixture = Fixture::new();
        let worker = fixture.plan();
        let replacement = prepare_replacement(
            &fixture.home,
            &fixture.id,
            &uuid::Uuid::new_v4().to_string(),
            "private-startup-nonce",
            &fixture.installed,
        )
        .unwrap();
        recorded(&worker);
        recorded(&replacement);
        let original = fs::read(replacement.marker()).unwrap();
        let entries = fs::read_dir(&fixture.home).unwrap().count();
        let launches = enumerate(&fixture.home).unwrap();
        assert_eq!(launches.len(), 2);
        assert!(
            launches
                .iter()
                .any(|launch| matches!(launch.identity, LaunchIdentity::Worker { .. }))
        );
        assert!(
            launches
                .iter()
                .any(|launch| matches!(launch.identity, LaunchIdentity::Replacement { .. }))
        );
        assert!(
            !format!(
                "{:?}",
                launches
                    .iter()
                    .map(|launch| &launch.identity)
                    .collect::<Vec<_>>()
            )
            .contains("private-startup-nonce")
        );
        assert_eq!(fs::read(replacement.marker()).unwrap(), original);
        assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), entries);
        let wrong = fixture.home.join("native-browser-launch-wrong-name.json");
        fs::write(&wrong, original).unwrap();
        let mut index = read_index(&fixture.home, &fixture.id).unwrap().unwrap();
        index.launches.insert(
            wrong.file_name().unwrap().to_str().unwrap().into(),
            profile_digest(&replacement).unwrap(),
        );
        save_index(&fixture.home, &index, &PrivateIndexWriter).unwrap();
        assert!(enumerate(&fixture.home).is_err());
    }

    #[test]
    fn enumeration_refuses_oversized_and_excess_outstanding_before_cleanup() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        recorded(&plan);
        fs::File::create(plan.marker())
            .unwrap()
            .set_len(MAX_LAUNCH_BYTES as u64 + 1)
            .unwrap();
        assert!(enumerate(&fixture.home).is_err());
        assert!(!retirement_path(&plan).exists());
        let fixture = Fixture::new();
        let index = OutstandingIndex {
            schema: 1,
            installation_id: fixture.id.clone(),
            launches: (0..=MAX_OUTSTANDING_LAUNCHES)
                .map(|number| {
                    (
                        format!("native-browser-launch-{number}.json"),
                        "a".repeat(64),
                    )
                })
                .collect(),
        };
        amplifier_native_platform::private_write_data(
            &index_path(&fixture.home),
            &serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        assert!(
            enumerate(&fixture.home)
                .unwrap_err()
                .to_string()
                .contains("Too many outstanding")
        );
    }

    #[cfg(unix)]
    #[test]
    fn enumeration_refuses_a_symlink_without_following_it() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let target = fixture.home.join("unrelated.json");
        fs::write(&target, b"unchanged").unwrap();
        recorded(&plan);
        fs::remove_file(plan.marker()).unwrap();
        std::os::unix::fs::symlink(&target, plan.marker()).unwrap();
        assert!(enumerate(&fixture.home).is_err());
        assert_eq!(fs::read(target).unwrap(), b"unchanged");
    }

    #[test]
    fn active_replacement_refusal_leaves_later_explicit_retirement_possible() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        recorded(&plan);
        let dispatch = fs::read(plan.marker()).unwrap();
        let mut manager = FakeRetirement::new(RetirementOutcome::Retired { removed: true });
        manager.manager.snapshot["active"] = json!(true);
        assert_eq!(
            retire_with(&plan, &manager).unwrap(),
            RetirementReceipt::Refused {
                code: "registration_not_settled".into()
            }
        );
        assert!(!retirement_path(&plan).exists());
        manager.manager.snapshot["active"] = json!(false);
        assert_eq!(
            retire_with(&plan, &manager).unwrap(),
            RetirementReceipt::Retired { removed: true }
        );
        assert_eq!(
            retire_with(&plan, &manager).unwrap(),
            RetirementReceipt::Retired { removed: false }
        );
        assert_eq!(manager.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(plan.marker()).unwrap(), dispatch);
        assert!(retirement_path(&plan).exists());
    }

    #[test]
    fn uncertain_retirement_is_never_replayed_and_absence_is_read_only() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        recorded(&plan);
        let manager = FakeRetirement::new(RetirementOutcome::Unknown {
            code: "lost_reply".into(),
            message: "private definition detail".into(),
        });
        assert_eq!(
            retire_with(&plan, &manager).unwrap(),
            retirement_unknown("lost_reply")
        );
        let before = fs::read(retirement_path(&plan)).unwrap();
        assert!(matches!(
            retire_with(&plan, &manager).unwrap(),
            RetirementReceipt::Unknown { .. }
        ));
        assert!(matches!(
            observe_retirement_with(&plan, &manager).unwrap(),
            Some(RetirementReceipt::Unknown { .. })
        ));
        assert_eq!(manager.calls.load(Ordering::SeqCst), 1);
        manager.absent.store(true, Ordering::SeqCst);
        assert_eq!(
            observe_retirement_with(&plan, &manager).unwrap(),
            Some(RetirementReceipt::Retired { removed: false })
        );
        assert_eq!(
            retire_with(&plan, &manager).unwrap(),
            RetirementReceipt::Retired { removed: false }
        );
        assert_eq!(fs::read(retirement_path(&plan)).unwrap(), before);
        assert!(
            !String::from_utf8(before)
                .unwrap()
                .contains("private definition detail")
        );
        assert_eq!(manager.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn mismatched_profile_or_missing_evidence_never_enters_retirement() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let mut manager = FakeRetirement::new(RetirementOutcome::Retired { removed: true });
        assert!(retire_with(&plan, &manager).is_err());
        recorded(&plan);
        manager.manager.matches = false;
        assert_eq!(
            retire_with(&plan, &manager).unwrap(),
            RetirementReceipt::Refused {
                code: "fixed_profile_mismatch".into()
            }
        );
        assert_eq!(manager.calls.load(Ordering::SeqCst), 0);
        assert!(!retirement_path(&plan).exists());
    }

    #[test]
    fn post_intent_refusal_preserves_evidence_and_does_not_retry() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        recorded(&plan);
        let manager = FakeRetirement::new(RetirementOutcome::Refused {
            code: "stale_snapshot".into(),
            message: "preserved".into(),
        });
        let expected = RetirementReceipt::Refused {
            code: "stale_snapshot".into(),
        };
        assert_eq!(retire_with(&plan, &manager).unwrap(), expected);
        let original = fs::read(retirement_path(&plan)).unwrap();
        assert_eq!(retire_with(&plan, &manager).unwrap(), expected);
        assert_eq!(fs::read(retirement_path(&plan)).unwrap(), original);
        assert_eq!(manager.calls.load(Ordering::SeqCst), 1);
    }

    struct LostIndexWrite {
        commit: bool,
        calls: AtomicUsize,
    }
    impl IndexWriter for LostIndexWrite {
        fn write(&self, home: &Path, bytes: &[u8]) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.commit {
                PrivateIndexWriter.write(home, bytes)?;
            }
            anyhow::bail!("Injected index publication acknowledgment failure")
        }
    }

    #[test]
    fn missing_legacy_inventory_is_never_silently_reclassified() {
        let fixture = Fixture::new();
        let plan = fixture.plan();
        // This is the exact older shape, without manager_attempted. It must
        // deserialize conservatively even if the manager currently looks absent.
        let old = json!({"prepared":plan,"receipt":null});
        amplifier_native_platform::private_create(
            &plan.marker(),
            &serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        assert!(read_dispatch(&plan).unwrap().unwrap().manager_attempted);
        let before = fs::read(plan.marker()).unwrap();
        assert!(
            enumerate(&fixture.home)
                .unwrap_err()
                .to_string()
                .contains("Legacy pre-index")
        );
        let manager = FakeManager::new(ActionOutcome::Accepted);
        assert!(
            launch_with(&fixture.plan(), &manager)
                .unwrap_err()
                .to_string()
                .contains("Legacy pre-index")
        );
        let retire = FakeRetirement::new(RetirementOutcome::Retired { removed: true });
        retire.absent.store(true, Ordering::SeqCst);
        assert!(retire_with(&plan, &retire).is_err());
        assert_eq!(manager.registrations.load(Ordering::SeqCst), 0);
        assert_eq!(retire.calls.load(Ordering::SeqCst), 0);
        assert!(!index_path(&fixture.home).exists());
        assert_eq!(fs::read(plan.marker()).unwrap(), before);
    }

    #[test]
    fn lost_index_publication_never_enters_manager_or_retries_dispatch() {
        for commit in [false, true] {
            let fixture = Fixture::new();
            let plan = fixture.plan();
            let manager = FakeManager::new(ActionOutcome::Accepted);
            let writer = LostIndexWrite {
                commit,
                calls: AtomicUsize::new(0),
            };
            assert_eq!(
                launch_with_writer(&plan, &manager, &writer).unwrap(),
                unknown("dispatch_index_unconfirmed")
            );
            let bytes = fs::read(plan.marker()).unwrap();
            let record = read_dispatch(&plan).unwrap().unwrap();
            assert!(!record.manager_attempted);
            assert!(record.receipt.is_none());
            assert_eq!(enumerate(&fixture.home).unwrap().len(), usize::from(commit));
            assert_eq!(
                launch_with(&plan, &manager).unwrap(),
                unknown("dispatch_already_attempted")
            );
            assert_eq!(fs::read(plan.marker()).unwrap(), bytes);
            assert_eq!(manager.registrations.load(Ordering::SeqCst), 0);
            assert_eq!(manager.starts.load(Ordering::SeqCst), 0);
            assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn failed_index_removal_reconciles_without_repeating_native_retirement() {
        for commit in [false, true] {
            let fixture = Fixture::new();
            let plan = fixture.plan();
            recorded(&plan);
            let dispatch = fs::read(plan.marker()).unwrap();
            let manager = FakeRetirement::new(RetirementOutcome::Retired { removed: true });
            let writer = LostIndexWrite {
                commit,
                calls: AtomicUsize::new(0),
            };
            assert_eq!(
                retire_with_writer(&plan, &manager, &writer).unwrap(),
                retirement_unknown("retirement_index_unconfirmed")
            );
            assert_eq!(
                enumerate(&fixture.home).unwrap().len(),
                usize::from(!commit)
            );
            let index = fs::read(index_path(&fixture.home)).unwrap();
            let retirement = fs::read(retirement_path(&plan)).unwrap();
            assert_eq!(
                observe_retirement_with(&plan, &manager).unwrap(),
                Some(RetirementReceipt::Retired { removed: false })
            );
            assert_eq!(fs::read(index_path(&fixture.home)).unwrap(), index);
            assert_eq!(
                retire_with(&plan, &manager).unwrap(),
                RetirementReceipt::Retired { removed: false }
            );
            assert!(enumerate(&fixture.home).unwrap().is_empty());
            assert_eq!(manager.calls.load(Ordering::SeqCst), 1);
            assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
            assert_eq!(fs::read(plan.marker()).unwrap(), dispatch);
            assert_eq!(fs::read(retirement_path(&plan)).unwrap(), retirement);
        }
    }

    #[test]
    fn retired_history_beyond_old_limit_does_not_block_new_launch_or_cleanup() {
        let fixture = Fixture::new();
        let manager = FakeManager::new(ActionOutcome::Accepted);
        let retirement = FakeRetirement::new(RetirementOutcome::Retired { removed: true });
        let mut historical = Vec::new();
        for _ in 0..MAX_OUTSTANDING_LAUNCHES + 4 {
            let plan = fixture.plan();
            assert_eq!(
                launch_with(&plan, &manager).unwrap(),
                LaunchReceipt::Accepted
            );
            retirement.absent.store(false, Ordering::SeqCst);
            assert_eq!(
                retire_with(&plan, &retirement).unwrap(),
                RetirementReceipt::Retired { removed: true }
            );
            historical.push((
                plan.marker(),
                fs::read(plan.marker()).unwrap(),
                retirement_path(&plan),
            ));
        }
        let next = fixture.plan();
        assert_eq!(
            launch_with(&next, &manager).unwrap(),
            LaunchReceipt::Accepted
        );
        let outstanding = enumerate(&fixture.home).unwrap();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].prepared, next);
        retirement.absent.store(false, Ordering::SeqCst);
        assert_eq!(
            retire_with(&next, &retirement).unwrap(),
            RetirementReceipt::Retired { removed: true }
        );
        assert!(enumerate(&fixture.home).unwrap().is_empty());
        assert_eq!(
            manager.starts.load(Ordering::SeqCst),
            MAX_OUTSTANDING_LAUNCHES + 5
        );
        assert_eq!(
            retirement.calls.load(Ordering::SeqCst),
            MAX_OUTSTANDING_LAUNCHES + 5
        );
        for (marker, bytes, retirement) in historical {
            assert_eq!(fs::read(marker).unwrap(), bytes);
            assert!(retirement.exists());
        }
    }

    #[test]
    fn paused_dispatch_prevents_absence_cleanup_until_manager_call_finishes() {
        use std::sync::{Mutex, mpsc};
        use std::time::Duration;
        struct PausedManager {
            inner: FakeManager,
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl Manager for PausedManager {
            fn register(&self, plan: &PreparedLaunch) -> Result<RegistrationOutcome> {
                self.entered.send(())?;
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))?;
                self.inner.register(plan)
            }
            fn snapshot(&self, plan: &PreparedLaunch) -> Result<Value> {
                self.inner.snapshot(plan)
            }
            fn matches(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<bool> {
                self.inner.matches(plan, snapshot)
            }
            fn start(&self, plan: &PreparedLaunch, snapshot: &Value) -> Result<ActionOutcome> {
                self.inner.start(plan, snapshot)
            }
        }
        let fixture = Fixture::new();
        let plan = fixture.plan();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let manager = Arc::new(PausedManager {
            inner: FakeManager::new(ActionOutcome::Accepted),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let runner = {
            let plan = plan.clone();
            let manager = manager.clone();
            std::thread::spawn(move || launch_with(&plan, manager.as_ref()).unwrap())
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let absent = FakeRetirement::new(RetirementOutcome::Retired { removed: true });
        absent.absent.store(true, Ordering::SeqCst);
        assert_eq!(
            retire_with(&plan, &absent).unwrap(),
            retirement_unknown("dispatch_or_retirement_in_progress")
        );
        assert_eq!(enumerate(&fixture.home).unwrap().len(), 1);
        assert!(!retirement_path(&plan).exists());
        // A different profile is independent even while this one is paused.
        assert_eq!(
            launch_with(&fixture.plan(), &FakeManager::new(ActionOutcome::Accepted)).unwrap(),
            LaunchReceipt::Accepted
        );
        release_tx.send(()).unwrap();
        assert_eq!(runner.join().unwrap(), LaunchReceipt::Accepted);
        assert_eq!(
            retire_with(&plan, &absent).unwrap(),
            RetirementReceipt::Retired { removed: false }
        );
        assert_eq!(manager.inner.registrations.load(Ordering::SeqCst), 1);
        assert_eq!(absent.calls.load(Ordering::SeqCst), 0);
        assert_eq!(enumerate(&fixture.home).unwrap().len(), 1);
    }
}
