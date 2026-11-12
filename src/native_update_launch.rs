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
    registration::{self, Registration, RegistrationOutcome},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
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
    receipt: Option<LaunchReceipt>,
}

/// Private manager evidence, including the complete definition and process
/// generation. Do not expose this snapshot or its arguments to the browser.
#[derive(Clone, Debug)]
pub struct LaunchObservation {
    pub receipt: Option<LaunchReceipt>,
    pub profile_matches: bool,
    pub snapshot: Value,
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
        Ok(_) => ordinary(&path, false)?,
    }
    ensure!(
        fs::metadata(&path)?.len() <= 128 * 1024,
        "Launch receipt exceeds observation limit"
    );
    let record: DispatchRecord = serde_json::from_slice(&fs::read(&path)?)?;
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
    plan.validate()?;
    if read_dispatch(plan)?.is_some() {
        return Ok(unknown("dispatch_already_attempted"));
    }
    let mut record = DispatchRecord {
        prepared: plan.clone(),
        receipt: None,
    };
    // Create-only publication wins races. A failed flush can follow publication;
    // therefore any error here remains uncertain and never enters the manager.
    if amplifier_native_platform::private_create(&plan.marker(), &serde_json::to_vec(&record)?)
        .is_err()
    {
        return Ok(unknown("dispatch_intent_unconfirmed"));
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
        atomic::{AtomicUsize, Ordering},
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
        fn register(&self, _: &PreparedLaunch) -> Result<RegistrationOutcome> {
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
        let record = DispatchRecord {
            prepared: plan.clone(),
            receipt: None,
        };
        amplifier_native_platform::private_create(
            &plan.marker(),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
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
}
