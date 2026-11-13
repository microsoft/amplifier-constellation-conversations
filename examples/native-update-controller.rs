//! Disposable qualification controller; never part of the packaged application.
//! Private stdin selects only this installation's original application or a
//! fixed launch already recorded by its real updater. Original desktop startup
//! removal is a separately checked post-proof test action, never one-shot cleanup.
use agent_conversations::{
    config::Config, desktop, native_bundle, native_startup_state, native_update,
    native_update_launch,
};
use amplifier_native_platform::{
    installation::{self, ActionOutcome, Service, ServiceAction, ServiceManager},
    process,
    registration::{self, Registration},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    PrepareManaged,
    Inspect,
    ObserveRetirement,
    Stop,
    RemoveOriginal,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Original,
    Worker {
        operation: String,
        attempt: String,
    },
    Replacement {
        transaction: String,
        #[serde(default)]
        nonce: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    home: PathBuf,
    action: Action,
    target: Target,
    previous_process: Option<Value>,
    setup_bundle: Option<PathBuf>,
    #[serde(default)]
    expected_original: Option<Value>,
}

fn digest(value: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn original_service(home: &std::path::Path, id: &str) -> Result<Service> {
    let name = format!("agent-conversations-{id}");
    let user = amplifier_native_platform::user_home()?;
    let (manager, name, definition_path) = match std::env::consts::OS {
        "linux" => {
            let name = format!("{name}.service");
            (
                ServiceManager::SystemdUser,
                name.clone(),
                user.join(".config/systemd/user").join(name),
            )
        }
        "macos" => (
            ServiceManager::LaunchAgent,
            name.clone(),
            user.join("Library/LaunchAgents")
                .join(format!("{name}.plist")),
        ),
        "windows" => (
            ServiceManager::ScheduledTask,
            name.clone(),
            home.join("startup.xml"),
        ),
        _ => anyhow::bail!("Unsupported native controller host"),
    };
    Ok(Service {
        manager,
        name,
        definition_path: Some(definition_path),
    })
}

fn saved_launch(request: &Request) -> Result<Option<native_update_launch::PreparedLaunch>> {
    let mut found = None;
    let mut count = 0;
    for entry in fs::read_dir(&request.home)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("native-browser-launch-") || !name.ends_with(".json") {
            continue;
        }
        count += 1;
        ensure!(count <= 256, "Too many retained launch records");
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 1024 * 1024,
            "Launch record is not a bounded ordinary file"
        );
        let value: Value = serde_json::from_slice(&fs::read(entry.path())?)?;
        let identity = &value["prepared"]["identity"];
        let matches = match &request.target {
            Target::Worker { operation, attempt } => {
                identity["kind"] == "worker"
                    && identity["operation"] == *operation
                    && identity["attempt"] == *attempt
            }
            Target::Replacement { transaction, nonce } => {
                identity["kind"] == "replacement"
                    && identity["transaction"] == *transaction
                    && nonce
                        .as_ref()
                        .is_none_or(|expected| identity["nonce"] == *expected)
            }
            Target::Original => false,
        };
        if matches {
            ensure!(found.is_none(), "Ambiguous retained launch identity");
            ensure!(
                value["prepared"]["home"] == request.home.to_string_lossy().as_ref(),
                "Launch belongs to another installation"
            );
            found = Some(serde_json::from_value(value["prepared"].clone())?);
        }
    }
    Ok(found)
}

fn sanitized(service: &Service, snapshot: &Value, profile_matches: bool) -> Result<Value> {
    let processes = snapshot["processes"]
        .as_array()
        .context("Missing native process observations")?;
    Ok(
        json!({"status":"observed","service_name":service.name,"manager":service.manager,
        "profile_matches":profile_matches,"active":snapshot["active"],"transitioning":snapshot["transitioning"],
        "enabled":snapshot["enabled"],"masked":snapshot["masked"],"definition_matches":snapshot["definition_matches"],
        "definition_sha256":snapshot["definition"]["sha256"],
        "effective_definition_sha256":snapshot["effective_definition"]["sha256"],
        "state_sha256":digest(snapshot)?,
        // Process birth identities are private controller protocol data. The
        // Python receipt keeps their digest; no definition/argv/nonce is output.
        "processes":processes.iter().map(|p| Ok(json!({"identity":p["identity"],
            "instance_guid_sha256":if p["instance_guid"].is_null() { Value::Null } else { json!(digest(&p["instance_guid"])?) }})))
            .collect::<Result<Vec<_>>>()?}),
    )
}

