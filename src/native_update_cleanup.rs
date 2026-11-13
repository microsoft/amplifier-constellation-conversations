//! Stop-only authority for one accepted manager launch. Included inside the
//! update module so all journal writes retain its existing ownership/clock.
use super::*;
use crate::native_update_identity::{self as identity, Binding, StopAuthority};
use amplifier_native_platform::{
    installation::{self, ActionOutcome, Service, ServiceAction},
    process,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Target {
    Startup {
        snapshot: Value,
    },
    Replacement {
        prepared: crate::native_update_launch::PreparedLaunch,
    },
}
impl Target {
    fn service(&self) -> Result<Service> {
        match self {
            Self::Startup { snapshot } => native_startup_state::native_service(snapshot),
            Self::Replacement { prepared } => prepared.service(),
        }
    }
    fn validate_profile(&self, native: &Value) -> Result<()> {
        ensure!(
            native["service"] == serde_json::to_value(self.service()?)?,
            "Launch service changed"
        );
        match self {
            Self::Startup { snapshot } => native_startup_state::validate(snapshot),
            Self::Replacement { prepared } => {
                ensure!(
                    prepared.profile_matches(native)?,
                    "Replacement profile changed"
                );
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Ack {
    Intent,
    Accepted,
    Refused { code: String },
    Unknown { code: String },
}
impl Ack {
    fn from_result(result: Result<ActionOutcome>) -> Self {
        match result {
            Ok(ActionOutcome::Accepted) => Self::Accepted,
            Ok(ActionOutcome::Refused { code, .. }) => Self::Refused { code },
            Ok(ActionOutcome::Unknown { code, .. }) => Self::Unknown { code },
            Err(_) => Self::Unknown {
                code: "manager_call_unconfirmed".into(),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExitObservation {
    application_exited: bool,
    service_lock_acquired: bool,
    native_snapshot: Value,
    binary_sha256: String,
    cleanup_is_health: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusProof {
    id: String,
    binary_sha256: String,
    transaction_id: String,
    launch_nonce: String,
    pid: u32,
    process_nonce: String,
    admission_closed: bool,
    process_birth: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AuthoritySource {
    ProcessEntry,
    AuthenticatedStatus {
        first_status: Box<StatusProof>,
        confirmed_status: Box<StatusProof>,
        restore_started: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    binding: Binding,
    target: Target,
    start: Ack,
    authority: Option<StopAuthority>,
    // Older unreleased receipts had no provenance. Preserve that absence rather
    // than guessing an origin; every new capture writes an explicit source.
    #[serde(default)]
    authority_source: Option<AuthoritySource>,
    revoked: bool,
    stop: Option<Ack>,
    exit_before_switch: Option<ExitObservation>,
    #[serde(default)]
    uncertain_observation: Option<Value>,
}

fn read_receipt(j: &Value) -> Result<Option<Receipt>> {
    if j["launch_cleanup"].is_null() {
        return Ok(None);
    }
    let r: Receipt = serde_json::from_value(j["launch_cleanup"].clone())?;
    r.binding.validate()?;
    ensure!(
        r.schema == 1
            && j["installation_id"] == r.binding.installation_id
            && j["transaction_id"] == r.binding.transaction_id,
        "Cleanup receipt belongs to another installation or transaction"
    );
    if let Some(a) = &r.authority {
        ensure!(
            a.binding == r.binding && a.service == r.target.service()?,
            "Cleanup authority changed"
        );
        identity::validate_retained(a, &r.binding)?;
    }
    if let Some(source) = &r.authority_source {
        let authority = r
            .authority
            .as_ref()
            .context("Authority source lacks captured generation")?;
        if let AuthoritySource::AuthenticatedStatus {
            first_status,
            confirmed_status,
            restore_started,
        } = source
        {
            validate_status_proof(first_status, &r.binding, *restore_started)?;
            validate_status_proof(confirmed_status, &r.binding, *restore_started)?;
            validate_status_pair(first_status, confirmed_status, &authority.application)?;
        }
    }
    if let Some(exit) = &r.exit_before_switch {
        let a = r
            .authority
            .as_ref()
            .context("Exit evidence lacks accepted generation")?;
        ensure!(
            r.revoked
                && r.start == Ack::Accepted
                && r.stop == Some(Ack::Accepted)
                && exit.application_exited
                && exit.service_lock_acquired
                && !exit.cleanup_is_health
                && exit.binary_sha256 == r.binding.package["binary_sha256"]
                && exit.native_snapshot["service"] == serde_json::to_value(&a.service)?
                && exit.native_snapshot["active"] == false
                && exit.native_snapshot["transitioning"] == false
                && exit.native_snapshot["processes"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
                && native_instances_settled(&exit.native_snapshot)?,
            "Malformed or unbound application exit evidence"
        );
        same_policy(&a.native_snapshot, &exit.native_snapshot)?;
    }
    Ok(Some(r))
}
fn store(home: &Path, j: &mut Value, r: &Receipt) -> Result<()> {
    j["launch_cleanup"] = serde_json::to_value(r)?;
    save(&home.join(JOURNAL), j)
}

pub(super) fn ensure_new_start(j: &Value) -> Result<()> {
    if let Some(r) = read_receipt(j)? {
        ensure!(
            r.exit_before_switch.is_some() && r.stop == Some(Ack::Accepted),
            "Prior manager launch remains uncertain; no new nonce or redispatch"
        );
    }
    Ok(())
}

/// Explicit recovery retains an independent manager replacement even after the
/// original browser worker has exited. A proved Stop is not a reason to move
/// the next listener into the recovering process's child/job lifetime.
pub(super) fn retains_replacement(j: &Value) -> Result<bool> {
    Ok(read_receipt(j)?.is_some_and(|r| matches!(r.target, Target::Replacement { .. })))
}

/// Called under the same final startup guard as executable validation. The
/// serving process writes its separate entry, never the updater-owned journal.
pub(super) fn publish_entry(home: &Path, j: &Value) -> Result<()> {
    let Some(r) = read_receipt(j)? else {
        return Ok(());
    };
    ensure!(
        !r.revoked
            && r.binding.package == *current_record(j)
            && j["launch_nonce"] == r.binding.nonce,
        "Cleanup entry does not match the current startup"
    );
    identity::publish_entry(home, &r.binding)
}

pub(super) fn start(home: &Path, j: &mut Value, target: Target) -> Result<()> {
    ensure_new_start(j)?;
    if let Some(prior) = read_receipt(j)? {
        ensure!(
            prior.exit_before_switch.is_some(),
            "Prior manager launch remains uncertain; no redispatch"
        );
        let history = j
            .as_object_mut()
            .context("Invalid journal")?
            .entry("launch_cleanup_history")
            .or_insert_with(|| json!([]));
        history
            .as_array_mut()
            .context("Invalid retained launch history")?
            .push(serde_json::to_value(prior)?);
    }
    let binding = Binding {
        installation_id: j["installation_id"]
            .as_str()
            .context("Missing installation")?
            .into(),
        transaction_id: j["transaction_id"]
            .as_str()
            .context("Missing transaction")?
            .into(),
        nonce: j["launch_nonce"]
            .as_str()
            .context("Missing launch nonce")?
            .into(),
        package: current_record(j).clone(),
    };
    binding.validate()?;
    let mut r = Receipt {
        schema: 1,
        binding,
        target,
        start: Ack::Intent,
        authority: None,
        authority_source: None,
        revoked: false,
        stop: None,
        exit_before_switch: None,
        uncertain_observation: None,
    };
    // The profile may not exist yet for an independent replacement. Its existing
    // launch module durably binds registration and Start to the fixed attempt.
    let before = match &r.target {
        Target::Startup { snapshot } => {
            native_startup_state::validate(snapshot)?;
            let observed = installation::snapshot(&r.target.service()?)?;
            r.target.validate_profile(&observed)?;
            ensure!(
                observed["active"] == false && observed["transitioning"] == false,
                "Existing or transitioning manager generation is not a new launch"
            );
            Some(observed)
        }
        Target::Replacement { .. } => None,
    };
    store(home, j, &r)?; // Intent precedes any manager dispatch.
    crate::native_update_clock::check()?;
    r.start = match &r.target {
        Target::Startup { .. } => Ack::from_result(installation::request_state(
            &r.target.service()?,
            ServiceAction::Start,
            before.as_ref().unwrap(),
        )),
        Target::Replacement { prepared } => match crate::native_update_launch::launch(prepared) {
            Ok(crate::native_update_launch::LaunchReceipt::Accepted) => Ack::Accepted,
            Ok(crate::native_update_launch::LaunchReceipt::Refused { code }) => {
                Ack::Refused { code }
            }
            Ok(crate::native_update_launch::LaunchReceipt::Unknown { code }) => {
                Ack::Unknown { code }
            }
            Err(_) => Ack::Unknown {
                code: "replacement_start_unconfirmed".into(),
            },
        },
    };
    store(home, j, &r)?;
    ensure!(
        r.start == Ack::Accepted,
        "Manager Start is not confirmed; retained intent grants no cleanup or redispatch"
    );
    crate::native_update_clock::check()
}

/// Observe only. Missing entry is an ordinary startup-in-progress observation;
/// malformed/stale entry or failed native proof is never normalized to absence.
pub(super) fn capture(home: &Path, j: &mut Value) -> Result<bool> {
    let Some(mut r) = read_receipt(j)? else {
        return Ok(true);
    };
    ensure!(
        !r.revoked && r.start == Ack::Accepted && j["launch_nonce"] == r.binding.nonce,
        "Launch acceptance or nonce is uncertain"
    );
    if r.authority.is_some() {
        return Ok(true);
    }
    let path = identity::entry_path(home, &r.binding)?;
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    crate::native_update_clock::check()?;
    let entry = identity::read_entry(home, &r.binding)?;
    let service = r.target.service()?;
    let native = installation::snapshot(&service)?;
    r.target.validate_profile(&native)?;
    r.authority = Some(identity::capture(&r.binding, &service, &native, &entry)?);
    r.authority_source = Some(AuthoritySource::ProcessEntry);
    store(home, j, &r)?;
    crate::native_update_clock::check()?;
    Ok(true)
}

fn validate_status_proof(
    proof: &StatusProof,
    binding: &Binding,
    restore_started: bool,
) -> Result<()> {
    ensure!(
        proof.id == binding.installation_id
            && proof.binary_sha256 == binding.package["binary_sha256"]
            && proof.transaction_id == binding.transaction_id
            && proof.launch_nonce == binding.nonce,
        "Authenticated status belongs to another launch or package"
    );
    ensure!(
        proof.pid > 0 && (16..=256).contains(&proof.process_nonce.len()),
        "Authenticated status lacks application identity"
    );
    ensure!(
        proof.admission_closed || restore_started,
        "Authenticated status does not retain the existing admission boundary"
    );
    ensure!(
        proof.process_birth.is_none_or(|birth| birth > 0),
        "Invalid authenticated process birth"
    );
    Ok(())
}

fn status_proof(status: &Value, binding: &Binding, restore_started: bool) -> Result<StatusProof> {
    let text = |field: &str| -> Result<String> {
        Ok(status[field]
            .as_str()
            .context("Missing authenticated status binding")?
            .into())
    };
    let proof = StatusProof {
        id: text("id")?,
        binary_sha256: text("binary_sha256")?,
        transaction_id: text("transaction_id")?,
        launch_nonce: text("launch_nonce")?,
        pid: status["pid"]
            .as_u64()
            .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64)
            .context("Invalid authenticated status PID")? as u32,
        process_nonce: text("process_nonce")?,
        admission_closed: status["admission"]["closed"]
            .as_bool()
            .context("Missing authenticated admission state")?,
        process_birth: status
            .get("process_birth")
            .map(|value| {
                value
                    .as_u64()
                    .context("Malformed authenticated process birth")
            })
            .transpose()?,
    };
    validate_status_proof(&proof, binding, restore_started)?;
    Ok(proof)
}

fn validate_status_pair(
    first: &StatusProof,
    confirmed: &StatusProof,
    application: &Value,
) -> Result<()> {
    ensure!(
        first.pid == confirmed.pid
            && first.process_nonce == confirmed.process_nonce
            && first.process_birth == confirmed.process_birth
            && application["pid"] == first.pid,
        "Authenticated application changed between status observations"
    );
    // Older Windows status already reports this actual process FILETIME. Keep
    // that evidence distinct from a scheduler engine identity.
    if let Some(birth) = application.get("creation_time_100ns") {
        let birth: u64 = birth
            .as_str()
            .context("Invalid native process birth")?
            .parse()?;
        ensure!(
            first.process_birth == Some(birth),
            "Authenticated and native process birth differ"
        );
    }
    Ok(())
}

fn validate_status_generation(application: &Value, current: Option<&Value>) -> Result<()> {
    ensure!(
        current == Some(application),
        "Application generation changed during authenticated status"
    );
    Ok(())
}

fn status_capture_receipt(j: &Value) -> Result<Receipt> {
    let r = read_receipt(j)?.context("No accepted manager launch for status capture")?;
    ensure!(
        !r.revoked
            && r.start == Ack::Accepted
            && r.stop.is_none()
            && r.exit_before_switch.is_none()
            && r.authority.is_none()
            && j["launch_nonce"] == r.binding.nonce
            && *current_record(j) == r.binding.package
            && matches!(j["phase"].as_str(), Some("verifying" | "restoring")),
        "Authenticated status cannot promote an uncertain or superseded launch"
    );
    Ok(r)
}

fn optional_entry(home: &Path, binding: &Binding) -> Result<Option<identity::Entry>> {
    let path = identity::entry_path(home, binding)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => identity::read_entry(home, binding).map(Some),
    }
}

/// Compatibility for an older, healthy application with no early publisher.
/// Both status reads use the existing authenticated local API. A native process
/// observation brackets the second response, so an earlier response's recycled
/// numeric PID cannot become authority over a later application generation.
/// No entry file, Start, session, prompt, or health claim is manufactured here.
pub(super) async fn capture_authenticated_status(
    home: &Path,
    j: &mut Value,
    first: &Value,
) -> Result<Value> {
    capture_authenticated_status_with(home, j, first, || local_api(home, "GET", "status", None))
        .await
}

async fn capture_authenticated_status_with<F: std::future::Future<Output = Result<Value>>>(
    home: &Path,
    j: &mut Value,
    first: &Value,
    confirm: impl FnOnce() -> F,
) -> Result<Value> {
    let mut r = status_capture_receipt(j)?;
    let restoring = j["restore_started"] == true;
    let first_proof = status_proof(first, &r.binding, restoring)?;
    crate::native_update_clock::check()?;
    let initial_entry = optional_entry(home, &r.binding)?;
    let application =
        process::identity(first_proof.pid)?.context("Authenticated application is absent")?;
    if let Some(entry) = &initial_entry {
        ensure!(
            entry.application == application,
            "Present process entry differs from authenticated application"
        );
    }
    let confirmed = confirm().await?;
    crate::native_update_clock::check()?;
    let confirmed_proof = status_proof(&confirmed, &r.binding, restoring)?;
    validate_status_pair(&first_proof, &confirmed_proof, &application)?;
    let current_application = process::identity(first_proof.pid)?;
    validate_status_generation(&application, current_application.as_ref())?;
    // A publisher may have reached entry after the caller's earlier absent
    // observation. Prefer its actual evidence; malformed present evidence is
    // never treated as permission to use the compatibility path.
    let final_entry = optional_entry(home, &r.binding)?;
    if let Some(initial) = &initial_entry {
        ensure!(
            final_entry.as_ref() == Some(initial),
            "Process entry changed during status capture"
        );
    }
    let service = r.target.service()?;
    crate::native_update_clock::check()?;
    let native = installation::snapshot(&service)?;
    r.target.validate_profile(&native)?;
    crate::native_update_clock::check()?;
    let (authority, source) = if let Some(entry) = final_entry {
        ensure!(
            entry.application == application,
            "Present process entry differs from authenticated generation"
        );
        (
            identity::capture(&r.binding, &service, &native, &entry)?,
            AuthoritySource::ProcessEntry,
        )
    } else {
        (
            identity::capture_process(&r.binding, &service, &native, &application)?,
            AuthoritySource::AuthenticatedStatus {
                first_status: Box::new(first_proof),
                confirmed_status: Box::new(confirmed_proof),
                restore_started: restoring,
            },
        )
    };
    crate::native_update_clock::check()?;
    r.authority = Some(authority);
    r.authority_source = Some(source);
    store(home, j, &r)?;
    crate::native_update_clock::check()?;
    Ok(confirmed)
}

/// Preserve the exact revoked attempt before clearing the public startup nonce.
/// Called as part of the existing switching journal publication, not a new gate.
pub(super) fn revoke(j: &mut Value) -> Result<()> {
    if let Some(mut r) = read_receipt(j)? {
        ensure!(
            (r.revoked && j["launch_nonce"].is_null())
                || (!r.revoked && j["launch_nonce"] == r.binding.nonce),
            "Another launch superseded cleanup"
        );
        r.revoked = true;
        j["launch_cleanup"] = serde_json::to_value(r)?;
    }
    j["launch_nonce"] = Value::Null;
    Ok(())
}

/// systemctl renders ExecStart as one command followed by execution results.
/// Preserve the command and accepted execution identity; a successful Stop may
/// change known terminal fields or clear the entire execution record after unit
/// collection. The latter also requires an independently settled native snapshot.
/// Unknown/multiple renderer records are never stripped from retained evidence.
#[derive(Debug, PartialEq, Eq)]
struct SystemdExecIdentity<'a> {
    path: &'a str,
    argv: &'a str,
    ignore_errors: &'a str,
    start_time: &'a str,
    stop_time: &'a str,
    pid: u32,
    code: &'a str,
    status: &'a str,
}

impl SystemdExecIdentity<'_> {
    fn cleared(&self) -> bool {
        self.start_time == "[n/a]"
            && self.stop_time == "[n/a]"
            && self.pid == 0
            && self.code == "(null)"
            && self.status == "0/0"
    }
}

fn systemd_exec_identity(value: &Value) -> Result<SystemdExecIdentity<'_>> {
    let text = value.as_str().context("Missing effective ExecStart")?;
    ensure!(
        !text.chars().any(char::is_control),
        "Malformed effective ExecStart"
    );
    let record = text
        .strip_prefix("{ ")
        .and_then(|s| s.strip_suffix(" }"))
        .context("Malformed effective ExecStart record")?;
    let fields: Vec<_> = record.split(" ; ").collect();
    ensure!(
        fields.len() == 8,
        "Unknown or multiple effective ExecStart records"
    );
    let field = |index: usize, name: &str| -> Result<&str> {
        fields[index]
            .strip_prefix(name)
            .filter(|s| !s.is_empty())
            .context("Malformed effective ExecStart field")
    };
    let path = field(0, "path=")?;
    let argv = field(1, "argv[]=")?;
    let ignore_errors = field(2, "ignore_errors=")?;
    ensure!(
        // This is systemd's POSIX rendering, even when reviewed on Windows.
        path.starts_with('/')
            && (argv == path || argv.strip_prefix(path).is_some_and(|s| s.starts_with(' ')))
            && matches!(ignore_errors, "no" | "yes"),
        "Malformed effective ExecStart command"
    );
    let timestamp = |value: &str| -> Result<()> {
        let inner = value
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .context("Malformed effective ExecStart timestamp")?;
        ensure!(
            !inner.is_empty() && !inner.contains(['[', ']']),
            "Malformed effective ExecStart timestamp"
        );
        Ok(())
    };
    let start_time = field(3, "start_time=")?;
    let stop_time = field(4, "stop_time=")?;
    timestamp(start_time)?;
    timestamp(stop_time)?;
    let pid_text = field(5, "pid=")?;
    let pid: u32 = pid_text
        .parse()
        .context("Malformed effective ExecStart PID")?;
    ensure!(
        pid.to_string() == pid_text,
        "Malformed effective ExecStart PID"
    );
    let code = field(6, "code=")?;
    ensure!(
        matches!(
            code,
            "(null)" | "exited" | "killed" | "dumped" | "trapped" | "stopped" | "continued"
        ),
        "Unknown effective ExecStart result code"
    );
    let full_status = field(7, "status=")?;
    let (status, name) = full_status
        .split_once('/')
        .context("Malformed effective ExecStart status")?;
    let number: u8 = status
        .parse()
        .context("Malformed effective ExecStart status number")?;
    ensure!(
        number.to_string() == status
            && !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"_+-".contains(&b)),
        "Malformed effective ExecStart status name"
    );
    let result = SystemdExecIdentity {
        path,
        argv,
        ignore_errors,
        start_time,
        stop_time,
        pid,
        code,
        status: full_status,
    };
    ensure!(
        (pid > 0 && start_time != "[n/a]") || result.cleared(),
        "Incomplete effective ExecStart generation or cleared execution record"
    );
    Ok(result)
}

fn unchanged_native_fields(before: &Value, after: &Value, dynamic: &[&str]) -> Result<()> {
    let previous = before.as_object().context("Missing native policy map")?;
    let current = after.as_object().context("Missing native policy map")?;
    for key in previous.keys().chain(current.keys()) {
        if !dynamic.contains(&key.as_str()) {
            ensure!(
                previous.get(key) == current.get(key),
                "Native service property changed: {key}"
            );
        }
    }
    Ok(())
}

fn task_native_fields(native: &Value) -> Result<&Vec<Value>> {
    for field in ["path", "xml", "owner_sid", "security_descriptor"] {
        ensure!(
            native[field].as_str().is_some_and(|s| !s.is_empty()),
            "Malformed native task field: {field}"
        );
    }
    ensure!(
        native["enabled"].is_boolean()
            && native["logon_type"].as_i64().is_some()
            && native["actions"].is_array(),
        "Malformed native task policy"
    );
    ensure!(
        native["state"]
            .as_u64()
            .is_some_and(|state| (1..=4).contains(&state)),
        "Unknown or malformed native task state"
    );
    native["instances"]
        .as_array()
        .context("Missing native task instances")
}

fn task_instance_fields(instance: &Value) -> Result<(&str, u64)> {
    let guid = instance["instance_guid"]
        .as_str()
        .context("Missing native task GUID")?;
    ensure!(
        !uuid::Uuid::parse_str(guid)?.is_nil(),
        "Invalid native task GUID"
    );
    let pid = instance["engine_pid"]
        .as_u64()
        .filter(|pid| *pid <= u32::MAX as u64)
        .context("Invalid native task engine PID")?;
    ensure!(
        instance["state"]
            .as_u64()
            .is_some_and(|state| (1..=4).contains(&state))
            && instance["current_action"].is_string()
            && instance["engine_instance_guids"].is_array(),
        "Malformed native task instance"
    );
    Ok((guid, pid))
}

fn same_task_policy(before: &Value, after: &Value) -> Result<()> {
    let expected = task_native_fields(before)?;
    let current = task_native_fields(after)?;
    ensure!(
        before["state"] == 4 && expected.len() == 1 && current.len() <= 1,
        "Native task invocation is not uniquely retained"
    );
    unchanged_native_fields(before, after, &["state", "instances"])?;
    let (expected_guid, expected_pid) = task_instance_fields(&expected[0])?;
    ensure!(
        expected_pid > 0 && expected[0]["engine_instance_guids"] == json!([expected_guid]),
        "Accepted task engine association is not unique"
    );
    for instance in current {
        let (guid, pid) = task_instance_fields(instance)?;
        ensure!(
            guid == expected_guid && (pid == expected_pid || pid == 0),
            "A different task invocation appeared after Stop"
        );
        let associations = instance["engine_instance_guids"].as_array().unwrap();
        ensure!(
            (pid == 0 && associations.is_empty())
                || (associations.len() == 1 && associations[0] == expected_guid),
            "Task engine association changed after Stop"
        );
        unchanged_native_fields(
            &expected[0],
            instance,
            &["state", "engine_pid", "engine_instance_guids"],
        )?;
    }
    Ok(())
}

fn same_launch_agent_policy(before: &Value, after: &Value) -> Result<()> {
    for snapshot in [before, after] {
        ensure!(
            snapshot["loaded"].is_boolean()
                && snapshot["native"]["domain"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
            "Malformed native LaunchAgent domain or loaded state"
        );
        let disabled = snapshot["native"]
            .get("disabled_override")
            .context("Missing native disable override")?;
        ensure!(
            disabled.is_null() || disabled.is_boolean(),
            "Malformed native disable override"
        );
        let properties = snapshot["native"]["properties"]
            .as_object()
            .context("Missing native LaunchAgent properties")?;
        ensure!(
            properties.values().all(Value::is_string),
            "Malformed native LaunchAgent property"
        );
        if snapshot["loaded"] == true {
            ensure!(
                properties.get("state").is_some_and(Value::is_string)
                    && properties.get("path").is_some_and(Value::is_string),
                "Loaded LaunchAgent lacks effective state or path"
            );
        } else {
            ensure!(
                properties.is_empty(),
                "Unloaded LaunchAgent retained unconfirmed properties"
            );
        }
    }
    ensure!(
        before["loaded"] == true,
        "Accepted LaunchAgent is not loaded"
    );
    unchanged_native_fields(&before["native"], &after["native"], &["properties"])?;
    if after["loaded"] == true {
        // Accepted shared observer already excludes counters and retains loaded
        // command/environment/policy. Only state and PID are termination fields.
        unchanged_native_fields(
            &before["native"]["properties"],
            &after["native"]["properties"],
            &["state", "pid"],
        )?;
    }
    Ok(())
}

fn native_instances_settled(snapshot: &Value) -> Result<bool> {
    if snapshot["service"]["manager"] == "scheduled_task" {
        return Ok(task_native_fields(&snapshot["native"])?.is_empty()
            && matches!(snapshot["native"]["state"].as_u64(), Some(1 | 3)));
    }
    Ok(true)
}

fn same_policy(before: &Value, after: &Value) -> Result<()> {
    for field in [
        "schema",
        "service",
        "definition",
        "effective_definition",
        "definition_matches",
        "enabled",
        "start_allowed",
        "masked",
    ] {
        ensure!(
            before.get(field).is_some()
                && after.get(field).is_some()
                && before[field] == after[field],
            "Cleanup policy changed: {field}"
        );
    }
    // Effective systemd properties need not be represented only by the file.
    if before["service"]["manager"] == "systemd_user" {
        for field in [
            "Id",
            "Names",
            "LoadState",
            "UnitFileState",
            "FragmentPath",
            "SourcePath",
            "DropInPaths",
            "NeedDaemonReload",
            "RefuseManualStart",
            "RefuseManualStop",
            "Restart",
            "KillMode",
            "Type",
        ] {
            ensure!(
                before["native"][field].is_string()
                    && after["native"][field].is_string()
                    && before["native"][field] == after["native"][field],
                "Effective service policy changed: {field}"
            );
        }
        let previous_exec = systemd_exec_identity(&before["native"]["ExecStart"])?;
        let current_exec = systemd_exec_identity(&after["native"]["ExecStart"])?;
        ensure!(
            previous_exec.pid > 0 && previous_exec.start_time != "[n/a]",
            "Missing accepted effective ExecStart generation"
        );
        ensure!(
            previous_exec.path == current_exec.path
                && previous_exec.argv == current_exec.argv
                && previous_exec.ignore_errors == current_exec.ignore_errors,
            "Effective ExecStart command changed"
        );
        if current_exec.cleared() {
            // A stopped no-autostart unit may be collected and lose all five
            // execution fields. This is not evidence of process exit: the stop
            // loop still checks the captured application generation and lock.
            ensure!(
                after["active"] == false
                    && after["transitioning"] == false
                    && after["processes"].as_array().is_some_and(Vec::is_empty)
                    && after["native"]["ActiveState"] == "inactive"
                    && after["native"]["SubState"] == "dead"
                    && after["native"]["MainPID"] == "0"
                    && after["native"]["ControlPID"] == "0"
                    && after["native"]["InvocationID"] == "",
                "Cleared effective ExecStart lacks a settled native observation"
            );
        } else {
            ensure!(
                previous_exec.start_time == current_exec.start_time
                    && previous_exec.pid == current_exec.pid,
                "Effective ExecStart accepted generation changed"
            );
        }
        // ExecStop is requested by the shared observer, but systemd omits it
        // when empty. Preserve it if present, as well as any future native
        // properties, instead of manufacturing a required empty string.
        let previous_native = before["native"]
            .as_object()
            .context("Missing native policy")?;
        let current_native = after["native"]
            .as_object()
            .context("Missing native policy")?;
        for key in previous_native.keys().chain(current_native.keys()) {
            if !matches!(
                key.as_str(),
                "ExecStart"
                    | "InvocationID"
                    | "ActiveState"
                    | "SubState"
                    | "MainPID"
                    | "ControlPID"
            ) {
                ensure!(
                    previous_native.get(key) == current_native.get(key),
                    "Native service property changed: {key}"
                );
            }
        }
        for native in [previous_native, current_native] {
            if let Some(stop) = native.get("ExecStop") {
                ensure!(stop.is_string(), "Malformed effective ExecStop");
            }
        }
        let invocation = after["native"]["InvocationID"]
            .as_str()
            .context("Missing native invocation observation")?;
        ensure!(
            before["native"]["InvocationID"].is_string(),
            "Missing accepted native invocation"
        );
        if !invocation.is_empty() {
            ensure!(
                before["native"]["InvocationID"] == after["native"]["InvocationID"],
                "A different native invocation replaced the accepted launch"
            );
        }
    } else if before["service"]["manager"] == "scheduled_task" {
        same_task_policy(&before["native"], &after["native"])?;
    } else if before["service"]["manager"] == "launch_agent" {
        same_launch_agent_policy(before, after)?;
    }
    Ok(())
}

fn observe_uncertain(home: &Path, j: &mut Value, r: &mut Receipt) -> Result<()> {
    crate::native_update_clock::check()?;
    // Observation cannot manufacture acknowledgement or authorize redispatch.
    r.uncertain_observation = Some(
        match r.target.service().and_then(|s| installation::snapshot(&s)) {
            Ok(snapshot) => json!({"status":"observed","native_snapshot":snapshot,
            "start_acknowledgement":r.start,"stop_acknowledgement":r.stop}),
            Err(_) => json!({"status":"unavailable","start_acknowledgement":r.start,
            "stop_acknowledgement":r.stop}),
        },
    );
    store(home, j, r)
}

/// A retained revoked attempt must be reconciled even when service.lock is free.
/// Missing authority/unknown acknowledgements cannot become permission to swap.
pub(super) async fn stop(home: &Path, j: &mut Value) -> Result<Option<InstallationLock>> {
    let Some(mut r) = read_receipt(j)? else {
        return Ok(None);
    };
    if !r.revoked {
        return Ok(None);
    }
    ensure!(
        j["phase"] == "switching" && j["launch_nonce"].is_null() && j["restore_started"] != true,
        "Cleanup is not a revoked pre-restoration attempt"
    );
    let admission = update_admission::status(home)?;
    ensure!(
        admission["closed"] == true
            && admission["ready"] == true
            && admission["transaction_id"] == r.binding.transaction_id,
        "Cleanup does not own the closed admission boundary"
    );
    if r.start != Ack::Accepted {
        observe_uncertain(home, j, &mut r)?;
        anyhow::bail!(
            "Unknown Start grants no cleanup authority; observation retained without redispatch"
        );
    }
    let authority = r
        .authority
        .as_ref()
        .context("No exact accepted application generation was captured")?
        .clone();
    if r.exit_before_switch.is_none() {
        ensure!(
            installed(home)?["package"] == r.binding.package,
            "Installed package changed before cleanup"
        );
    }
    if r.exit_before_switch.is_some() {
        ensure!(
            r.stop == Some(Ack::Accepted),
            "Retained Stop acknowledgement is uncertain"
        );
    } else if r.stop.is_none() {
        crate::native_update_clock::check()?;
        let current = installation::snapshot(&authority.service)?;
        r.target.validate_profile(&current)?;
        identity::validate(&authority, &r.binding, &current)?;
        r.stop = Some(Ack::Intent);
        store(home, j, &r)?; // A lost reply becomes observation-only, never Stop again.
        let result = installation::request_state_checked(
            &authority.service,
            ServiceAction::Stop,
            &authority.native_snapshot,
            |native| {
                let current_admission = update_admission::status(home)?;
                ensure!(
                    current_admission["closed"] == true
                        && current_admission["ready"] == true
                        && current_admission["transaction_id"] == r.binding.transaction_id,
                    "Application work has not settled for stop-only cleanup"
                );
                identity::validate(&authority, &r.binding, native)?;
                Ok(None)
            },
        );
        r.stop = Some(Ack::from_result(result));
        store(home, j, &r)?;
    }
    if r.stop != Some(Ack::Accepted) {
        observe_uncertain(home, j, &mut r)?;
        anyhow::bail!("Stop acknowledgement is uncertain; observation retained without redispatch");
    }
    let deadline =
        std::time::Instant::now() + crate::native_update_clock::cap(Duration::from_secs(15))?;
    loop {
        crate::native_update_clock::check()?;
        let current = installation::snapshot(&authority.service)?;
        r.target.validate_profile(&current)?;
        same_policy(&authority.native_snapshot, &current)?;
        let expected_processes = authority.native_snapshot["processes"]
            .as_array()
            .context("Missing accepted processes")?;
        ensure!(
            current["processes"]
                .as_array()
                .context("Missing current processes")?
                .iter()
                .all(|p| expected_processes.contains(p)),
            "A different manager generation appeared after Stop"
        );
        if current["active"] == false
            && current["transitioning"] == false
            && current["processes"].as_array().is_some_and(Vec::is_empty)
            && native_instances_settled(&current)?
            && !process::matches(&authority.application)?
            && let Ok(service) = lock(home, "service.lock")
        {
            if let Some(saved) = &r.exit_before_switch {
                ensure!(
                    saved.native_snapshot == current,
                    "Stopped manager changed after cleanup"
                );
            } else {
                let binary_sha256 = digest(&binary(home))?;
                ensure!(
                    binary_sha256 == r.binding.package["binary_sha256"],
                    "Binary changed before confirmed exit"
                );
                r.exit_before_switch = Some(ExitObservation {
                    application_exited: true,
                    service_lock_acquired: true,
                    native_snapshot: current,
                    binary_sha256,
                    cleanup_is_health: false,
                });
                store(home, j, &r)?;
            }
            return Ok(Some(service));
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "Exact application exit remains unknown; no files switched"
        );
        tokio::time::sleep(Duration::from_millis(75)).await;
    }
}

#[cfg(test)]
#[path = "native_update_cleanup_tests.rs"]
mod tests;
