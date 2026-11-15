//! Explicit Linux legacy adoption. The owner stops the captured old service;
//! this command never drains, cancels, resolves or replays conversation work.
//! The legacy executable is retained as a legacy origin, not a native bundle.
use super::*;
use crate::{native_adoption_state as saved, native_legacy_origin as origin};
use amplifier_native_platform::installation::{self, ActionOutcome, ServiceAction};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeSet;

const CAPTURE: &str = "native-legacy-capture.json";
const ORIGIN: &str = "native-legacy-origin.json";

fn supported(home: &Path) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "Legacy adoption is qualified only for Linux"
    );
    ensure!(
        home.is_absolute() && dunce::canonicalize(home)? == home,
        "Use the canonical existing installation home"
    );
    amplifier_native_platform::validate_private_directory(home)?;
    ensure!(
        !home.join("companion.json").exists(),
        "Companion is excluded from legacy adoption"
    );
    ensure!(
        !home.join("native-update-owner.json").exists(),
        "An enrolled owner already controls this installation"
    );
    Ok(())
}
fn protected_record(path: &Path) -> Result<Value> {
    ordinary(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = fs::symlink_metadata(path)?;
        ensure!(
            m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0,
            "Legacy record is not private current-user storage"
        );
    }
    read(path)
}
fn create_record(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    ensure!(
        bytes.len() < 1024 * 1024,
        "Legacy evidence exceeds the bounded record size"
    );
    amplifier_native_platform::private_create(path, &bytes)
}

fn wrapper(home: &Path) -> Result<(Value, String)> {
    let record = protected_record(&home.join(ORIGIN))?;
    ensure!(
        record["schema"] == 1
            && record["kind"] == "legacy_adoption_origin"
            && record["id"] == Config::load(home)?.id
            && record["home"] == home.to_string_lossy().as_ref(),
        "Legacy origin belongs to another installation"
    );
    let digest = digest(&home.join(ORIGIN))?;
    Ok((record, digest))
}
fn bound_origin(home: &Path, j: &Value) -> Result<Value> {
    let (o, hash) = wrapper(home)?;
    ensure!(
        j["kind"] == "legacy_adoption"
            && j["legacy_origin_sha256"] == hash
            && o["transaction_id"] == j["transaction_id"]
            && o["candidate"] == j["candidate"],
        "Legacy origin or candidate binding changed"
    );
    origin::validate(home, &o["origin"])?;
    Ok(o)
}
pub(super) fn launch_target(home: &Path, j: &Value) -> Result<cleanup::Target> {
    ensure!(
        j["adoption_start_requested"] == true,
        "No explicit adoption Start intent"
    );
    let o = bound_origin(home, j)?;
    Ok(cleanup::Target::LegacyOrigin {
        home: home.to_path_buf(),
        origin: o["origin"].clone(),
    })
}

/// Capture before the maintenance owner stops the original service. The caller
/// supplies its independently qualified old executable digest; capture grants
/// neither Stop nor permission to run an arbitrary package.
pub fn capture_legacy_origin(home: &Path, transaction: &str, legacy_sha256: &str) -> Result<Value> {
    supported(home)?;
    id(transaction)?;
    let _owner = lock(home, "native-update.lock")?;
    ensure!(
        !home.join(INSTALLED).exists()
            && !home.join(JOURNAL).exists()
            && !home.join(ORIGIN).exists(),
        "This installation already has native update state"
    );
    ensure!(
        legacy_sha256.len() == 64 && digest(&binary(home))? == legacy_sha256,
        "The qualified legacy executable changed"
    );
    let capture = json!({"schema":1,"id":Config::load(home)?.id,"home":home,
        "transaction_id":transaction,"legacy_binary_sha256":legacy_sha256,
        "config_sha256":digest(&home.join("config.json"))?,
        "origin":origin::capture(home, transaction)?});
    create_record(&home.join(CAPTURE), &capture)?;
    Ok(json!({"captured":true,"transaction_id":transaction,
        "next":"Owner maintenance stop, then adopt-existing; no Stop was requested"}))
}