fn application_binding(application: &Value, processes: &Value) -> Result<Option<&'static str>> {
    if application.is_null() {
        return Ok(None);
    }
    let identities = processes.as_array().context("Missing manager identities")?;
    if identities.iter().any(|p| p["identity"] == *application) {
        ensure!(
            process::matches(application)?,
            "Application generation changed"
        );
        return Ok(Some("same_process"));
    }
    // Tasks expose the task-engine generation. Follow native, generation-checked
    // ancestry instead of equating a bare EnginePID to the application.
    if cfg!(windows) {
        let mut current = application.clone();
        for _ in 0..8 {
            let Some(parent) = process::parent_identity(&current)? else {
                break;
            };
            if identities.iter().any(|p| p["identity"] == parent) {
                ensure!(
                    process::matches(application)?,
                    "Application generation changed"
                );
                return Ok(Some("descendant_of_task_engine"));
            }
            current = parent;
        }
    }
    Ok(None)
}

fn required_application_matches(
    expected: Option<&Value>,
    snapshot: &Value,
) -> Result<Option<ActionOutcome>> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    Ok(
        (application_binding(expected, &snapshot["processes"])?.is_none()).then(|| {
            ActionOutcome::Refused {
                code: "changed_fixture_application".into(),
                message:
                    "Captured fixture application generation is no longer bound to this manager"
                        .into(),
            }
        }),
    )
}

fn actual_worker(request: &Request, installation_id: &str) -> Result<Option<Value>> {
    let Target::Worker { operation, attempt } = &request.target else {
        return Ok(None);
    };
    let path = request.home.join("native-browser-update/control.json");
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= 4 * 1024 * 1024,
        "Worker ledger is not a bounded ordinary file"
    );
    let ledger: Value = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        ledger["installation_id"] == installation_id,
        "Worker ledger belongs to another installation"
    );
    let saved = &ledger["operations"][operation];
    ensure!(
        saved["id"] == *operation && saved["attempt"]["id"] == *attempt,
        "Worker receipt binds another operation or attempt"
    );
    let identity = &saved["attempt"]["worker_process"];
    if identity.is_null() {
        return Ok(None);
    }
    ensure!(
        saved["attempt"]["claimed"] == true,
        "Worker process was not claimed"
    );
    Ok(Some(identity.clone()))
}

fn inactive(snapshot: &Value) -> Result<()> {
    ensure!(
        snapshot["active"] == false
            && snapshot["transitioning"] == false
            && snapshot["processes"].as_array().is_some_and(Vec::is_empty),
        "Original fixture registration is active, transitioning, or unknown"
    );
    Ok(())
}

fn preference(value: &Value) -> Value {
    let mut result = serde_json::Map::new();
    for key in [
        "registered",
        "service_name",
        "manager",
        "enabled",
        "masked",
        "definition_sha256",
        "effective_definition_sha256",
        "startup_settings_sha256",
    ] {
        result.insert(key.into(), value[key].clone());
    }
    Value::Object(result)
}

fn fixture_lock(home: &std::path::Path, name: &str) -> Result<fs::File> {
    let path = home.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Fixture lock must already be ordinary"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Fixture lock cannot be a reparse point"
        );
    }
    let file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    file.try_lock_exclusive().context("Fixture remains owned")?;
    Ok(file)
}

fn removed_state(
    manager: &ServiceManager,
    definition_absent: bool,
    manager_present: bool,
    cached: Option<&Value>,
) -> Result<Value> {
    ensure!(
        definition_absent,
        "Original definition remains after removal"
    );
    if !manager_present {
        ensure!(
            cached.is_none(),
            "Absent manager must not have a fabricated snapshot"
        );
        return Ok(json!({"manager_present":false,"native_absence_proved":true,
            "inactive_after":true,"cached_native_sha256":null}));
    }
    ensure!(
        matches!(manager, ServiceManager::SystemdUser),
        "Original native registration remains after removal"
    );
    let snapshot = cached.context("Retained systemd cache needs actual native inspection")?;
    inactive(snapshot)?;
    Ok(json!({"manager_present":true,"native_absence_proved":false,
        "inactive_after":true,"cached_native_sha256":digest(snapshot)?}))
}

