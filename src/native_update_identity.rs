//! Process-entry evidence for stop-only recovery before authenticated status.
//!
//! The caller owns the update journal, Start/Stop intent and acknowledgement,
//! startup revocation, policy validation, and verified restoration. Nothing in
//! this module dispatches a manager action or treats process entry as health.
use crate::config::Config;
use amplifier_native_platform::{
    installation::{Service, ServiceManager},
    process,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const RECORD_LIMIT: usize = 128 * 1024;
const MAX_ANCESTRY: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub installation_id: String,
    pub transaction_id: String,
    pub nonce: String,
    pub package: Value,
}

impl Binding {
    pub fn validate(&self) -> Result<()> {
        for id in [&self.installation_id, &self.transaction_id] {
            ensure!(
                uuid::Uuid::parse_str(id)?.to_string() == *id,
                "Invalid launch identity"
            );
        }
        ensure!(
            !self.nonce.is_empty()
                && self.nonce.len() <= 256
                && self
                    .nonce
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
            "Invalid launch nonce"
        );
        ensure!(self.package.is_object(), "Missing launch package");
        for value in [
            &self.package["binary_sha256"],
            &self.package["bundle"]["manifest_sha256"],
        ] {
            let value = value.as_str().context("Missing launch package digest")?;
            ensure!(
                value.len() == 64
                    && value
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "Invalid launch package digest"
            );
        }
        ensure!(
            serde_json::to_vec(self)?.len() <= RECORD_LIMIT / 2,
            "Launch binding exceeds its bound"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub schema: u32,
    pub binding: Binding,
    pub application: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StopAuthority {
    pub schema: u32,
    pub binding: Binding,
    pub service: Service,
    pub native_snapshot: Value,
    pub application: Value,
    /// Application first, manager process last. Windows keeps the engine and
    /// application identities distinct even when native evidence shows equality.
    pub ancestry: Vec<Value>,
}

fn owned_home(home: &Path, binding: &Binding) -> Result<()> {
    binding.validate()?;
    ensure!(home.is_absolute(), "Use an absolute installation home");
    amplifier_native_platform::validate_private_directory(home)?;
    ensure!(
        Config::load(home)?.id == binding.installation_id,
        "Launch installation changed"
    );
    Ok(())
}

pub fn entry_path(home: &Path, binding: &Binding) -> Result<PathBuf> {
    owned_home(home, binding)?;
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(binding)?));
    Ok(home.join(format!("native-update-entry-{digest}.json")))
}

fn read_record(path: &Path) -> Result<Entry> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Launch entry must be an ordinary file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
            "Launch entry is not private current-user storage"
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Reparse launch entry is not supported"
        );
    }
    ensure!(
        metadata.len() <= RECORD_LIMIT as u64,
        "Launch entry exceeds its bound"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(RECORD_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= RECORD_LIMIT,
        "Launch entry exceeds its bound"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

/// Called only while retaining the existing final startup-validation guard,
/// after the selected executable hash has matched the journal. This does not
/// repeat executable verification or grant permission to open AppState.
pub fn publish_entry(home: &Path, binding: &Binding) -> Result<()> {
    let path = entry_path(home, binding)?;
    let entry = Entry {
        schema: 1,
        binding: binding.clone(),
        application: process::identity(std::process::id())?
            .context("Cannot identify entering application")?,
    };
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            ensure!(
                read_record(&path)? == entry,
                "Another process already entered this launch attempt"
            );
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // A failed create/flush stays an error even when publication may have
    // occurred. No overwrite or create retry follows an uncertain result.
    amplifier_native_platform::private_create(&path, &serde_json::to_vec(&entry)?)
        .context("Cannot publish process-entry evidence")
}

pub fn read_entry(home: &Path, binding: &Binding) -> Result<Entry> {
    let entry = read_record(&entry_path(home, binding)?)?;
    ensure!(
        entry.schema == 1 && entry.binding == *binding,
        "Launch entry belongs to another attempt"
    );
    Ok(entry)
}

trait Processes {
    fn matches(&self, expected: &Value) -> Result<bool>;
    fn parent(&self, child: &Value) -> Result<Option<Value>>;
}
struct NativeProcesses;
impl Processes for NativeProcesses {
    fn matches(&self, expected: &Value) -> Result<bool> {
        process::matches(expected)
    }
    fn parent(&self, child: &Value) -> Result<Option<Value>> {
        process::parent_identity(child)
    }
}

fn pid(value: &Value) -> Result<u32> {
    value["pid"]
        .as_u64()
        .filter(|p| *p > 0 && *p <= u32::MAX as u64)
        .map(|p| p as u32)
        .context("Missing process identity PID")
}

fn manager_identity<'a>(service: &Service, native: &'a Value) -> Result<&'a Value> {
    ensure!(
        native["schema"] == 1 && native["service"] == serde_json::to_value(service)?,
        "Native service identity differs"
    );
    ensure!(
        native["definition_matches"] == true
            && native["active"] == true
            && native["transitioning"] == false
            && native["loaded"] == true
            && native["masked"] == false
            && native["start_allowed"] == true,
        "Native generation or policy is not confirmed"
    );
    let processes = native["processes"]
        .as_array()
        .context("Missing native process identities")?;
    ensure!(
        processes.len() == 1,
        "Native process ownership is ambiguous"
    );
    let manager = &processes[0];
    let manager_pid = pid(&manager["identity"])?;
    match service.manager {
        ServiceManager::SystemdUser => {
            ensure!(
                manager["role"] == "MainPID"
                    && native["native"]["Id"] == service.name
                    && native["native"]["MainPID"] == manager_pid.to_string()
                    && native["native"]["ControlPID"] == "0",
                "Systemd application generation is not confirmed"
            );
            let invocation = native["native"]["InvocationID"]
                .as_str()
                .context("Missing systemd invocation")?;
            ensure!(
                invocation.len() == 32
                    && invocation.bytes().all(|b| b.is_ascii_hexdigit())
                    && invocation.bytes().any(|b| b != b'0'),
                "Invalid systemd invocation"
            );
        }
        ServiceManager::LaunchAgent => {
            ensure!(
                manager["role"] == "launchd_pid"
                    && native["native"]["properties"]["pid"] == manager_pid.to_string(),
                "LaunchAgent application generation is not confirmed"
            );
        }
        ServiceManager::ScheduledTask => {
            ensure!(
                manager["role"] == "scheduled_task_engine",
                "Missing scheduled-task engine identity"
            );
            let instances = native["native"]["instances"]
                .as_array()
                .context("Missing task instances")?;
            ensure!(instances.len() == 1, "Task instance ownership is ambiguous");
            let instance = &instances[0];
            let guid = instance["instance_guid"]
                .as_str()
                .context("Missing task instance GUID")?;
            ensure!(
                !uuid::Uuid::parse_str(guid)
                    .context("Invalid task instance GUID")?
                    .is_nil(),
                "Missing task instance GUID"
            );
            ensure!(
                instance["engine_pid"] == u64::from(manager_pid)
                    && instance["state"] == 4
                    && manager["instance_guid"] == guid,
                "Task native instance differs"
            );
            let associations = instance["engine_instance_guids"]
                .as_array()
                .context("Missing task-engine associations")?;
            ensure!(
                associations.len() == 1 && associations[0] == guid,
                "Task engine belongs to another or unknown invocation"
            );
        }
    }
    Ok(&manager["identity"])
}