fn require_capture(home: &Path, transaction: &str) -> Result<Value> {
    let capture = protected_record(&home.join(CAPTURE))?;
    ensure!(
        capture["schema"] == 1
            && capture["transaction_id"] == transaction
            && capture["id"] == Config::load(home)?.id
            && capture["home"] == home.to_string_lossy().as_ref()
            && capture["config_sha256"] == digest(&home.join("config.json"))?
            && capture["legacy_binary_sha256"] == digest(&binary(home))?,
        "Captured legacy installation changed; original preserved"
    );
    origin::verify_stopped(home, &capture["origin"])?;
    Ok(capture)
}

fn window(j: &Value) -> Result<saved::InitializationWindow> {
    Ok(saved::InitializationWindow {
        min_utc_ms: j["initialization_started_at"]
            .as_i64()
            .context("No initialization boundary")?,
        max_utc_ms: crate::state::now_ms(),
    })
}
fn state_proof(home: &Path, j: &Value, o: &Value) -> Result<()> {
    let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
    let gate = saved::ExpectedGate {
        transaction: j["transaction_id"]
            .as_str()
            .context("Missing transaction")?
            .to_owned(),
        phase: saved::GatePhase::Prepared,
    };
    saved::verify_preserved(
        home,
        &before,
        window(j)?,
        if j["legacy_early_recovery"] == true && j["early_gate_present"] == false {
            None
        } else {
            Some(&gate)
        },
    )
}

/// The command is run from the qualified replacement package. The original
/// manager is already stopped. No process is started until the raw-state audit,
/// additive closed gate and controlled migrations all succeed.
pub async fn adopt_existing(home: &Path, transaction: &str, package: &Path) -> Result<Value> {
    supported(home)?;
    standalone_worker(home)?;
    id(transaction)?;
    let _owner = lock(home, "native-update.lock")?;
    let service = lock(home, "service.lock")?;
    ensure!(
        !home.join(INSTALLED).exists()
            && !home.join(JOURNAL).exists()
            && !home.join(ORIGIN).exists(),
        "Existing adoption must be observed or recovered, never redispatched"
    );
    let captured = require_capture(home, transaction)?;
    let audit = saved::AuditGuard::begin(home)?;
    ensure!(
        audit.report().ready,
        "Legacy work is active or unsettled; no adoption performed"
    );
    let staged = native_bundle::install(package, home)?;
    let candidate = package_record(staged)?;
    check_package(&candidate)?;
    // The library performing migrations and the candidate must be the same
    // exact executable; a newer or different package cannot borrow this proof.
    ensure!(
        digest(&std::env::current_exe()?)? == candidate["binary_sha256"],
        "Run adoption using this exact candidate package executable"
    );
    require_capture(home, transaction)?;
    audit.verify_current()?;
    let legacy_path = home.join(format!("native-legacy-{transaction}.bin"));
    let old_bytes = fs::read(binary(home))?;
    ensure!(
        hash(&old_bytes) == captured["legacy_binary_sha256"],
        "Legacy executable changed"
    );
    amplifier_native_platform::private_create(&legacy_path, &old_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&legacy_path, fs::Permissions::from_mode(0o700))?;
    }
    let o = json!({"schema":1,"kind":"legacy_adoption_origin","id":captured["id"],"home":home,
        "transaction_id":transaction,"origin":captured["origin"],"snapshot":audit.snapshot(),
        "legacy_binary":legacy_path,"legacy_binary_sha256":captured["legacy_binary_sha256"],
        "candidate":candidate});
    create_record(&home.join(ORIGIN), &o)?;
    let mut j = json!({"schema":1,"kind":"legacy_adoption","installation_id":captured["id"],
        "transaction_id":transaction,"phase":"legacy_prepared","candidate":candidate,"previous":null,
        "selected":"candidate","legacy_origin_sha256":digest(&home.join(ORIGIN))?,
        "config_sha256":captured["config_sha256"],"original_running":false,"startup":null,
        "adoption_start_requested":false,"launch_nonce":null,"restore_started":false,
        "direction":"applied","initialization_started_at":crate::state::now_ms()});
    save(&home.join(JOURNAL), &j)?; // durable recovery intent precedes the gate
    audit.prepare_gate(transaction)?;
    // No router, driver, probe or delivery loop runs in this controlled open.
    drop(AppState::open(home.to_path_buf())?);
    state_proof(home, &j, &o)?;
    origin::verify_stopped(home, &o["origin"])?;
    ensure!(
        digest(&home.join("config.json"))? == j["config_sha256"],
        "Configuration changed"
    );
    phase(home, &mut j, "switching")?;
    write_binary(
        &path(&candidate["bundle"]["entrypoints"], "service")?,
        &binary(home),
    )?;
    phase(home, &mut j, "applied")?;
    drop(service);
    phase(home, &mut j, "verifying")?;
    j["adoption_start_requested"] = json!(true);
    let mut child = start(home, &mut j)?;
    ensure!(
        child.is_none(),
        "Legacy adoption must use the existing manager"
    );
    let health = healthy(home, &mut j, child.as_mut()).await?;
    ensure!(
        health["live_work"] == false,
        "Candidate accepted unexpected work"
    );
    state_proof(home, &j, &o)?;
    j["candidate_health"] = health;
    phase(home, &mut j, "verified")?;
    commit(home, &mut j).await
}

