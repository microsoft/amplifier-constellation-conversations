//! Disposable qualification controller; never part of the packaged application.
//! Private stdin selects only this installation's original application or a
//! fixed launch already recorded by its real updater. There is no delete action.
use agent_conversations::{config::Config, native_startup_state, native_update_launch};
use amplifier_native_platform::{
    installation::{self, ActionOutcome, Service, ServiceAction, ServiceManager},
    process,
    registration::{self, Registration},
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::PathBuf};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Inspect,
    ObserveRetirement,
    Stop,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Original,
    Worker { operation: String, attempt: String },
    Replacement { transaction: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    home: PathBuf,
    action: Action,
    target: Target,
    previous_process: Option<Value>,
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
            Target::Replacement { transaction } => {
                identity["kind"] == "replacement" && identity["transaction"] == *transaction
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

fn run(mut request: Request) -> Result<Value> {
    ensure!(
        std::env::var("CONSTELLATION_NATIVE_STARTUP_TEST").as_deref() == Ok("1"),
        "Disposable manager controller opt-in missing"
    );
    ensure!(
        request.home.is_absolute(),
        "Installation home must be absolute"
    );
    let metadata = fs::symlink_metadata(&request.home)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Installation home is not ordinary"
    );
    request.home = dunce::canonicalize(&request.home)?;
    let config = Config::load(&request.home)?;
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
                        |_| {
                            native_startup_state::validate(&original)?;
                            Ok(None)
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
                        Ok(
                            (!registration::profile_matches(&service, &profile, current)?).then(
                                || ActionOutcome::Refused {
                                    code: "changed_profile".into(),
                                    message: "Exact disposable profile changed before Stop".into(),
                                },
                            ),
                        )
                    },
                )?;
                value["dispatch"] = serde_json::to_value(outcome)?;
            }
            value
        }
    };
    if let Some(previous) = request.previous_process {
        result["previous_process_alive"] = json!(process::matches(&previous)?);
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