fn observe_original_removed(service: &Service) -> Result<Value> {
    // Native registration absence is a distinct observation from a missing
    // plist/unit file. In particular, never turn an absent Mac plist into
    // permission to infer that its loaded manager service has disappeared.
    let present = installation::manager_registration_exists(service)?;
    let definition_absent = match &service.manager {
        ServiceManager::ScheduledTask => !present,
        _ => match fs::symlink_metadata(
            service
                .definition_path
                .as_ref()
                .context("Missing original definition path")?,
        ) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(e.into()),
            Ok(_) => false,
        },
    };
    let cached = if present && matches!(service.manager, ServiceManager::SystemdUser) {
        // The exact manager/name is unchanged. Its removed definition must not
        // be reread as a required file; the remaining cache is only evidence of
        // inactive/no-process state, never a claim of complete native absence.
        let cached_service = Service {
            manager: service.manager.clone(),
            name: service.name.clone(),
            definition_path: None,
        };
        Some(installation::snapshot(&cached_service)?)
    } else {
        None
    };
    removed_state(
        &service.manager,
        definition_absent,
        present,
        cached.as_ref(),
    )
}

fn remove_original(request: &Request, config: &Config) -> Result<Value> {
    ensure!(
        matches!(&request.target, Target::Original)
            && request.previous_process.is_none()
            && request.setup_bundle.is_none(),
        "Removal applies only to the inactive original desktop profile"
    );
    let expected = request
        .expected_original
        .as_ref()
        .context("Missing captured original preference")?;
    ensure!(
        expected.is_object() && preference(expected) == *expected && expected["registered"] == true,
        "Original removal requires the exact captured preference fields"
    );
    // These are the existing updater/application lock files, not a new product
    // lock or permission bypass. Only this disposable controller uses them.
    let _owner = fixture_lock(&request.home, "native-update.lock")?;
    let _service_lock = fixture_lock(&request.home, "service.lock")?;
    let original = native_startup_state::capture(&request.home, &config.id)?;
    ensure!(
        original["registered"] == true,
        "No original desktop registration exists"
    );
    let service = original_service(&request.home, &config.id)?;
    let before = installation::snapshot(&service)?;
    inactive(&before)?;
    native_startup_state::validate(&original)?;
    let mut observed = sanitized(&service, &before, true)?;
    observed["registered"] = json!(true);
    observed["startup_settings_sha256"] = json!(digest(&original["settings"])?);
    ensure!(
        preference(&observed) == *expected,
        "Original startup preference changed before removal"
    );
    let final_before = installation::snapshot(&service)?;
    inactive(&final_before)?;
    ensure!(
        final_before == before,
        "Original native snapshot changed before removal"
    );
    native_startup_state::validate(&original)?;
    let id = uuid::Uuid::parse_str(&config.id)?;
    if amplifier_native_platform::remove_startup(&native_bundle::APPLICATION, &request.home, &id)
        .is_err()
    {
        return Ok(
            json!({"status":"original_removal_unknown","dispatch":{"status":"unknown"},
            "before":observed,"retry_permitted":false}),
        );
    }
    let after = match observe_original_removed(&service) {
        Ok(value) => value,
        Err(_) => {
            return Ok(
                json!({"status":"original_removal_unknown","dispatch":{"status":"unknown"},
            "before":observed,"code":"post_remove_native_state_unknown","retry_permitted":false}),
            );
        }
    };
    Ok(
        json!({"status":"original_removed", "dispatch":{"status":"accepted"},
        "before":observed,"after":after,"original_definition_absent":true,
        "inactive_after":true,"one_shot_registration_mutation":false,"retry_permitted":false,
        "manager_cache_absence_claimed":false}),
    )
}