async fn commit(home: &Path, j: &mut Value) -> Result<Value> {
    let o = bound_origin(home, j)?;
    ensure!(
        j["phase"] == "verified",
        "Verified candidate required before adoption commit"
    );
    state_proof(home, j, &o)?;
    check_package(&j["candidate"])?;
    ensure!(
        digest(&binary(home))? == j["candidate"]["binary_sha256"],
        "Candidate executable changed"
    );
    let installed = json!({"schema":1,"id":j["installation_id"],"package":j["candidate"],
        "legacy_origin_sha256":j["legacy_origin_sha256"]});
    // First genuine native receipt is published only after actual health and
    // retained-state verification. Legacy bytes never masquerade as a package.
    save(&home.join(INSTALLED), &installed)?;
    j["restore_started"] = json!(true);
    j["restore_boundary_verified"] = json!(true);
    j["finish_intent"] = json!(true);
    phase(home, j, "restoring")?;
    local_api(
        home,
        "POST",
        "finish",
        Some(json!({"transaction_id":j["transaction_id"],
        "launch_nonce":j["launch_nonce"]})),
    )
    .await?;
    phase(home, j, "restored")?;
    Ok(json!({"adopted":true,"transaction_id":j["transaction_id"],
        "legacy_rollback":"pre-admission only; future updates use native bundles"}))
}

pub(super) fn validate_candidate_cleanup(home: &Path, j: &Value, package: &Value) -> Result<()> {
    let _ = bound_origin(home, j)?;
    ensure!(
        j["candidate"] == *package && digest(&binary(home))? == package["binary_sha256"],
        "Legacy candidate cleanup binding changed"
    );
    check_package(package)
}

/// Exact historical unknowns may remain unknown across a stopped adoption.
/// Only origin-bound unchanged original rows are excluded from the input busy
/// predicate; no other busy predicate, new row or changed outcome is waived.
pub(crate) fn retained_inputs(db: &Connection) -> Result<BTreeSet<String>> {
    let Some(path) = db.path().filter(|p| !p.is_empty()) else {
        return Ok(BTreeSet::new());
    };
    let home = Path::new(path).parent().context("Missing database home")?;
    if !home.join(ORIGIN).exists() {
        return Ok(BTreeSet::new());
    }
    let (o, hash) = wrapper(home)?;
    let committed = if home.join(INSTALLED).exists() {
        protected_record(&home.join(INSTALLED))?["legacy_origin_sha256"] == hash
    } else {
        false
    };
    if !committed {
        let j = protected_record(&home.join(JOURNAL))?;
        ensure!(
            j["kind"] == "legacy_adoption"
                && j["legacy_origin_sha256"] == hash
                && j["transaction_id"] == o["transaction_id"],
            "Uncommitted legacy origin is not bound"
        );
    }
    let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
    saved::matching_historical_inputs(db, &before, &before.saved_state_sha256)
}