fn prove_chain(
    service: &Service,
    native: &Value,
    application: &Value,
    processes: &dyn Processes,
) -> Result<Vec<Value>> {
    let manager = manager_identity(service, native)?;
    ensure!(
        processes.matches(application)? && processes.matches(manager)?,
        "Application or manager generation changed"
    );
    if service.manager != ServiceManager::ScheduledTask {
        ensure!(
            application == manager,
            "The manager PID is not the entered application"
        );
        return Ok(vec![application.clone()]);
    }
    let mut chain = vec![application.clone()];
    while chain.last() != Some(manager) {
        ensure!(
            chain.len() < MAX_ANCESTRY,
            "Application ancestry exceeds its bound"
        );
        let parent = processes
            .parent(chain.last().unwrap())?
            .context("Application has no matching manager ancestor")?;
        ensure!(
            !chain.contains(&parent),
            "Application ancestry contains a cycle"
        );
        ensure!(processes.matches(&parent)?, "Application ancestor changed");
        chain.push(parent);
    }
    // Parent observations have their own generation brackets. Recheck the full
    // retained chain too, rather than mistaking a reused engine PID for a link.
    for identity in &chain {
        ensure!(
            processes.matches(identity)?,
            "Application ancestry changed after observation"
        );
    }
    Ok(chain)
}

