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
struct Receipt {
    schema: u32,
    binding: Binding,
    target: Target,
    start: Ack,
    authority: Option<StopAuthority>,
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
                    .is_some_and(Vec::is_empty),
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
    store(home, j, &r)?;
    crate::native_update_clock::check()?;
    Ok(true)
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
            "UnitFileState",
            "FragmentPath",
            "SourcePath",
            "DropInPaths",
            "NeedDaemonReload",
            "RefuseManualStart",
            "RefuseManualStop",
            "ExecStart",
            "ExecStop",
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