/// Explicit recovery is stop-only for a confirmed candidate generation. A lost
/// Start/Stop/finish acknowledgement is observed, never automatically repeated.
pub async fn recover_adoption(home: &Path, transaction: &str) -> Result<Value> {
    supported(home)?;
    standalone_worker(home)?;
    let _owner = lock(home, "native-update.lock")?;
    if !home.join(JOURNAL).exists() {
        // An immutable origin can be published just before the journal. No
        // gate, migration, image switch or Start was reached in that window.
        let service = lock(home, "service.lock")?;
        let (o, origin_hash) = wrapper(home)?;
        ensure!(
            o["transaction_id"] == transaction && !home.join(INSTALLED).exists(),
            "Orphan origin belongs to another operation"
        );
        origin::verify_stopped(home, &o["origin"])?;
        let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
        saved::verify_before_startup(home, &before, None)?;
        let mut j = json!({"schema":1,"kind":"legacy_adoption","installation_id":o["id"],
            "transaction_id":transaction,"phase":"legacy_prepared","candidate":o["candidate"],
            "previous":null,"selected":"candidate","original_running":false,"startup":null,
            "legacy_origin_sha256":origin_hash,"config_sha256":before.config_sha256,
            "legacy_early_recovery":true,"early_gate_present":false,
            "initialization_started_at":crate::state::now_ms(),"restore_started":false});
        save(&home.join(JOURNAL), &j)?;
        return begin_legacy_restoration(home, &mut j, &o, service).await;
    }
    let mut j = journal(home, transaction)?;
    let o = bound_origin(home, &j)?;
    if terminal(&j) {
        return Ok(json!({"terminal":true,"transaction_id":transaction}));
    }
    if home.join(INSTALLED).exists() {
        ensure!(
            protected_record(&home.join(INSTALLED))?["legacy_origin_sha256"]
                == j["legacy_origin_sha256"],
            "Installed origin changed"
        );
        let db = Connection::open_with_flags(
            home.join("state.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let restored: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM native_update_admission WHERE transaction_id=?1 AND phase='restored')",
            [transaction], |r| r.get(0))?;
        if !restored && j["finish_intent"] != true && j["phase"] == "verified" {
            // Receipt publication succeeded before finish intent. Observe the
            // existing accepted generation; do not issue another Start.
            healthy(home, &mut j, None).await?;
            return commit(home, &mut j).await;
        }
        ensure!(
            restored,
            "Native baseline committed but finish remains unconfirmed; no rollback or redispatch"
        );
        phase(home, &mut j, "restored")?;
        return Ok(json!({"adopted":true,"reconciled":true,"transaction_id":transaction}));
    }
    if j["legacy_restore_intent"] == true {
        ensure!(
            j["legacy_restore_ack"] == "accepted",
            "Retained legacy Start has no confirmed acknowledgement; no redispatch"
        );
        return legacy_health(home, &mut j, &o).await;
    }
    if j["phase"] == "legacy_prepared" && j["launch_cleanup"].is_null() {
        let service = lock(home, "service.lock")?;
        origin::verify_stopped(home, &o["origin"])?;
        let gate = early_gate(home, transaction)?;
        let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
        if saved::verify_before_startup(home, &before, gate.as_ref()).is_ok() {
            j["legacy_early_recovery"] = json!(true);
            j["early_gate_present"] = json!(gate.is_some());
            j["initialization_started_at"] = json!(crate::state::now_ms());
            return begin_legacy_restoration(home, &mut j, &o, service).await;
        }
        // Controlled initialization may have completed before the next journal
        // write. It must pass the retained post-migration proof; never rerun it.
        state_proof(home, &j, &o)?;
        drop(service);
    }
    // No legacy downgrade is permitted after admission was released.
    ensure!(
        update_admission::status(home)?["closed"] == true,
        "Adoption no longer owns a closed work boundary"
    );
    if j["phase"] != "switching" {
        if !j["launch_cleanup"].is_null() {
            cleanup::capture(home, &mut j)?;
            cleanup::revoke(&mut j)?;
        }
        j["launch_nonce"] = Value::Null;
        phase(home, &mut j, "switching")?;
    }
    let service = if j["launch_cleanup"].is_null() {
        lock(home, "service.lock")?
    } else {
        cleanup::stop(home, &mut j)
            .await?
            .context("No accepted candidate exit proof")?
    };
    origin::verify_absent(home, &o["origin"])?;
    state_proof(home, &j, &o)?;
    let old_path = path(&o, "legacy_binary")?;
    ensure!(
        digest(&old_path)? == o["legacy_binary_sha256"],
        "Retained legacy executable changed"
    );
    let actual = digest(&binary(home))?;
    ensure!(
        actual == o["legacy_binary_sha256"] || actual == j["candidate"]["binary_sha256"],
        "Unrecognized executable; recovery preserves it"
    );
    write_binary(&old_path, &binary(home))?;
    begin_legacy_restoration(home, &mut j, &o, service).await
}

fn early_gate(home: &Path, transaction: &str) -> Result<Option<saved::ExpectedGate>> {
    let db =
        Connection::open_with_flags(home.join("state.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tables: i64 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('native_update_admission','native_update_activity')",[],|r|r.get(0))?;
    if tables == 0 {
        return Ok(None);
    }
    ensure!(
        tables == 2,
        "Incomplete admission schema; no early recovery authority"
    );
    Ok(Some(saved::ExpectedGate {
        transaction: transaction.to_owned(),
        phase: saved::GatePhase::Prepared,
    }))
}

async fn begin_legacy_restoration(
    home: &Path,
    j: &mut Value,
    o: &Value,
    service: InstallationLock,
) -> Result<Value> {
    j["legacy_restore_intent"] = json!(true);
    ensure!(
        j["legacy_restore_ack"].is_null(),
        "Prior legacy Start is uncertain; no redispatch"
    );
    j["legacy_restore_ack"] = json!("intent");
    phase(home, j, "legacy_restoring")?;
    let manager = origin::validate(home, &o["origin"])?;
    let expected = installation::snapshot(&manager)?;
    origin::native_profile(&o["origin"], &expected)?;
    drop(service);
    // Explicitly separate restoration of the owner-stopped legacy service from
    // the truthful original_running=false capture used by native updates.
    let acknowledgement = installation::request_state(&manager, ServiceAction::Start, &expected);
    j["legacy_restore_ack"] = match acknowledgement {
        Ok(ActionOutcome::Accepted) => json!("accepted"),
        Ok(ActionOutcome::Refused { .. }) => json!("refused"),
        _ => json!("unknown"),
    };
    save(&home.join(JOURNAL), j)?;
    ensure!(
        j["legacy_restore_ack"] == "accepted",
        "Legacy Start unconfirmed; retained without retry"
    );
    legacy_health(home, j, o).await
}

async fn legacy_health(home: &Path, j: &mut Value, o: &Value) -> Result<Value> {
    let manager = origin::validate(home, &o["origin"])?;
    // Old code has no native health/ticket protocol. Its original authenticated
    // read route plus exact manager generation/profile are separate evidence.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let native = installation::snapshot(&manager)?;
        origin::native_profile(&o["origin"], &native)?;
        if native["active"] == true
            && let Ok(meta) = request_path(home, "GET", "/api/v1/meta", None).await
            && meta["id"] == j["installation_id"]
        {
            let transaction = j["transaction_id"]
                .as_str()
                .context("Missing transaction")?
                .to_owned();
            let restored_origin = origin::capture(home, &transaction)?;
            ensure!(
                restored_origin["binary_sha256"] == o["legacy_binary_sha256"],
                "Restored legacy running image changed"
            );
            state_proof(home, j, o)?;
            j["legacy_health"] = json!({"native":native,"authenticated_meta":true,
                "restored_origin":restored_origin});
            j["direction"] = json!("rolled_back");
            // The old server does not consume the new gate. Keep its exact row
            // as retained evidence; do not claim native health or baseline.
            phase(home, j, "restored")?;
            return Ok(
                json!({"legacy_restored":true,"native_installed":false,"transaction_id":transaction}),
            );
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "Legacy restoration health unconfirmed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn generic_lifecycle_refuses_legacy_journal_before_database_or_manager_work() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = Config::local(home, "Unstarted test", 19876).unwrap();
        config.save(home).unwrap();
        config::private_dir(&home.join("bin")).unwrap();
        config::private_write(&binary(home), b"unexecuted original fixture").unwrap();
        let transaction = uuid::Uuid::new_v4().to_string();
        let retained = json!({"schema":1,"kind":"legacy_adoption","installation_id":config.id,
            "transaction_id":transaction,"phase":"applied","candidate":null,"original_running":false});
        save(&home.join(JOURNAL), &retained).unwrap();
        let bytes = fs::read(home.join(JOURNAL)).unwrap();
        for result in [
            prepare(home, &transaction, &json!({})).await,
            apply(home, &transaction, &json!({}), false).await,
            verify(home, &transaction).await,
            restore(home, &transaction, "applied").await,
            abort_rollback(home, &transaction, "fixture").await,
        ] {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("explicit adoption controls"), "{error}");
            assert_eq!(fs::read(home.join(JOURNAL)).unwrap(), bytes);
            assert!(!home.join("state.sqlite3").exists());
            assert!(!home.join(INSTALLED).exists());
        }
    }
}