/// Input snapshot must come from the accepted native observer and an exact
/// profile check. The caller may retain this only after confirmed Start; a
/// successful capture is not a Start acknowledgement or an application claim.
pub fn capture(
    binding: &Binding,
    service: &Service,
    snapshot: &Value,
    entry: &Entry,
) -> Result<StopAuthority> {
    capture_with(binding, service, snapshot, entry, &NativeProcesses)
}
fn capture_with(
    binding: &Binding,
    service: &Service,
    snapshot: &Value,
    entry: &Entry,
    processes: &dyn Processes,
) -> Result<StopAuthority> {
    binding.validate()?;
    ensure!(
        entry.schema == 1 && entry.binding == *binding,
        "Application entry belongs to another attempt"
    );
    let ancestry = prove_chain(service, snapshot, &entry.application, processes)?;
    Ok(StopAuthority {
        schema: 1,
        binding: binding.clone(),
        service: service.clone(),
        native_snapshot: snapshot.clone(),
        application: entry.application.clone(),
        ancestry,
    })
}

/// Use inside request_state_checked's refusal-only callback, after the caller
/// has proved journal revocation and unchanged profile. No manager call occurs.
pub fn validate(authority: &StopAuthority, binding: &Binding, current: &Value) -> Result<()> {
    validate_with(authority, binding, current, &NativeProcesses)
}

/// Check the retained receipt's internal binding and generation-chain shape.
/// This deliberately performs no process observation: a completed exit receipt
/// refers to an old process which should no longer exist. It is not permission
/// to stop a live process; that still requires `validate` and checked dispatch.
pub fn validate_retained(authority: &StopAuthority, binding: &Binding) -> Result<()> {
    binding.validate()?;
    ensure!(
        authority.schema == 1 && authority.binding == *binding,
        "Stop evidence belongs to another launch attempt"
    );
    let manager = manager_identity(&authority.service, &authority.native_snapshot)?;
    ensure!(
        !authority.ancestry.is_empty()
            && authority.ancestry.len() <= MAX_ANCESTRY
            && authority.ancestry.first() == Some(&authority.application)
            && authority.ancestry.last() == Some(manager),
        "Retained application ancestry is incomplete or unbound"
    );
    for (index, identity) in authority.ancestry.iter().enumerate() {
        pid(identity)?;
        ensure!(
            !authority.ancestry[..index].contains(identity),
            "Retained application ancestry contains a cycle"
        );
    }
    ensure!(
        authority.service.manager == ServiceManager::ScheduledTask || authority.ancestry.len() == 1,
        "The retained native application is not its manager process"
    );
    Ok(())
}