fn run(mut request: Request) -> Result<Value> {
    ensure!(
        std::env::var("CONSTELLATION_NATIVE_STARTUP_TEST").as_deref() == Ok("1"),
        "Disposable manager controller opt-in missing"
    );
    ensure!(
        request.home.is_absolute(),
        "Installation home must be absolute"
    );
    ensure!(
        matches!(request.action, Action::RemoveOriginal) || request.expected_original.is_none(),
        "Expected original preference belongs only to removal"
    );
    if matches!(request.action, Action::PrepareManaged) {
        ensure!(
            matches!(request.target, Target::Original),
            "Setup target must be original"
        );
        ensure!(
            request.previous_process.is_none(),
            "Fresh setup has no prior process"
        );
        ensure!(
            matches!(fs::symlink_metadata(&request.home), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
            "Managed fixture home must be strictly absent"
        );
        let bundle = request.setup_bundle.context("Missing exact setup bundle")?;
        ensure!(bundle.is_absolute(), "Setup bundle must be absolute");
        let verified = native_bundle::verify(&bundle)?;
        ensure!(
            verified["target"] == native_bundle::target()?,
            "Setup bundle targets another host"
        );
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        desktop::create(&request.home, listener.local_addr()?.port())?;
        drop(listener);
        native_update::bootstrap(&request.home, &bundle)?;
        ensure!(
            !request.home.join("runtime.json").exists(),
            "Fresh setup unexpectedly launched an application"
        );
        return Ok(
            json!({"status":"prepared","setup":"library_assisted_fresh_home", "service_started":false,
            "manifest_sha256":verified["manifest_sha256"]}),
        );
    }
    ensure!(
        request.setup_bundle.is_none(),
        "Setup bundle is only valid for fresh managed setup"
    );
    let metadata = fs::symlink_metadata(&request.home)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Installation home is not ordinary"
    );
    request.home = dunce::canonicalize(&request.home)?;
    let config = Config::load(&request.home)?;
    if matches!(request.action, Action::RemoveOriginal) {
        return remove_original(&request, &config);
    }
    ensure!(
        request.expected_original.is_none(),
        "Expected original preference belongs only to removal"
    );
    let mut result = match request.target {
        Target::Original => {
            ensure!(
                !matches!(request.action, Action::ObserveRetirement),
                "Original startup registration is separate from one-shot retirement"
            );
            let original = native_startup_state::capture(&request.home, &config.id)?;
            if original["registered"] == false {
                ensure!(
                    matches!(request.action, Action::Inspect),
                    "Unregistered application cannot be manager-stopped"
                );
                json!({"status":"absent","registered":false,"active":false,"processes":[]})
            } else {
                let service = original_service(&request.home, &config.id)?;
                let snapshot = installation::snapshot(&service)?;
                native_startup_state::validate(&original)?;
                let mut value = sanitized(&service, &snapshot, true)?;
                value["registered"] = json!(true);
                value["startup_settings_sha256"] = json!(digest(&original["settings"])?);
                if matches!(request.action, Action::Stop) {
                    let outcome = installation::request_state_checked(
                        &service,
                        ServiceAction::Stop,
                        &snapshot,
                        |current| {
                            native_startup_state::validate(&original)?;
                            required_application_matches(request.previous_process.as_ref(), current)
                        },
                    )?;
                    value["dispatch"] = serde_json::to_value(outcome)?;
                }
                value
            }
        }
        _ => {
            let Some(prepared) = saved_launch(&request)? else {
                ensure!(
                    matches!(request.action, Action::Inspect),
                    "No saved launch exists to stop"
                );
                return Ok(json!({"status":"not_dispatched"}));
            };
            if matches!(request.action, Action::ObserveRetirement) {
                let receipt = native_update_launch::observe_retirement(&prepared)?;
                let retired = matches!(
                    receipt,
                    Some(native_update_launch::RetirementReceipt::Retired { .. })
                );
                return Ok(json!({"status":"retirement_observed","retired":retired,
                    "receipt":receipt}));
            }
            // observe rebuilds the fixed profile from its protected identity and
            // checks the actual native manager. The marker is never enough.
            let observation = native_update_launch::observe(&prepared)?;
            let encoded = serde_json::to_value(&prepared)?;
            let service: Service = serde_json::from_value(encoded["service"].clone())?;
            let profile: Registration = serde_json::from_value(encoded["profile"].clone())?;
            let mut value =
                sanitized(&service, &observation.snapshot, observation.profile_matches)?;
            value["launch_receipt"] = serde_json::to_value(observation.receipt)?;
            if matches!(request.action, Action::Stop) {
                ensure!(
                    observation.profile_matches,
                    "Saved launch does not match its native registration"
                );
                let outcome = installation::request_state_checked(
                    &service,
                    ServiceAction::Stop,
                    &observation.snapshot,
                    |current| {
                        if !registration::profile_matches(&service, &profile, current)? {
                            return Ok(Some(ActionOutcome::Refused {
                                code: "changed_profile".into(),
                                message: "Exact disposable profile changed before Stop".into(),
                            }));
                        }
                        required_application_matches(request.previous_process.as_ref(), current)
                    },
                )?;
                value["dispatch"] = serde_json::to_value(outcome)?;
            }
            value
        }
    };
    if matches!(request.target, Target::Worker { .. }) {
        let identity = actual_worker(&request, &config.id)?;
        let alive = identity.as_ref().map(process::matches).transpose()?;
        let binding = if alive == Some(true) {
            application_binding(identity.as_ref().unwrap(), &result["processes"])?
        } else {
            None
        };
        result["actual_worker_identity"] = json!(identity);
        result["actual_worker_alive"] = json!(alive);
        result["actual_worker_manager_binding"] = json!(binding);
    }
    if let Some(previous) = request.previous_process {
        let alive = process::matches(&previous)?;
        result["previous_process_alive"] = json!(alive);
        // Read-only fault-fixture proof: a pre-status application has no new
        // runtime.json yet. Bind its independently captured native generation
        // through actual manager ancestry, not Task Scheduler EnginePID equality.
        result["previous_process_manager_binding"] = json!(if alive {
            application_binding(&previous, &result["processes"])?
        } else {
            None
        });
    }
    let runtime = request.home.join("runtime.json");
    if runtime.is_file() {
        let runtime: Value = serde_json::from_slice(&fs::read(runtime)?)?;
        let pid = runtime["pid"]
            .as_u64()
            .context("Runtime process identity missing")?;
        result["application_identity"] =
            process::identity(u32::try_from(pid)?)?.unwrap_or(Value::Null);
        result["application_manager_binding"] = json!(application_binding(
            &result["application_identity"],
            &result["processes"]
        )?);
    }
    Ok(result)
}

