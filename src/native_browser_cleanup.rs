//! Bounded maintenance for exact retained browser-update registrations. This
//! never stops a process, removes an active replacement, or retries an uncertain
//! manager mutation. The shared checked-retirement primitive owns OS changes.
use crate::{
    auth,
    config::Config,
    error::{ApiError, ApiResult},
    native_browser_update, native_update,
    native_update_launch::{
        self, LaunchIdentity, PreparedLaunch, RetainedLaunch, RetirementReceipt,
    },
    state::AppState,
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const RECEIPT: &str = "native-browser-update/cleanup.json";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CleanupInput {}

trait Retirer {
    fn enumerate(&self, home: &Path) -> Result<Vec<RetainedLaunch>>;
    fn observe(&self, prepared: &PreparedLaunch)
    -> Result<native_update_launch::LaunchObservation>;
    fn retire(&self, prepared: &PreparedLaunch) -> Result<RetirementReceipt>;
}
struct NativeRetirer;
impl Retirer for NativeRetirer {
    fn enumerate(&self, home: &Path) -> Result<Vec<RetainedLaunch>> {
        native_update_launch::enumerate(home)
    }
    fn observe(
        &self,
        prepared: &PreparedLaunch,
    ) -> Result<native_update_launch::LaunchObservation> {
        native_update_launch::observe(prepared)
    }
    fn retire(&self, prepared: &PreparedLaunch) -> Result<RetirementReceipt> {
        native_update_launch::retire(prepared)
    }
}
fn save(home: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= 256 * 1024,
        "Cleanup summary exceeds its bound"
    );
    amplifier_native_platform::private_dir(&home.join("native-browser-update"))?;
    amplifier_native_platform::private_write_data(&home.join(RECEIPT), &bytes)
}
fn identity(value: &LaunchIdentity) -> Value {
    match value {
        LaunchIdentity::Worker { operation, attempt } => {
            json!({"kind":"worker","operation_id":operation,"attempt_id":attempt})
        }
        LaunchIdentity::Replacement { transaction } => {
            json!({"kind":"replacement","transaction_id":transaction})
        }
    }
}
fn allowed(authority: &native_browser_update::CleanupAuthority, value: &LaunchIdentity) -> bool {
    match value {
        LaunchIdentity::Worker { operation, attempt } => authority.worker(operation, attempt),
        LaunchIdentity::Replacement { transaction } => authority.replacement(transaction),
    }
}
fn safe_code(code: &str) -> &str {
    if !code.is_empty()
        && code.len() <= 96
        && code
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        code
    } else {
        "retirement_not_confirmed"
    }
}
fn execute(
    home: &Path,
    authority: &native_browser_update::CleanupAuthority,
    retirer: &dyn Retirer,
) -> Result<Value> {
    let launches = match retirer.enumerate(home) {
        Ok(launches) => launches,
        Err(_) => {
            // Inventory refusal occurs before any retirement is requested. It
            // must not turn retained history into a lifetime package-check cap.
            let receipt = json!({"schema":1,"id":uuid::Uuid::new_v4().to_string(),"installation_id":Config::load(home)?.id,
                "started_at":crate::state::now_ms(),"finished_at":crate::state::now_ms(),"complete":false,
                "state":"refused","code":"launch_inventory_unavailable","outcomes":[]});
            save(home, &receipt)?;
            return Ok(
                json!({"cleanup":{"id":receipt["id"],"complete":false,"state":"refused","code":"launch_inventory_unavailable","outcomes":[]}}),
            );
        }
    };
    ensure!(
        launches.len() <= 256,
        "Too many retained operation registrations"
    );
    let mut receipt = json!({"schema":1,"id":uuid::Uuid::new_v4().to_string(),"installation_id":Config::load(home)?.id,
        "started_at":crate::state::now_ms(),"finished_at":null,"complete":false,"outcomes":launches.iter().map(|launch|{
            let mut value=identity(&launch.identity);value["state"]=json!("pending");value
        }).collect::<Vec<_>>()});
    // This bounded current scan receipt may be replaced by a later scan; each
    // original launch and one-time retirement marker remains durable separately.
    save(home, &receipt)?;
    for (index, launch) in launches.iter().enumerate() {
        let mut row = identity(&launch.identity);
        row["observed_active"] = Value::Null;
        row["profile_matches"] = Value::Null;
        row["absent"] = Value::Null;
        if !allowed(authority, &launch.identity) {
            row["state"] = json!("refused");
            row["code"] = json!("operation_not_verified_terminal");
        } else {
            if let Ok(observation) = retirer.observe(&launch.prepared) {
                row["observed_active"] = observation.snapshot["active"]
                    .as_bool()
                    .map_or(Value::Null, Value::Bool);
                row["profile_matches"] = json!(observation.profile_matches);
            }
            match retirer.retire(&launch.prepared) {
                Ok(RetirementReceipt::Retired { removed }) => {
                    row["state"] = json!("retired");
                    row["absent"] = json!(true);
                    row["removed"] = json!(removed);
                }
                Ok(RetirementReceipt::Refused { code }) => {
                    row["state"] = json!("refused");
                    row["code"] = json!(safe_code(&code));
                }
                Ok(RetirementReceipt::Unknown { code }) => {
                    row["state"] = json!("unknown");
                    row["code"] = json!(safe_code(&code));
                }
                Err(_) => {
                    row["state"] = json!("unknown");
                    row["code"] = json!("retirement_observation_unavailable");
                }
            }
        }
        receipt["outcomes"][index] = row;
        save(home, &receipt)?;
    }
    receipt["complete"] = json!(
        receipt["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "retired")
    );
    receipt["finished_at"] = json!(crate::state::now_ms());
    save(home, &receipt)?;
    // No prepared arguments, executable paths, startup nonces or raw snapshots
    // are serialized into the browser response or this sanitized scan receipt.
    Ok(
        json!({"cleanup":{"id":receipt["id"],"complete":receipt["complete"],"outcomes":receipt["outcomes"]}}),
    )
}
fn cleanup_with(home: &Path, before_check: bool, retirer: &dyn Retirer) -> ApiResult<Value> {
    let _owner = native_update::lock(home, "native-update.lock")?;
    if home.join("native-update-owner.json").exists() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Use the installation's enrolled updater controls".into(),
        ));
    }
    let _execution =
        native_update::lock(home, native_browser_update::WORKER_LOCK).map_err(|_| {
            ApiError(
                StatusCode::CONFLICT,
                "A native worker still owns this installation".into(),
            )
        })?;
    let authority = native_browser_update::cleanup_authority(home)?;
    if before_check && !authority.current_settled {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Resolve the retained update before checking another package".into(),
        ));
    }
    Ok(execute(home, &authority, retirer)?)
}
pub(crate) fn cleanup(home: &Path) -> ApiResult<Value> {
    cleanup_with(home, false, &NativeRetirer)
}
pub(crate) fn before_check(home: &Path) -> ApiResult<()> {
    cleanup_with(home, true, &NativeRetirer).map(|_| ())
}
pub(crate) fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/companion/update/cleanup", post(http_cleanup))
}
async fn http_cleanup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_input): Json<CleanupInput>,
) -> ApiResult<Json<Value>> {
    auth::check(&state, &headers, "admin", true)?;
    let home = state.home.clone();
    tokio::task::spawn_blocking(move || cleanup(&home))
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Retained cleanup completion could not be observed".into(),
            )
        })?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };
    struct Fixture {
        _root: tempfile::TempDir,
        home: PathBuf,
        worker: PreparedLaunch,
        replacement: PreparedLaunch,
        operation: String,
        attempt: String,
        transaction: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let home = dunce::canonicalize(root.path()).unwrap();
            let config = Config::local(&home, "Cleanup fixture", 54127).unwrap();
            config.save(&home).unwrap();
            let helper = home
                .join("packages")
                .join("a".repeat(64))
                .join(crate::platform::executable("agent-conversations"));
            fs::create_dir_all(helper.parent().unwrap()).unwrap();
            fs::write(&helper, b"unexecuted immutable fixture").unwrap();
            let installed = amplifier_native_platform::installed_binary(
                &crate::native_bundle::APPLICATION,
                &home,
            );
            fs::create_dir_all(installed.parent().unwrap()).unwrap();
            fs::write(&installed, b"unexecuted installed fixture").unwrap();
            let operation = uuid::Uuid::new_v4().to_string();
            let attempt = uuid::Uuid::new_v4().to_string();
            let transaction = uuid::Uuid::new_v4().to_string();
            let worker = native_update_launch::prepare_worker(
                &home, &config.id, &operation, &attempt, &helper,
            )
            .unwrap();
            let replacement = native_update_launch::prepare_replacement(
                &home,
                &config.id,
                &transaction,
                "PRIVATE_STARTUP_NONCE",
                &installed,
            )
            .unwrap();
            Self {
                _root: root,
                home,
                worker,
                replacement,
                operation,
                attempt,
                transaction,
            }
        }
        fn authority(&self) -> native_browser_update::CleanupAuthority {
            native_browser_update::CleanupAuthority {
                workers: [(self.operation.clone(), self.attempt.clone())].into(),
                transactions: [self.transaction.clone()].into(),
                current_settled: true,
            }
        }
        fn launches(&self) -> Vec<RetainedLaunch> {
            vec![
                RetainedLaunch {
                    identity: LaunchIdentity::Worker {
                        operation: self.operation.clone(),
                        attempt: self.attempt.clone(),
                    },
                    prepared: self.worker.clone(),
                    launch_receipt: None,
                },
                RetainedLaunch {
                    identity: LaunchIdentity::Replacement {
                        transaction: self.transaction.clone(),
                    },
                    prepared: self.replacement.clone(),
                    launch_receipt: Some(native_update_launch::LaunchReceipt::Accepted),
                },
            ]
        }
    }
    struct FakeRetirer {
        records: Vec<RetainedLaunch>,
        calls: AtomicUsize,
        enumeration: AtomicUsize,
        unknown: bool,
    }
    impl Retirer for FakeRetirer {
        fn enumerate(&self, _: &Path) -> Result<Vec<RetainedLaunch>> {
            self.enumeration.fetch_add(1, Ordering::SeqCst);
            Ok(self.records.clone())
        }
        fn observe(
            &self,
            prepared: &PreparedLaunch,
        ) -> Result<native_update_launch::LaunchObservation> {
            let active = prepared == &self.records[1].prepared;
            Ok(native_update_launch::LaunchObservation {
                receipt: None,
                profile_matches: true,
                snapshot: json!({"active":active,"secret":"PRIVATE_SNAPSHOT_VALUE"}),
            })
        }
        fn retire(&self, prepared: &PreparedLaunch) -> Result<RetirementReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.unknown {
                return Ok(RetirementReceipt::Unknown {
                    code: "unknown_maintenance".into(),
                });
            }
            if prepared == &self.records[1].prepared {
                Ok(RetirementReceipt::Refused {
                    code: "active_instance".into(),
                })
            } else {
                Ok(RetirementReceipt::Retired { removed: true })
            }
        }
    }
    #[test]
    fn terminal_cleanup_reports_absence_and_active_refusal_without_private_profiles() {
        let fixture = Fixture::new();
        let fake = FakeRetirer {
            records: fixture.launches(),
            calls: AtomicUsize::new(0),
            enumeration: AtomicUsize::new(0),
            unknown: false,
        };
        let value = execute(&fixture.home, &fixture.authority(), &fake).unwrap();
        assert_eq!(value["cleanup"]["complete"], false);
        assert_eq!(value["cleanup"]["outcomes"][0]["state"], "retired");
        assert_eq!(value["cleanup"]["outcomes"][0]["absent"], true);
        assert_eq!(value["cleanup"]["outcomes"][1]["state"], "refused");
        assert_eq!(value["cleanup"]["outcomes"][1]["observed_active"], true);
        assert!(value["cleanup"]["outcomes"][1]["absent"].is_null());
        let response = value.to_string();
        let receipt = fs::read_to_string(fixture.home.join(RECEIPT)).unwrap();
        for text in [&response, &receipt] {
            for secret in [
                fixture.home.to_str().unwrap(),
                "PRIVATE_STARTUP_NONCE",
                "PRIVATE_SNAPSHOT_VALUE",
                "executable",
                "args",
            ] {
                assert!(!text.contains(secret));
            }
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn unresolved_or_unrecognized_attempts_are_never_sent_to_retirement() {
        let fixture = Fixture::new();
        let fake = FakeRetirer {
            records: fixture.launches(),
            calls: AtomicUsize::new(0),
            enumeration: AtomicUsize::new(0),
            unknown: false,
        };
        let mut authority = fixture.authority();
        authority.workers.clear();
        authority.transactions.clear();
        let value = execute(&fixture.home, &authority, &fake).unwrap();
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        assert!(
            value["cleanup"]["outcomes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["state"] == "refused"
                    && row["code"] == "operation_not_verified_terminal")
        );
    }
    #[test]
    fn unknown_outcomes_remain_unknown_and_never_claim_absence() {
        let fixture = Fixture::new();
        let fake = FakeRetirer {
            records: fixture.launches(),
            calls: AtomicUsize::new(0),
            enumeration: AtomicUsize::new(0),
            unknown: true,
        };
        let value = execute(&fixture.home, &fixture.authority(), &fake).unwrap();
        assert_eq!(value["cleanup"]["complete"], false);
        assert!(
            value["cleanup"]["outcomes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["state"] == "unknown" && row["absent"].is_null())
        );
        assert!(fixture.home.join(RECEIPT).is_file());
    }
    #[test]
    fn enrolled_or_executing_worker_refuses_before_manager_enumeration() {
        let fixture = Fixture::new();
        let fake = FakeRetirer {
            records: fixture.launches(),
            calls: AtomicUsize::new(0),
            enumeration: AtomicUsize::new(0),
            unknown: false,
        };
        let worker =
            native_update::lock(&fixture.home, native_browser_update::WORKER_LOCK).unwrap();
        assert_eq!(
            cleanup_with(&fixture.home, false, &fake).unwrap_err().0,
            StatusCode::CONFLICT
        );
        drop(worker);
        fs::write(fixture.home.join("native-update-owner.json"), b"{}").unwrap();
        assert_eq!(
            cleanup_with(&fixture.home, false, &fake).unwrap_err().0,
            StatusCode::CONFLICT
        );
        assert_eq!(fake.enumeration.load(Ordering::SeqCst), 0);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn inventory_refusal_is_retained_without_blocking_package_check_or_claiming_cleanup() {
        struct Unavailable;
        impl Retirer for Unavailable {
            fn enumerate(&self, _: &Path) -> Result<Vec<RetainedLaunch>> {
                anyhow::bail!("PRIVATE inventory failure detail")
            }
            fn observe(
                &self,
                _: &PreparedLaunch,
            ) -> Result<native_update_launch::LaunchObservation> {
                panic!("no manager observation")
            }
            fn retire(&self, _: &PreparedLaunch) -> Result<RetirementReceipt> {
                panic!("no manager mutation")
            }
        }
        let fixture = Fixture::new();
        let value = cleanup_with(&fixture.home, true, &Unavailable).unwrap();
        assert_eq!(value["cleanup"]["complete"], false);
        assert_eq!(value["cleanup"]["state"], "refused");
        assert_eq!(value["cleanup"]["code"], "launch_inventory_unavailable");
        assert!(
            !fs::read_to_string(fixture.home.join(RECEIPT))
                .unwrap()
                .contains("PRIVATE")
        );
    }
    #[test]
    fn input_accepts_no_arbitrary_profile_or_path_fields() {
        assert!(serde_json::from_value::<CleanupInput>(json!({})).is_ok());
        for value in [
            json!({"path":"/private"}),
            json!({"profile":{}}),
            json!({"operation_id":"arbitrary"}),
        ] {
            assert!(serde_json::from_value::<CleanupInput>(value).is_err());
        }
        assert_eq!(
            safe_code("private /path with token"),
            "retirement_not_confirmed"
        );
    }
}
