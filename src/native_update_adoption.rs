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

fn retain_legacy_image(path: &Path, bytes: &[u8]) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            amplifier_native_platform::private_create(path, bytes)?;
        }
        result => {
            let before = result?;
            ordinary(path)?;
            ensure!(
                before.len() == bytes.len() as u64,
                "Retained legacy image length differs"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    before.uid() == unsafe { libc::geteuid() } && before.mode() & 0o077 == 0,
                    "Retained legacy image policy is unsafe"
                );
            }
            // A crash after exact create but before origin/journal publication
            // is a pre-effect boundary. The caller has re-established the
            // captured stopped service and same transaction. Observe these
            // exact bytes; never replace an unknown or partial backup.
            ensure!(
                fs::read(path)? == bytes,
                "Retained legacy image differs; preserved"
            );
            ordinary(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let after = fs::symlink_metadata(path)?;
                ensure!(
                    before.dev() == after.dev()
                        && before.ino() == after.ino()
                        && before.mode() == after.mode()
                        && before.uid() == after.uid()
                        && before.mtime() == after.mtime()
                        && before.mtime_nsec() == after.mtime_nsec()
                        && before.ctime() == after.ctime()
                        && before.ctime_nsec() == after.ctime_nsec(),
                    "Retained legacy image changed during observation"
                );
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(path)?.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn validate_wrapper(home: &Path, record: &Value) -> Result<()> {
    ensure!(
        record["schema"] == 1
            && record["kind"] == "legacy_adoption_origin"
            && record["id"] == Config::load(home)?.id
            && record["home"] == home.to_string_lossy().as_ref()
            && record["transaction_id"].is_string()
            && record["origin"]["transaction_id"] == record["transaction_id"],
        "Legacy origin belongs to another installation"
    );
    Ok(())
}
fn record_hash(record: &Value) -> Result<String> {
    Ok(hash(&serde_json::to_vec_pretty(record)?))
}
fn fixed_wrapper(home: &Path) -> Result<(Value, String)> {
    let record = protected_record(&home.join(ORIGIN))?;
    validate_wrapper(home, &record)?;
    Ok((record, digest(&home.join(ORIGIN))?))
}
fn selected_wrapper(home: &Path, j: &Value) -> Result<(Value, String)> {
    if let Some(record) = j.get("legacy_origin_record") {
        validate_wrapper(home, record)?;
        let hash = record_hash(record)?;
        ensure!(
            j["legacy_origin_sha256"] == hash,
            "Journal-selected origin changed"
        );
        return Ok((record.clone(), hash));
    }
    fixed_wrapper(home)
}
fn wrapper(home: &Path) -> Result<(Value, String)> {
    if !home.join(INSTALLED).exists() && home.join(JOURNAL).exists() {
        let j = protected_record(&home.join(JOURNAL))?;
        if j["kind"] == "legacy_adoption" {
            return selected_wrapper(home, &j);
        }
    }
    fixed_wrapper(home)
}
fn bound_origin(home: &Path, j: &Value) -> Result<Value> {
    let (o, hash) = selected_wrapper(home, j)?;
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

fn predecessor(home: &Path, reference: &Value) -> Result<Value> {
    let transaction = reference["transaction_id"]
        .as_str()
        .context("Missing predecessor transaction")?;
    let archive_hash = reference["sha256"]
        .as_str()
        .context("Missing predecessor archive hash")?;
    let archive = crate::native_browser_update::retained_legacy_adoption(
        home,
        &Config::load(home)?.id,
        transaction,
        archive_hash,
    )?;
    ensure!(
        archive["schema"] == 1
            && archive["kind"] == "legacy_adoption_rollback"
            && archive["id"] == Config::load(home)?.id
            && archive["transaction_id"] == transaction
            && archive["journal"]["schema"] == 1
            && archive["journal"]["kind"] == "legacy_adoption"
            && archive["journal"]["installation_id"] == archive["id"]
            && archive["journal"]["transaction_id"] == transaction
            && archive["journal"]["phase"] == "restored"
            && archive["journal"]["direction"] == "rolled_back"
            && archive["journal"]["legacy_restore_ack"] == "accepted"
            && archive["journal"]["legacy_health"]["authenticated_meta"] == true,
        "Predecessor is not a verified terminal legacy restoration"
    );
    validate_wrapper(home, &archive["origin"])?;
    validate_capture_identity(home, transaction, &archive["capture"])?;
    ensure!(
        archive["origin"]["transaction_id"] == transaction
            && archive["journal"]["legacy_origin_sha256"] == record_hash(&archive["origin"])?
            && archive["journal"]["candidate"] == archive["origin"]["candidate"],
        "Predecessor origin or candidate binding changed"
    );
    for (raw, parsed) in [("capture_bytes", "capture"), ("journal_bytes", "journal")] {
        let bytes = archive[raw]
            .as_str()
            .context("Missing archived record bytes")?;
        ensure!(
            serde_json::from_str::<Value>(bytes)? == archive[parsed],
            "Archived record bytes differ from their bound value"
        );
    }
    let root_bytes = archive["root_origin_bytes"]
        .as_str()
        .context("Missing archived root origin")?;
    let root: Value = serde_json::from_str(root_bytes)?;
    validate_wrapper(home, &root)?;
    ensure!(
        hash(root_bytes.as_bytes()) == archive["root_origin_sha256"],
        "Archived root origin hash differs"
    );
    // A prior reentry can retain an older fixed root until native commit.
    // Its transaction need not equal this archive's transaction.
    Ok(archive)
}

fn validate_capture_identity(home: &Path, transaction: &str, capture: &Value) -> Result<()> {
    ensure!(
        capture["schema"] == 1
            && capture["transaction_id"] == transaction
            && capture["origin"]["transaction_id"] == transaction
            && capture["id"] == Config::load(home)?.id
            && capture["home"] == home.to_string_lossy().as_ref(),
        "Capture origin belongs to another installation or transaction"
    );
    Ok(())
}

fn validated_root_bytes(home: &Path, j: &Value, o: &Value) -> Result<Vec<u8>> {
    validate_wrapper(home, o)?;
    let selected = serde_json::to_vec_pretty(o)?;
    ensure!(
        hash(&selected) == j["legacy_origin_sha256"],
        "Selected origin changed"
    );
    protected_record(&home.join(ORIGIN))?;
    let current = fs::read(home.join(ORIGIN))?;
    if current != selected {
        ensure!(
            j["legacy_origin_record"] == *o,
            "Original immutable origin changed"
        );
        let prior = predecessor(home, &j["predecessor"])?;
        ensure!(
            current
                == prior["root_origin_bytes"]
                    .as_str()
                    .context("Archived root origin is missing")?
                    .as_bytes(),
            "Root origin is neither the exact predecessor nor selected origin; preserved"
        );
    }
    Ok(current)
}

fn reentry_archive(home: &Path, j: &Value, captured: &Value) -> Result<Value> {
    ensure!(
        j["kind"] == "legacy_adoption"
            && terminal(j)
            && j["direction"] == "rolled_back"
            && j["legacy_restore_ack"] == "accepted"
            && j["legacy_health"]["authenticated_meta"] == true,
        "Only a verified terminal legacy rollback permits a fresh attempt"
    );
    let o = bound_origin(home, j)?;
    // Do not promote a third/unrecognized fixed root into a new archive.
    // Reentry may retain only the exact selected root or its known predecessor.
    let root_bytes = validated_root_bytes(home, j, &o)?;
    ensure!(
        captured["transaction_id"] == j["transaction_id"],
        "A different capture is already pending; observe its exact request"
    );
    validate_capture_identity(
        home,
        j["transaction_id"]
            .as_str()
            .context("Missing transaction")?,
        captured,
    )?;
    let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
    let witness: saved::PreservationWitness =
        serde_json::from_value(j["preservation_witness"].clone())
            .context("Verified restoration requires its retained state witness")?;
    let gates = saved::predecessor_ledger(home, &before, Some(&witness))?;
    let restored = &j["legacy_health"]["restored_origin"];
    let mut expected = restored.clone();
    expected["transaction_id"] = captured["next_transaction_id"].clone();
    // This is retained authenticated health plus fresh same-generation
    // liveness. It does not manufacture a new HTTP health observation.
    let current = origin::capture(
        home,
        captured["next_transaction_id"]
            .as_str()
            .context("Missing fresh explicit transaction")?,
    )?;
    ensure!(
        current == expected,
        "Restored legacy identity or policy changed; fresh health evidence is required"
    );
    let bytes = |name: &str| -> Result<String> {
        protected_record(&home.join(name))?;
        Ok(String::from_utf8(fs::read(home.join(name))?)?)
    };
    let archive = json!({"schema":1,"kind":"legacy_adoption_rollback", "id":Config::load(home)?.id,
        "transaction_id":j["transaction_id"],"capture":protected_record(&home.join(CAPTURE))?,
        "origin":o,"journal":j,"gates":gates,
        "capture_bytes":bytes(CAPTURE)?,"journal_bytes":bytes(JOURNAL)?,
        "root_origin_bytes":String::from_utf8(root_bytes.clone())?,"root_origin_sha256":hash(&root_bytes)});
    let transaction = j["transaction_id"]
        .as_str()
        .context("Missing predecessor transaction")?;
    let hash = crate::native_browser_update::retain_legacy_adoption(
        home,
        &Config::load(home)?.id,
        transaction,
        &archive,
    )?;
    Ok(json!({"reference":{"transaction_id":transaction,"sha256":hash},"origin":current}))
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
        !home.join(INSTALLED).exists(),
        "This installation already has a native baseline"
    );
    ensure!(
        legacy_sha256.len() == 64 && digest(&binary(home))? == legacy_sha256,
        "The qualified legacy executable changed"
    );
    let existing = home
        .join(CAPTURE)
        .exists()
        .then(|| protected_record(&home.join(CAPTURE)))
        .transpose()?;
    if let Some(existing) = &existing
        && existing["transaction_id"] == transaction
    {
        ensure!(
            existing["schema"] == 1
                && existing["id"] == Config::load(home)?.id
                && existing["home"] == home.to_string_lossy().as_ref()
                && existing["legacy_binary_sha256"] == legacy_sha256,
            "Saved capture request differs"
        );
        if home.join(JOURNAL).exists() {
            let j = protected_record(&home.join(JOURNAL))?;
            if j["transaction_id"] == transaction {
                ensure!(
                    j["schema"] == 1
                        && j["kind"] == "legacy_adoption"
                        && j["installation_id"] == Config::load(home)?.id,
                    "Saved capture does not identify this adoption journal"
                );
                return Ok(
                    json!({"captured":false,"reconciled":true,"transaction_id":transaction,
                    "terminal":terminal(&j),"next":"Observe or recover the existing adoption; do not reuse its capture"}),
                );
            }
        }
        return Ok(
            json!({"captured":true,"reconciled":true,"transaction_id":transaction,
            "note":"Existing capture receipt only; no new capture or service action"}),
        );
    }
    let next = if home.join(JOURNAL).exists() {
        let j = protected_record(&home.join(JOURNAL))?;
        ensure!(
            j["transaction_id"] != transaction,
            "A new adoption requires a fresh transaction"
        );
        let mut old_capture = existing.clone().context("Prior capture is missing")?;
        old_capture["next_transaction_id"] = json!(transaction);
        Some(reentry_archive(home, &j, &old_capture)?)
    } else {
        ensure!(
            existing.is_none() && !home.join(ORIGIN).exists(),
            "Existing adoption must be observed or recovered"
        );
        None
    };
    let mut capture = json!({"schema":1,"id":Config::load(home)?.id,"home":home,
        "transaction_id":transaction,"legacy_binary_sha256":legacy_sha256,
        "config_sha256":digest(&home.join("config.json"))?,
        "origin":match &next {Some(next)=>next["origin"].clone(),None=>origin::capture(home, transaction)?}});
    if let Some(next) = next {
        let archive = predecessor(home, &next["reference"])?;
        ensure!(
            fs::read_to_string(home.join(CAPTURE))?
                == archive["capture_bytes"]
                    .as_str()
                    .context("Missing archived capture bytes")?,
            "Prior capture changed before publication"
        );
        capture["predecessor"] = next["reference"].clone();
        save(&home.join(CAPTURE), &capture)?;
    } else {
        create_record(&home.join(CAPTURE), &capture)?;
    }
    Ok(json!({"captured":true,"transaction_id":transaction,
        "next":"Owner maintenance stop, then adopt-existing; no Stop was requested"}))
}

fn require_capture(home: &Path, transaction: &str) -> Result<Value> {
    let capture = protected_record(&home.join(CAPTURE))?;
    validate_capture_identity(home, transaction, &capture)?;
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
fn state_proof(home: &Path, j: &Value, o: &Value) -> Result<saved::PreservationWitness> {
    let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
    if o["gate_transfer"].is_object() {
        let transfer: saved::GateTransfer = serde_json::from_value(o["gate_transfer"].clone())?;
        let observed = saved::classify_reentry_gate(home, &before, &transfer)?;
        ensure!(
            observed.stage == saved::GateStage::Prepared
                || (j["legacy_early_recovery"] == true
                    && observed.stage == saved::GateStage::Before),
            "Reentry no longer owns its exact closed admission boundary"
        );
        return saved::verify_reentry_preserved(home, &before, &transfer, &observed, window(j)?);
    }
    let gate = saved::ExpectedGate {
        transaction: j["transaction_id"]
            .as_str()
            .context("Missing transaction")?
            .to_owned(),
        phase: saved::GatePhase::Prepared,
    };
    saved::verify_preserved_with_witness(
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
        !home.join(INSTALLED).exists(),
        "This installation already has a native baseline"
    );
    let captured = require_capture(home, transaction)?;
    let prior = if captured["predecessor"].is_object() {
        let archive = predecessor(home, &captured["predecessor"])?;
        protected_record(&home.join(JOURNAL))?;
        protected_record(&home.join(ORIGIN))?;
        ensure!(
            archive["transaction_id"] != transaction
                && fs::read_to_string(home.join(JOURNAL))?
                    == archive["journal_bytes"]
                        .as_str()
                        .context("Missing archived journal bytes")?
                && fs::read_to_string(home.join(ORIGIN))?
                    == archive["root_origin_bytes"]
                        .as_str()
                        .context("Missing archived origin bytes")?,
            "Prior operation changed; observe or recover it without redispatch"
        );
        Some(archive)
    } else {
        ensure!(
            !home.join(JOURNAL).exists() && !home.join(ORIGIN).exists(),
            "Existing adoption must be observed or recovered, never redispatched"
        );
        None
    };
    let audit = if let Some(archive) = &prior {
        let before: saved::Snapshot =
            serde_json::from_value(archive["origin"]["snapshot"].clone())?;
        let proof = saved::ReentryProof {
            archive_sha256: captured["predecessor"]["sha256"]
                .as_str()
                .context("Missing archive hash")?
                .to_owned(),
            predecessor_transaction: archive["transaction_id"]
                .as_str()
                .context("Missing predecessor")?
                .to_owned(),
            expected_gates: serde_json::from_value(archive["gates"].clone())?,
            witness: if archive["journal"]["preservation_witness"].is_null() {
                None
            } else {
                Some(serde_json::from_value(
                    archive["journal"]["preservation_witness"].clone(),
                )?)
            },
        };
        saved::AuditGuard::begin_reentry(home, &before, &proof)?
    } else {
        saved::AuditGuard::begin(home)?
    };
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
    retain_legacy_image(&legacy_path, &old_bytes)?;
    let transfer = if prior.is_some() {
        Some(audit.plan_reentry_gate(transaction, crate::state::now_ms())?)
    } else {
        None
    };
    let mut o = json!({"schema":1,"kind":"legacy_adoption_origin","id":captured["id"],"home":home,
        "transaction_id":transaction,"origin":captured["origin"],"snapshot":audit.snapshot(),
        "legacy_binary":legacy_path,"legacy_binary_sha256":captured["legacy_binary_sha256"],
        "candidate":candidate});
    if let Some(transfer) = &transfer {
        o["gate_transfer"] = serde_json::to_value(transfer)?;
        o["predecessor"] = captured["predecessor"].clone();
    } else {
        create_record(&home.join(ORIGIN), &o)?;
    }
    let mut j = json!({"schema":1,"kind":"legacy_adoption","installation_id":captured["id"],
        "transaction_id":transaction,"phase":"legacy_prepared","candidate":candidate,"previous":null,
        "selected":"candidate","legacy_origin_sha256":record_hash(&o)?,
        "config_sha256":captured["config_sha256"],"original_running":false,"startup":null,
        "adoption_start_requested":false,"launch_nonce":null,"restore_started":false,
        "direction":"applied","initialization_started_at":crate::state::now_ms()});
    if let Some(archive) = &prior {
        // One journal publication selects the new origin and predecessor.
        // The old root origin remains intact until a verified native commit.
        j["legacy_origin_record"] = o.clone();
        j["predecessor"] = captured["predecessor"].clone();
        j["preservation_witness"] = archive["journal"]["preservation_witness"].clone();
    }
    save(&home.join(JOURNAL), &j)?; // durable recovery intent precedes the gate
    if let Some(transfer) = &transfer {
        audit.prepare_reentry_gate(transfer)?;
    } else {
        audit.prepare_gate(transaction)?;
    }
    // No router, driver, probe or delivery loop runs in this controlled open.
    drop(AppState::open(home.to_path_buf())?);
    j["preservation_witness"] = serde_json::to_value(state_proof(home, &j, &o)?)?;
    save(&home.join(JOURNAL), &j)?;
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
    materialize_origin(home, j, &o)?;
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

fn materialize_origin(home: &Path, j: &Value, o: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(o)?;
    ensure!(
        hash(&bytes) == j["legacy_origin_sha256"],
        "Selected origin changed before commit"
    );
    let current = validated_root_bytes(home, j, o)?;
    if current == bytes {
        return Ok(());
    }
    // All prior bytes are already durable in immutable history. A crash after
    // this write is still owned by the selected journal, before native commit.
    save(&home.join(ORIGIN), o)?;
    ensure!(
        digest(&home.join(ORIGIN))? == j["legacy_origin_sha256"],
        "Published origin differs from the selected record"
    );
    Ok(())
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
        let before: saved::Snapshot = serde_json::from_value(o["snapshot"].clone())?;
        if o["gate_transfer"].is_object() {
            let transfer: saved::GateTransfer = serde_json::from_value(o["gate_transfer"].clone())?;
            let observed = saved::classify_reentry_gate(home, &before, &transfer)?;
            ensure!(
                observed.stage != saved::GateStage::Restored,
                "Reentry admission was already released; no legacy downgrade"
            );
            if saved::verify_reentry_before_startup(home, &before, &transfer, &observed).is_ok() {
                j["legacy_early_recovery"] = json!(true);
                j["early_gate_present"] = json!(observed.stage == saved::GateStage::Prepared);
                j["initialization_started_at"] = json!(crate::state::now_ms());
                return begin_legacy_restoration(home, &mut j, &o, service).await;
            }
            j["preservation_witness"] = serde_json::to_value(state_proof(home, &j, &o)?)?;
            save(&home.join(JOURNAL), &j)?;
            drop(service);
        } else {
            let gate = early_gate(home, transaction)?;
            if saved::verify_before_startup(home, &before, gate.as_ref()).is_ok() {
                j["legacy_early_recovery"] = json!(true);
                j["early_gate_present"] = json!(gate.is_some());
                j["initialization_started_at"] = json!(crate::state::now_ms());
                return begin_legacy_restoration(home, &mut j, &o, service).await;
            }
            // Controlled initialization may have completed before the next journal
            // write. It must pass the retained post-migration proof; never rerun it.
            j["preservation_witness"] = serde_json::to_value(state_proof(home, &j, &o)?)?;
            save(&home.join(JOURNAL), &j)?;
            drop(service);
        }
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
    j["preservation_witness"] = serde_json::to_value(state_proof(home, &j, &o)?)?;
    save(&home.join(JOURNAL), &j)?;
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
            j["preservation_witness"] = serde_json::to_value(state_proof(home, j, o)?)?;
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

    fn home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        config::private_dir(dir.path()).unwrap();
        let config = Config::local(dir.path(), "Unstarted adoption fixture", 19876).unwrap();
        config.save(dir.path()).unwrap();
        config::private_dir(&dir.path().join("bin")).unwrap();
        config::private_write(&binary(dir.path()), b"unexecuted legacy image").unwrap();
        dir
    }

    fn origin_record(home: &Path, transaction: &str, label: &str) -> Value {
        json!({"schema":1,"kind":"legacy_adoption_origin","id":Config::load(home).unwrap().id,
            "home":home,"transaction_id":transaction,"origin":{"fixture":label,"transaction_id":transaction},
            "snapshot":{"retained_unknown":null},"candidate":{"fixture":label}})
    }

    fn archive_value(home: &Path, transaction: &str, root: &Value) -> Value {
        let capture = json!({"schema":1,"id":Config::load(home).unwrap().id,
            "home":home,"transaction_id":transaction,"origin":root["origin"]});
        let journal = json!({"schema":1,"kind":"legacy_adoption","installation_id":Config::load(home).unwrap().id,
            "transaction_id":transaction,"phase":"restored","direction":"rolled_back",
            "legacy_origin_sha256":record_hash(root).unwrap(),"candidate":root["candidate"],
            "legacy_restore_ack":"accepted","legacy_health":{"authenticated_meta":true}});
        json!({"schema":1,"kind":"legacy_adoption_rollback",
            "id":Config::load(home).unwrap().id,"transaction_id":transaction,
            "root_origin_bytes":String::from_utf8(serde_json::to_vec_pretty(root).unwrap()).unwrap(),
            "root_origin_sha256":record_hash(root).unwrap(),"origin":root,
            "capture_bytes":serde_json::to_string_pretty(&capture).unwrap(),"capture":capture,
            "journal_bytes":serde_json::to_string_pretty(&journal).unwrap(),"journal":journal,
            "unknown_receipt":{"transport_submitted":null}})
    }

    fn retain_archive(home: &Path, transaction: &str, archive: &Value) -> Value {
        let hash = crate::native_browser_update::retain_legacy_adoption(
            home,
            &Config::load(home).unwrap().id,
            transaction,
            archive,
        )
        .unwrap();
        json!({"transaction_id":transaction,"sha256":hash})
    }

    fn archived(home: &Path, transaction: &str, root: &Value) -> Value {
        retain_archive(home, transaction, &archive_value(home, transaction, root))
    }

    #[test]
    fn archived_payload_and_raw_record_identity_mismatch_refuse_without_rewrite() {
        for field in [
            "payload_id",
            "journal_id",
            "capture_inner_id",
            "origin_inner_id",
            "capture_raw",
            "journal_raw",
            "root_hash",
        ] {
            let dir = home();
            let home = dir.path();
            let transaction = uuid::Uuid::new_v4().to_string();
            let root = origin_record(home, &transaction, "old");
            let mut archive = archive_value(home, &transaction, &root);
            match field {
                "payload_id" => archive["id"] = json!("foreign"),
                "journal_id" => archive["journal"]["installation_id"] = json!("foreign"),
                "capture_inner_id" => {
                    archive["capture"]["origin"]["transaction_id"] =
                        json!(uuid::Uuid::new_v4().to_string())
                }
                "origin_inner_id" => {
                    archive["origin"]["origin"]["transaction_id"] =
                        json!(uuid::Uuid::new_v4().to_string())
                }
                "capture_raw" => archive["capture_bytes"] = json!("{}"),
                "journal_raw" => archive["journal_bytes"] = json!("{}"),
                "root_hash" => archive["root_origin_sha256"] = json!("0".repeat(64)),
                _ => unreachable!(),
            }
            let reference = retain_archive(home, &transaction, &archive);
            assert!(predecessor(home, &reference).is_err(), "{field}");
            assert_eq!(
                crate::native_browser_update::retained_legacy_adoption(
                    home,
                    &Config::load(home).unwrap().id,
                    &transaction,
                    reference["sha256"].as_str().unwrap()
                )
                .unwrap(),
                archive
            );
            assert!(!home.join("state.sqlite3").exists());
        }
    }

    #[test]
    fn root_must_match_selected_or_exact_predecessor_before_archival() {
        let dir = home();
        let home = dir.path();
        let old_id = uuid::Uuid::new_v4().to_string();
        let new_id = uuid::Uuid::new_v4().to_string();
        let old = origin_record(home, &old_id, "old");
        let new = origin_record(home, &new_id, "new");
        let third = origin_record(home, &old_id, "unknown root");
        let prior = archived(home, &old_id, &old);
        let j = json!({"legacy_origin_record":new,"legacy_origin_sha256":record_hash(&new).unwrap(),"predecessor":prior});
        create_record(&home.join(ORIGIN), &old).unwrap();
        assert_eq!(
            validated_root_bytes(home, &j, &new).unwrap(),
            fs::read(home.join(ORIGIN)).unwrap()
        );
        save(&home.join(ORIGIN), &new).unwrap();
        assert_eq!(
            validated_root_bytes(home, &j, &new).unwrap(),
            fs::read(home.join(ORIGIN)).unwrap()
        );
        save(&home.join(ORIGIN), &third).unwrap();
        let bytes = fs::read(home.join(ORIGIN)).unwrap();
        assert!(validated_root_bytes(home, &j, &new).is_err());
        assert_eq!(fs::read(home.join(ORIGIN)).unwrap(), bytes);
        assert!(!home.join("state.sqlite3").exists());
    }

    #[test]
    fn capture_inner_transaction_mismatch_refuses_before_database_or_manager() {
        let dir = home();
        let home = dir.path();
        let transaction = uuid::Uuid::new_v4().to_string();
        let capture = json!({"schema":1,"id":Config::load(home).unwrap().id,
            "home":home,"transaction_id":transaction,"origin":{"transaction_id":uuid::Uuid::new_v4().to_string()}});
        create_record(&home.join(CAPTURE), &capture).unwrap();
        let bytes = fs::read(home.join(CAPTURE)).unwrap();
        assert!(
            require_capture(home, &transaction)
                .unwrap_err()
                .to_string()
                .contains("Capture origin")
        );
        assert_eq!(fs::read(home.join(CAPTURE)).unwrap(), bytes);
        assert!(!home.join("state.sqlite3").exists());
    }

    #[test]
    fn pre_journal_exact_backup_is_observed_but_partial_or_changed_bytes_refuse() {
        let home = home();
        let path = home.path().join("native-legacy-pre-journal.bin");
        let original = b"qualified original legacy image";
        retain_legacy_image(&path, original).unwrap();
        let metadata = fs::metadata(&path).unwrap();
        retain_legacy_image(&path, original).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let after = fs::metadata(&path).unwrap();
            assert_eq!(
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec()
                ),
                (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
            );
        }
        #[cfg(not(unix))]
        assert_eq!(metadata.len(), original.len() as u64);
        assert!(retain_legacy_image(&path, b"changed but same length content").is_err());
        assert!(retain_legacy_image(&path, b"partial").is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(retain_legacy_image(&path, original).is_err());
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
            let linked = home.path().join("linked-backup");
            symlink(&path, &linked).unwrap();
            assert!(retain_legacy_image(&linked, original).is_err());
            assert!(linked.is_symlink());
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pending_capture_reconciles_only_same_request_without_touching_state() {
        let home = home();
        let home = home.path();
        let transaction = uuid::Uuid::new_v4().to_string();
        let hash = digest(&binary(home)).unwrap();
        let capture = json!({"schema":1,"id":Config::load(home).unwrap().id,"home":home,
            "transaction_id":transaction,"legacy_binary_sha256":hash});
        create_record(&home.join(CAPTURE), &capture).unwrap();
        let bytes = fs::read(home.join(CAPTURE)).unwrap();
        let result = capture_legacy_origin(home, &transaction, &hash).unwrap();
        assert_eq!(result["reconciled"], true);
        assert_eq!(result["captured"], true);
        assert!(capture_legacy_origin(home, &uuid::Uuid::new_v4().to_string(), &hash).is_err());
        assert_eq!(fs::read(home.join(CAPTURE)).unwrap(), bytes);
        assert!(!home.join(JOURNAL).exists());
        assert!(!home.join("state.sqlite3").exists());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn completed_capture_never_becomes_a_new_start_permission() {
        let home = home();
        let home = home.path();
        let transaction = uuid::Uuid::new_v4().to_string();
        let hash = digest(&binary(home)).unwrap();
        create_record(
            &home.join(CAPTURE),
            &json!({"schema":1,"id":Config::load(home).unwrap().id,
            "home":home,"transaction_id":transaction,"legacy_binary_sha256":hash}),
        )
        .unwrap();
        let journal = json!({"schema":1,"kind":"legacy_adoption","installation_id":Config::load(home).unwrap().id,
            "transaction_id":transaction,"phase":"restored","direction":"rolled_back"});
        save(&home.join(JOURNAL), &journal).unwrap();
        let result = capture_legacy_origin(home, &transaction, &hash).unwrap();
        assert_eq!(result["captured"], false);
        assert_eq!(result["terminal"], true);
        assert_eq!(read(&home.join(JOURNAL)).unwrap(), journal);
    }

    #[test]
    fn next_origin_is_journal_selected_before_root_materialization() {
        let home = home();
        let home = home.path();
        let old_id = uuid::Uuid::new_v4().to_string();
        let new_id = uuid::Uuid::new_v4().to_string();
        let old = origin_record(home, &old_id, "old");
        let new = origin_record(home, &new_id, "new");
        create_record(&home.join(ORIGIN), &old).unwrap();
        let prior = archived(home, &old_id, &old);
        let retained = predecessor(home, &prior).unwrap();
        let j = json!({"kind":"legacy_adoption","transaction_id":new_id,
            "legacy_origin_record":new,"legacy_origin_sha256":record_hash(&new).unwrap(),
            "predecessor":prior,"phase":"legacy_prepared"});
        save(&home.join(JOURNAL), &j).unwrap();
        assert_eq!(wrapper(home).unwrap().0, new);
        assert_eq!(read(&home.join(ORIGIN)).unwrap(), old);
        materialize_origin(home, &j, &new).unwrap();
        assert_eq!(read(&home.join(ORIGIN)).unwrap(), new);
        materialize_origin(home, &j, &new).unwrap(); // observe exact already-published bytes
        assert_eq!(predecessor(home, &prior).unwrap(), retained);
        assert_eq!(read(&home.join(JOURNAL)).unwrap(), j);
        assert!(!home.join(INSTALLED).exists());
    }

    #[test]
    fn foreign_origin_and_changed_history_are_preserved_and_refused() {
        let home = home();
        let home = home.path();
        let old_id = uuid::Uuid::new_v4().to_string();
        let new_id = uuid::Uuid::new_v4().to_string();
        let old = origin_record(home, &old_id, "old");
        let new = origin_record(home, &new_id, "new");
        let foreign = origin_record(home, &old_id, "unrecognized");
        create_record(&home.join(ORIGIN), &foreign).unwrap();
        let prior = archived(home, &old_id, &old);
        let j = json!({"legacy_origin_record":new,"legacy_origin_sha256":record_hash(&new).unwrap(),
            "predecessor":prior});
        assert!(materialize_origin(home, &j, &new).is_err());
        assert_eq!(read(&home.join(ORIGIN)).unwrap(), foreign);
        let mut changed = predecessor(home, &prior).unwrap();
        changed["unknown_receipt"]["transport_submitted"] = json!(true);
        assert!(
            crate::native_browser_update::retain_legacy_adoption(
                home,
                &Config::load(home).unwrap().id,
                &old_id,
                &changed
            )
            .is_err()
        );
        assert!(
            predecessor(
                home,
                &json!({"transaction_id":old_id,"sha256":"0".repeat(64)})
            )
            .is_err()
        );
        assert_eq!(
            predecessor(home, &prior).unwrap()["unknown_receipt"]["transport_submitted"],
            Value::Null
        );
    }

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