fn main() {
    let outcome = (|| -> Result<Value> {
        let mut input = Vec::new();
        std::io::stdin().take(65537).read_to_end(&mut input)?;
        ensure!(input.len() <= 65536, "Private request exceeded bound");
        run(serde_json::from_slice(&input)?)
    })();
    match outcome {
        Ok(value) => println!("{value}"),
        Err(_) => {
            // Native errors can contain private manager definitions/argv.
            // Preserve uncertainty without copying those strings into evidence.
            println!(
                "{}",
                json!({"status":"unavailable","code":"native_controller_observation_or_dispatch_failed"})
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_disposal_refuses_missing_active_or_ambiguous_native_state() {
        let settled = json!({"active":false,"transitioning":false,"processes":[]});
        assert!(inactive(&settled).is_ok());
        for changed in [
            json!({}),
            json!({"active":false,"transitioning":false}),
            json!({"active":true,"transitioning":false,"processes":[]}),
            json!({"active":false,"transitioning":true,"processes":[]}),
            json!({"active":false,"transitioning":false,"processes":[{"identity":{"pid":1}}]}),
        ] {
            assert!(inactive(&changed).is_err());
        }
    }

    #[test]
    fn original_disposal_cannot_target_a_one_shot_or_silently_drop_policy_fields() {
        let request: Request = serde_json::from_value(json!({"home":"/fixture",
            "action":"remove_original","target":{"kind":"worker","operation":"x","attempt":"y"},
            "previous_process":null,"setup_bundle":null,"expected_original":{"registered":true}}))
        .unwrap();
        assert!(!matches!(&request.target, Target::Original));
        let missing = json!({"registered":true});
        assert_ne!(preference(&missing), missing);
        let mut full = preference(&missing);
        full["registered"] = json!(true);
        assert_eq!(preference(&full), full);
        full["arbitrary_command"] = json!("never accepted");
        assert_ne!(preference(&full), full);
    }

    #[test]
    fn removed_definition_alone_never_proves_native_registration_absence() {
        let inactive = json!({"active":false,"transitioning":false,"processes":[]});
        assert!(removed_state(&ServiceManager::LaunchAgent, true, true, None).is_err());
        assert!(removed_state(&ServiceManager::ScheduledTask, true, true, None).is_err());
        assert!(removed_state(&ServiceManager::SystemdUser, true, true, None).is_err());
        assert!(removed_state(&ServiceManager::SystemdUser, false, false, None).is_err());
        let cache =
            removed_state(&ServiceManager::SystemdUser, true, true, Some(&inactive)).unwrap();
        assert_eq!(cache["native_absence_proved"], false);
        assert_eq!(cache["inactive_after"], true);
        assert!(
            removed_state(
                &ServiceManager::SystemdUser,
                true,
                true,
                Some(&json!({"active":true,"transitioning":false,"processes":[]}))
            )
            .is_err()
        );
        for manager in [
            ServiceManager::SystemdUser,
            ServiceManager::LaunchAgent,
            ServiceManager::ScheduledTask,
        ] {
            assert_eq!(
                removed_state(&manager, true, false, None).unwrap()["native_absence_proved"],
                true
            );
        }
    }
}