fn validate_with(
    authority: &StopAuthority,
    binding: &Binding,
    current: &Value,
    processes: &dyn Processes,
) -> Result<()> {
    validate_retained(authority, binding)?;
    ensure!(
        current == &authority.native_snapshot,
        "Native generation, profile or policy changed; preserved"
    );
    ensure!(
        prove_chain(
            &authority.service,
            current,
            &authority.application,
            processes
        )? == authority.ancestry,
        "Application ancestry changed; preserved"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct FakeProcesses {
        identities: Vec<Value>,
        parents: BTreeMap<u32, Value>,
    }
    impl Processes for FakeProcesses {
        fn matches(&self, expected: &Value) -> Result<bool> {
            Ok(self.identities.contains(expected))
        }
        fn parent(&self, child: &Value) -> Result<Option<Value>> {
            Ok(self.parents.get(&pid(child)?).cloned())
        }
    }
    fn binding() -> Binding {
        Binding {
            installation_id: uuid::Uuid::new_v4().to_string(),
            transaction_id: uuid::Uuid::new_v4().to_string(),
            nonce: "fixed-fixture-nonce".into(),
            package: json!({"binary_sha256":"a".repeat(64), "bundle":{"manifest_sha256":"b".repeat(64)}}),
        }
    }
    fn fixture(windows: bool) -> (Binding, Service, Value, Entry, FakeProcesses) {
        let binding = binding();
        let service = Service {
            manager: if windows {
                ServiceManager::ScheduledTask
            } else {
                ServiceManager::SystemdUser
            },
            name: "fixture.service".into(),
            definition_path: None,
        };
        let app = json!({"pid":123,"generation":"app"});
        let manager = if windows {
            json!({"pid":99,"generation":"engine"})
        } else {
            app.clone()
        };
        let guid = uuid::Uuid::new_v4().to_string();
        let native = if windows {
            json!({"instances":[{"instance_guid":guid,"engine_pid":99,"state":4,"engine_instance_guids":[guid]}]})
        } else {
            json!({"Id":"fixture.service","MainPID":"123","ControlPID":"0","InvocationID":"a".repeat(32)})
        };
        let snapshot = json!({"schema":1,"service":service,"definition_matches":true,"active":true,"transitioning":false,
                              "loaded":true,"masked":false,"start_allowed":true,"definition":{"bytes":[1]},"enabled":true,
                              "processes":[{"role":if windows {"scheduled_task_engine"}else{"MainPID"},"identity":manager,"instance_guid":guid}],"native":native});
        let entry = Entry {
            schema: 1,
            binding: binding.clone(),
            application: app.clone(),
        };
        let processes = FakeProcesses {
            identities: vec![app.clone(), manager.clone()],
            parents: BTreeMap::from([(123, manager)]),
        };
        (binding, service, snapshot, entry, processes)
    }
    #[test]
    fn stop_evidence_binds_exact_generation_and_complete_profile() {
        let (b, s, n, e, p) = fixture(false);
        let authority = capture_with(&b, &s, &n, &e, &p).unwrap();
        validate_with(&authority, &b, &n, &p).unwrap();
        for key in ["definition", "enabled", "masked", "native", "processes"] {
            let mut changed = n.clone();
            changed[key] = Value::Null;
            assert!(
                validate_with(&authority, &b, &changed, &p).is_err(),
                "{key}"
            );
        }
    }
    #[test]
    fn superseded_nonce_transaction_installation_and_package_refuse() {
        let (b, s, n, e, p) = fixture(false);
        let authority = capture_with(&b, &s, &n, &e, &p).unwrap();
        for which in 0..4 {
            let mut next = b.clone();
            match which {
                0 => next.nonce = "other".into(),
                1 => next.transaction_id = uuid::Uuid::new_v4().to_string(),
                2 => next.installation_id = uuid::Uuid::new_v4().to_string(),
                _ => next.package["binary_sha256"] = json!("c".repeat(64)),
            }
            assert!(validate_with(&authority, &next, &n, &p).is_err());
            assert!(capture_with(&next, &s, &n, &e, &p).is_err());
        }
    }
    #[test]
    fn unix_manager_pid_must_be_the_application_not_a_parent() {
        let (b, s, n, mut e, p) = fixture(false);
        e.application = json!({"pid":99,"generation":"other"});
        assert!(capture_with(&b, &s, &n, &e, &p).is_err());
    }
    #[test]
    fn native_unknown_inactive_multiple_and_missing_generations_refuse() {
        let (b, s, n, e, p) = fixture(false);
        for (key, value) in [
            ("active", Value::Null),
            ("active", json!(false)),
            ("transitioning", json!(true)),
            ("definition_matches", json!(false)),
            ("loaded", json!(false)),
            ("masked", json!(true)),
            ("start_allowed", json!(false)),
            ("processes", json!([])),
            ("processes", json!([n["processes"][0], n["processes"][0]])),
        ] {
            let mut changed = n.clone();
            changed[key] = value;
            assert!(capture_with(&b, &s, &changed, &e, &p).is_err(), "{key}");
        }
        let mut changed = n;
        changed["native"]["InvocationID"] = Value::Null;
        assert!(capture_with(&b, &s, &changed, &e, &p).is_err());
    }
    #[test]
    fn windows_requires_unique_native_instance_and_distinct_application_proof() {
        let (b, s, n, e, p) = fixture(true);
        let authority = capture_with(&b, &s, &n, &e, &p).unwrap();
        assert_ne!(authority.application, n["processes"][0]["identity"]);
        assert_eq!(authority.ancestry.len(), 2);
        validate_with(&authority, &b, &n, &p).unwrap();
        for which in 0..5 {
            let mut changed = n.clone();
            match which {
                0 => changed["native"]["instances"][0]["engine_instance_guids"] = json!([]),
                1 => {
                    changed["native"]["instances"][0]["engine_instance_guids"] = json!([
                        n["native"]["instances"][0]["instance_guid"],
                        uuid::Uuid::new_v4().to_string()
                    ])
                }
                2 => {
                    changed["native"]["instances"][0]["instance_guid"] =
                        json!(uuid::Uuid::new_v4().to_string())
                }
                3 => changed["native"]["instances"][0]["state"] = Value::Null,
                _ => changed["native"]["instances"][0]["engine_pid"] = json!(123),
            }
            assert!(capture_with(&b, &s, &changed, &e, &p).is_err());
        }
    }
    #[test]
    fn changed_or_missing_actual_generation_refuses() {
        let (b, s, n, e, mut p) = fixture(true);
        let authority = capture_with(&b, &s, &n, &e, &p).unwrap();
        p.identities.retain(|v| v != &e.application);
        p.identities.push(json!({"pid":123,"generation":"reused"}));
        assert!(validate_with(&authority, &b, &n, &p).is_err());
    }
    #[test]
    fn missing_or_cyclic_windows_ancestry_refuses() {
        let (b, s, n, e, mut p) = fixture(true);
        p.parents.clear();
        assert!(capture_with(&b, &s, &n, &e, &p).is_err());
        p.parents.insert(123, e.application.clone());
        assert!(capture_with(&b, &s, &n, &e, &p).is_err());
    }
    #[test]
    fn retained_shape_is_not_a_live_process_or_stop_claim() {
        let (b, s, n, e, p) = fixture(true);
        let authority = capture_with(&b, &s, &n, &e, &p).unwrap();
        // Synthetic identities are deliberately not valid native process
        // records. Retained-shape validation must not consult live processes.
        validate_retained(&authority, &b).unwrap();
        let mut malformed = authority.clone();
        malformed.ancestry.insert(0, malformed.application.clone());
        assert!(validate_retained(&malformed, &b).is_err());
        malformed = authority;
        malformed.ancestry.clear();
        assert!(validate_retained(&malformed, &b).is_err());
    }
    #[test]
    fn malformed_binding_refuses_before_identity_observation() {
        let original = binding();
        for which in 0..5 {
            let mut b = original.clone();
            match which {
                0 => b.nonce = "../foreign".into(),
                1 => b.transaction_id = "missing".into(),
                2 => b.installation_id = "missing".into(),
                3 => b.package["binary_sha256"] = Value::Null,
                _ => b.package["bundle"]["manifest_sha256"] = json!("A".repeat(64)),
            }
            assert!(b.validate().is_err());
        }
    }
    #[test]
    fn actual_entry_is_create_only_and_same_process_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let home = dunce::canonicalize(directory.path()).unwrap();
        crate::config::private_dir(&home).unwrap();
        let config = Config::local(&home, "Entry fixture", 54127).unwrap();
        config.save(&home).unwrap();
        let mut b = binding();
        b.installation_id = config.id;
        publish_entry(&home, &b).unwrap();
        let path = entry_path(&home, &b).unwrap();
        let bytes = fs::read(&path).unwrap();
        publish_entry(&home, &b).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let entry = read_entry(&home, &b).unwrap();
        assert_eq!(
            entry.application,
            process::identity(std::process::id()).unwrap().unwrap()
        );
        let mut foreign = entry;
        foreign.application["pid"] = json!(1);
        amplifier_native_platform::private_write_data(
            &path,
            &serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        let altered = fs::read(&path).unwrap();
        assert!(publish_entry(&home, &b).is_err());
        assert_eq!(fs::read(&path).unwrap(), altered);
    }
    #[test]
    fn actual_current_process_can_bind_a_unix_native_process_entry() {
        let b = binding();
        let identity = process::identity(std::process::id()).unwrap().unwrap();
        assert!(process::matches(&identity).unwrap());
        if let Some(parent) = process::parent_identity(&identity).unwrap() {
            assert!(process::matches(&parent).unwrap());
        }
        // Synthetic manager fields, real native generation checks: this does
        // not replace the required actual OS-manager lifecycle regression.
        let s = Service {
            manager: ServiceManager::SystemdUser,
            name: "fixture.service".into(),
            definition_path: None,
        };
        let n = json!({"schema":1,"service":s,"definition_matches":true,"active":true,"transitioning":false,
            "loaded":true,"masked":false,"start_allowed":true,"processes":[{"role":"MainPID","identity":identity}],
            "native":{"Id":"fixture.service","MainPID":std::process::id().to_string(),"ControlPID":"0","InvocationID":"a".repeat(32)}});
        let e = Entry {
            schema: 1,
            binding: b.clone(),
            application: identity,
        };
        let authority = capture(&b, &s, &n, &e).unwrap();
        validate(&authority, &b, &n).unwrap();
    }
}
