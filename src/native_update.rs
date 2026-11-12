//! Installation-owned whole-bundle updates. No scheduler, agent authority, or
//! generic host-command surface. The same transaction serves standalone and
//! explicitly enrolled installations.
use crate::{
    auth,
    config::{self, Config},
    native_bundle, native_startup_state,
    native_update_process::{self, ServiceChild},
    state::AppState,
    update_admission,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use fs2::FileExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const JOURNAL: &str = "native-update.json";
const INSTALLED: &str = "native-installation.json";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(path: &Path) -> Result<String> {
    ordinary(path)?;
    Ok(hash(&fs::read(path)?))
}
fn ordinary(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_file() && !m.file_type().is_symlink(),
        "Use an ordinary installation file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            m.file_attributes() & 0x400 == 0,
            "Reparse installation files are not supported"
        );
    }
    Ok(())
}
fn read(path: &Path) -> Result<Value> {
    ordinary(path)?;
    ensure!(
        path.metadata()?.len() < 1024 * 1024,
        "Installation record too large"
    );
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn save(path: &Path, v: &Value) -> Result<()> {
    // Startup holds this same guard from its final ticket check through database
    // initialization. All journal publication (including first prepare intent)
    // participates; never retain the guard across a service stop or lock wait.
    let _launch = if path.file_name() == Some(std::ffi::OsStr::new(JOURNAL)) {
        Some(launch_guard(
            path.parent().context("Missing installation home")?,
        )?)
    } else {
        None
    };
    let context = match path.file_name().and_then(|v| v.to_str()) {
        Some(JOURNAL) => "Cannot publish native update journal",
        Some(INSTALLED) => "Cannot commit installed bundle receipt",
        _ => "Cannot persist native installation receipt",
    };
    // These callers publish mutable JSON journals, installation receipts and
    // Windows process-exit evidence. Existing readers may retain their snapshot;
    // executable replacement and create-only ownership keep their strict APIs.
    amplifier_native_platform::private_write_data(path, &serde_json::to_vec_pretty(v)?)
        .context(context)
}
fn id(value: &str) -> Result<()> {
    uuid::Uuid::parse_str(value)?;
    Ok(())
}
fn path(v: &Value, name: &str) -> Result<PathBuf> {
    let p = PathBuf::from(v[name].as_str().context("Missing installation path")?);
    ensure!(p.is_absolute(), "Installation paths must be absolute");
    Ok(p)
}
fn binary(home: &Path) -> PathBuf {
    amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, home)
}
/// An owned installation lock, released before its file descriptor is closed.
///
/// On Unix, a child between fork and exec can retain the same open file
/// description despite CLOEXEC. Closing only this process's descriptor would
/// leave its flock held until that child closes the inherited descriptor.
#[must_use = "Keep the guard alive for the entire installation operation"]
pub struct InstallationLock {
    file: File,
}
impl Drop for InstallationLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            // Drop cannot report an error to the operation. Keep ordinary file
            // close as the fallback; a subsequent acquisition still fails closed
            // if an inherited descriptor keeps the lock alive.
            eprintln!("Could not explicitly release installation lock: {error}");
        }
    }
}
pub(crate) fn lock(home: &Path, name: &str) -> Result<InstallationLock> {
    let p = home.join(name);
    if !p.exists() {
        match amplifier_native_platform::private_create(&p, b"") {
            Ok(()) => {}
            Err(e) if p.exists() => {
                let _ = e;
            }
            Err(e) => return Err(e),
        }
    }
    ordinary(&p)?;
    let f = fs::OpenOptions::new().read(true).write(true).open(p)?;
    f.try_lock_exclusive()
        .context("This installation operation is already running")?;
    Ok(InstallationLock { file: f })
}
fn launch_guard(home: &Path) -> Result<InstallationLock> {
    launch_guard_waiting(home, || {})
}
fn launch_guard_waiting(home: &Path, waiting: impl FnOnce()) -> Result<InstallationLock> {
    // The spawned service may already be migrating its database when its parent
    // records child_pid. Allow a healthy initialization, bounded by the whole
    // participant budget or by thirty seconds for standalone startup/update.
    let deadline =
        std::time::Instant::now() + crate::native_update_clock::cap(Duration::from_secs(30))?;
    let mut waiting = Some(waiting);
    loop {
        crate::native_update_clock::check()?;
        ensure!(
            std::time::Instant::now() < deadline,
            "Startup authorization is busy"
        );
        match lock(home, "native-launch.lock") {
            Ok(guard) => return Ok(guard),
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                }) && std::time::Instant::now() < deadline =>
            {
                if let Some(waiting) = waiting.take() {
                    waiting();
                }
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
            }
            Err(error) => return Err(error.context("Startup authorization is busy")),
        }
    }
}
pub fn startup_change_guard(home: &Path) -> Result<InstallationLock> {
    let guard = lock(home, "native-update.lock")?;
    if home.join(JOURNAL).exists() {
        ensure!(
            terminal(&read(&home.join(JOURNAL))?),
            "Finish the retained update before changing startup"
        );
    }
    Ok(guard)
}
fn write_binary(from: &Path, to: &Path) -> Result<()> {
    ordinary(from)?;
    config::private_dir(to.parent().context("Missing binary folder")?)?;
    let bytes = fs::read(from).context("Cannot read selected native service executable")?;
    config::private_write(to, &bytes).context("Cannot replace native service executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn package_record(receipt: Value) -> Result<Value> {
    let service = path(&receipt["entrypoints"], "service")?;
    Ok(json!({"bundle":receipt,"binary_sha256":digest(&service)?}))
}
fn check_package(record: &Value) -> Result<()> {
    let receipt = native_bundle::verify(&path(&record["bundle"], "bundle")?)?;
    ensure!(
        receipt == record["bundle"]
            && digest(&path(&receipt["entrypoints"], "service")?)? == record["binary_sha256"],
        "Frozen package changed"
    );
    Ok(())
}
fn companion(home: &Path) -> Result<Option<Vec<u8>>> {
    let p = home.join("companion.json");
    if !p.exists() {
        return Ok(None);
    }
    ordinary(&p)?;
    let bytes = fs::read(p)?;
    std::str::from_utf8(&bytes).context("Companion configuration is not UTF-8; preserved")?;
    Ok(Some(bytes))
}
fn rebind(bytes: Option<&[u8]>, previous: &Value, candidate: &Value) -> Result<Option<Vec<u8>>> {
    let Some(bytes) = bytes else { return Ok(None) };
    let mut v: Value = serde_json::from_slice(bytes)?;
    for key in ["node", "bridge"] {
        let old = path(&previous["bundle"]["entrypoints"], key)?;
        let configured = path(&v, key)?;
        ensure!(
            dunce::canonicalize(&configured)? == dunce::canonicalize(old)?
                || dunce::canonicalize(configured)?
                    == dunce::canonicalize(path(&candidate["bundle"]["entrypoints"], key)?)?,
            "Companion {key} was customized; its binding was preserved"
        );
        v[key] = candidate["bundle"]["entrypoints"][key].clone();
    }
    Ok(Some(serde_json::to_vec_pretty(&v)?))
}
fn installed(home: &Path) -> Result<Value> {
    let v = read(&home.join(INSTALLED))?;
    ensure!(
        v["schema"] == 1 && v["id"] == Config::load(home)?.id,
        "Installation identity changed"
    );
    check_package(&v["package"])?;
    ensure!(
        digest(&binary(home))? == v["package"]["binary_sha256"],
        "Installed application changed"
    );
    Ok(v)
}

/// Explicit first-install adoption while stopped. Does not register startup or
/// enable voice/updates, and never replaces a different existing application.
pub fn bootstrap(home: &Path, package: &Path) -> Result<Value> {
    let _owner = lock(home, "native-update.lock")?;
    let _service = lock(home, "service.lock")?;
    let c = Config::load(home)?;
    let incoming = package_record(native_bundle::verify(package)?)?;
    let selected = package_record(
        native_bundle::install(package, home).context("Cannot stage immutable native bundle")?,
    )?;
    if home.join(INSTALLED).exists() {
        let old = installed(home)?;
        ensure!(
            old["package"] == selected,
            "This installation already has a baseline; stage an update instead"
        );
        return Ok(old);
    }
    let before = companion(home)?;
    let after = rebind(before.as_deref(), &incoming, &selected)?;
    let service = binary(home);
    if service.exists() {
        ensure!(
            digest(&service)? == selected["binary_sha256"],
            "Existing executable preserved"
        );
    } else {
        write_binary(
            &path(&selected["bundle"]["entrypoints"], "service")?,
            &service,
        )?;
    }
    // A previous bootstrap may have published the private binary before its
    // execute permission was applied. Only the verified selected bytes reach
    // this point; finish that step durably before committing the receipt.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&service, fs::Permissions::from_mode(0o700))
            .context("Cannot finish native service executable permissions")?;
        File::open(&service)?
            .sync_all()
            .context("Cannot persist native service executable permissions")?;
    }
    if let Some(bytes) = after {
        config::private_write(&home.join("companion.json"), &bytes)
            .context("Cannot commit Companion bundle bindings")?;
    }
    let record = json!({"schema":1,"id":c.id,"package":selected});
    save(&home.join(INSTALLED), &record)?;
    Ok(record)
}

pub fn plan(home: &Path, package: &Path) -> Result<Value> {
    let _owner = lock(home, "native-update.lock")?;
    let old = installed(home)?;
    if home.join(JOURNAL).exists() {
        ensure!(
            terminal(&read(&home.join(JOURNAL))?),
            "Finish the retained update before planning another"
        );
    }
    let candidate = package_record(
        native_bundle::install(package, home).context("Cannot stage immutable native bundle")?,
    )?;
    let before = companion(home)?;
    let _ = rebind(before.as_deref(), &old["package"], &candidate)?;
    Ok(
        json!({"schema":1,"installation_id":old["id"],"previous":old["package"],"candidate":candidate,"config_sha256":digest(&home.join("config.json"))?,"companion_sha256":before.as_ref().map(|b|hash(b)),"available":old["package"]!=candidate,"version":candidate["bundle"]["version"]}),
    )
}
fn terminal(j: &Value) -> bool {
    j["phase"] == "restored"
}
fn journal(home: &Path, transaction: &str) -> Result<Value> {
    id(transaction)?;
    let j = read(&home.join(JOURNAL))?;
    ensure!(
        j["schema"] == 1
            && j["transaction_id"] == transaction
            && j["installation_id"] == Config::load(home)?.id,
        "Update transaction changed"
    );
    Ok(j)
}
fn phase(home: &Path, j: &mut Value, name: &str) -> Result<()> {
    j["phase"] = json!(name);
    save(&home.join(JOURNAL), j)
}
fn current_record(j: &Value) -> &Value {
    if j["selected"] == "previous" || j["candidate"].is_null() {
        &j["previous"]
    } else {
        j.get("candidate")
            .filter(|v| !v.is_null())
            .unwrap_or(&j["previous"])
    }
}
pub fn status(home: &Path) -> Result<Value> {
    let c = Config::load(home)?;
    let i = read(&home.join(INSTALLED))?;
    let j = if home.join(JOURNAL).exists() {
        read(&home.join(JOURNAL))?
    } else {
        Value::Null
    };
    Ok(
        json!({"schema":1,"id":c.id,"version":i["package"]["bundle"]["version"],"phase":j["phase"],"transaction_id":j["transaction_id"],"context_id":j["context"]["id"],"context_hash":j["context_hash"],"original_running":j["original_running"],"selected_manifest":i["package"]["bundle"]["manifest_sha256"]}),
    )
}

pub async fn local_api(
    home: &Path,
    method: &str,
    endpoint: &str,
    body: Option<Value>,
) -> Result<Value> {
    crate::native_update_clock::check()?;
    let c = Config::load(home)?;
    let r = read(&home.join("runtime.json"))?;
    let port = r["port"].as_u64().context("No recorded listener")?;
    ensure!((1..=65535).contains(&port), "Invalid local listener");
    let ca = reqwest::Certificate::from_pem(&fs::read(c.tls_ca.unwrap_or(c.tls_cert))?)?;
    let client = reqwest::Client::builder()
        .add_root_certificate(ca)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(crate::native_update_clock::cap(Duration::from_secs(3))?)
        .build()?;
    let token = fs::read_to_string(home.join("secrets/control-token"))?;
    let mut req = client
        .request(
            method.parse()?,
            format!("https://127.0.0.1:{port}/api/v1/native-update/{endpoint}"),
        )
        .bearer_auth(token.trim());
    if let Some(body) = body {
        req = req.json(&body);
    }
    let response = req.send().await?;
    let code = response.status();
    let v: Value = response.json().await?;
    ensure!(
        code.is_success(),
        "Local lifecycle request failed ({code}): {}",
        v["error"].as_str().unwrap_or("not accepted")
    );
    Ok(v)
}
async fn wait_stopped(home: &Path) -> Result<InstallationLock> {
    let deadline =
        std::time::Instant::now() + crate::native_update_clock::cap(Duration::from_secs(15))?;
    loop {
        crate::native_update_clock::check()?;
        if let Ok(f) = lock(home, "service.lock") {
            return Ok(f);
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "The owned application has not stopped; no files were switched"
        );
        tokio::time::sleep(Duration::from_millis(75)).await;
    }
}
async fn stop_running(home: &Path, j: &Value) -> Result<InstallationLock> {
    if let Ok(f) = lock(home, "service.lock") {
        if j["startup"]["managed"] == true {
            native_startup_state::stop(&j["startup"])?;
        }
        // Windows keeps an executable image mapped after the service lock may
        // have been released. Reopen the durable exact-generation receipt when
        // recovering; a lock alone is not proof that replacement is possible.
        #[cfg(windows)]
        windows_exit::reconcile(home, j).await?;
        return Ok(f);
    }
    let s = local_api(home, "GET", "status", None).await?;
    ensure!(
        s["id"] == j["installation_id"]
            && (s["binary_sha256"] == j["previous"]["binary_sha256"]
                || s["binary_sha256"] == j["candidate"]["binary_sha256"]),
        "Different process owns this listener"
    );
    ensure!(
        s["admission"]["ready"] == true
            && s["admission"]["transaction_id"] == j["transaction_id"]
            && s["live_work"] == false,
        "Application work has not settled"
    );
    #[cfg(windows)]
    let exit = windows_exit::retain(home, j, &s).await?;
    // A lost acknowledgement is reconciled using the lock/actual exit, never a
    // retry of application work or a PID-only termination.
    if j["startup"]["managed"] == true {
        native_startup_state::stop(&j["startup"])?;
    } else {
        let _ = local_api(
            home,
            "POST",
            "stop",
            Some(json!({"transaction_id":j["transaction_id"],"process_nonce":s["process_nonce"]})),
        )
        .await;
    }
    let service = wait_stopped(home).await?;
    #[cfg(windows)]
    {
        if let Some(exit) = exit {
            exit.wait().await?;
        }
        windows_exit::confirm(home, j)?;
    }
    Ok(service)
}
pub async fn prepare(home: &Path, transaction: &str, context: &Value) -> Result<Value> {
    fn defer(home: &Path, j: &mut Value, readiness: Value, release_gate: bool) -> Result<Value> {
        // No stop has been requested in intent/deferred. A retry uses the same
        // transaction; do not terminalize it or clear a different owner's gate.
        if release_gate {
            update_admission::defer(
                home,
                j["transaction_id"]
                    .as_str()
                    .context("Missing transaction")?,
            )?;
        }
        j["admission_result"] = readiness.clone();
        phase(home, j, "deferred")?;
        Ok(readiness)
    }
    let _owner = lock(home, "native-update.lock")?;
    if context["owner"] == "standalone" {
        // Enrollment may have won the same lock after standalone planning but
        // before preparation. Recheck before writing intent or acquiring a gate.
        ensure!(
            !home.join("native-update-owner.json").exists(),
            "This home uses its enrolled updater; use that updater's existing controls"
        );
    }
    id(transaction)?;
    let context_hash = hash(&serde_json::to_vec(context)?);
    let existing = if home.join(JOURNAL).exists() {
        Some(read(&home.join(JOURNAL))?)
    } else {
        None
    };
    let previous_startup = existing
        .as_ref()
        .map(|j| {
            if j["startup"].is_null() {
                j["previous_startup"].clone()
            } else {
                j["startup"].clone()
            }
        })
        .unwrap_or(Value::Null);
    let mut j = if let Some(j) = existing.filter(|j| j["transaction_id"] == transaction) {
        ensure!(
            j["context_hash"] == context_hash && j["installation_id"] == Config::load(home)?.id,
            "Immutable update context or installation changed"
        );
        if terminal(&j) {
            return Ok(json!({"ready":false,"reason":"transaction_finished"}));
        }
        if j["phase"] == "preparing" {
            let readiness = update_admission::status(home)?;
            ensure!(
                readiness["ready"] == true && readiness["transaction_id"] == transaction,
                "The prepared admission boundary no longer matches; retained for recovery"
            );
            let _service = stop_running(home, &j).await?;
            let mut j = j;
            phase(home, &mut j, "prepared")?;
            return Ok(json!({"ready":true,"transaction_id":transaction}));
        }
        if !matches!(j["phase"].as_str(), Some("intent" | "deferred")) {
            return Ok(json!({"ready":true,"transaction_id":transaction}));
        }
        j
    } else {
        if home.join(JOURNAL).exists() {
            ensure!(
                terminal(&read(&home.join(JOURNAL))?),
                "Another transaction remains unresolved"
            );
        }
        let old = installed(home)?;
        json!({"schema":1,"transaction_id":transaction,"installation_id":old["id"],"context":context,"context_hash":context_hash,"phase":"intent","previous":old["package"],"previous_startup":previous_startup,"candidate":null,"direction":"unchanged","selected":"previous","original_running":null,"stop_requested":false,"launch_nonce":null,"restore_started":false})
    };
    // The recoverable intent must exist BEFORE a durable gate can be acquired.
    // A busy attempt has changed no application work or service availability.
    phase(home, &mut j, "intent")?;
    let readiness = update_admission::prepare(home, transaction)?;
    if readiness["ready"] != true {
        return defer(home, &mut j, readiness, false);
    }
    let captured = (|| -> Result<(Value, Option<Vec<u8>>, String, Value)> {
        Ok((
            installed(home)?,
            companion(home)?,
            digest(&home.join("config.json"))?,
            native_startup_state::capture(home, &Config::load(home)?.id)?,
        ))
    })();
    let (old, before, config_hash, startup) = match captured {
        Ok(captured) => captured,
        Err(error) => {
            defer(
                home,
                &mut j,
                json!({"ready":false,"reason":"installation_unavailable_before_stop"}),
                true,
            )?;
            return Err(error);
        }
    };
    // Retain a successful lock until prepared is persisted. A competing normal
    // startup must never turn an originally stopped installation into one we stop.
    let stopped = lock(home, "service.lock").ok();
    let running = stopped.is_none();
    let mut original_process = Value::Null;
    if running {
        match local_api(home, "GET", "status", None).await {
            Ok(s)
                if s["id"] == old["id"]
                    && s["binary_sha256"] == old["package"]["binary_sha256"]
                    && (startup["managed"] != true
                        || startup["manager_pid"].is_null()
                        || startup["manager_pid"] == s["pid"]) =>
            {
                original_process = s;
            }
            _ => {
                return defer(
                    home,
                    &mut j,
                    json!({"ready":false,"reason":"unmanaged_or_unreachable_application"}),
                    true,
                );
            }
        }
    }
    if !running && startup["managed"] == true {
        return defer(
            home,
            &mut j,
            json!({"ready":false,"reason":"startup_manager_and_application_disagree"}),
            true,
        );
    }
    j["startup"] = startup;
    j["original_process"] = original_process;
    j["previous"] = old["package"].clone();
    j["config_sha256"] = json!(config_hash);
    j["companion_before"] = before
        .as_ref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .map_or(Value::Null, Value::String);
    j["original_running"] = json!(running);
    j["stop_requested"] = json!(running);
    j["admission_result"] = readiness;
    phase(home, &mut j, "preparing")?;
    if running {
        let _service = stop_running(home, &j).await?;
        phase(home, &mut j, "prepared")?;
    } else {
        phase(home, &mut j, "prepared")?;
    }
    Ok(json!({"ready":true,"transaction_id":transaction}))
}
fn binding_bytes(j: &Value, rollback: bool) -> Result<Option<Vec<u8>>> {
    let before = j["companion_before"].as_str().map(str::as_bytes);
    if rollback {
        Ok(before.map(<[u8]>::to_vec))
    } else {
        rebind(before, &j["previous"], &j["candidate"])
    }
}
fn switch(home: &Path, j: &Value, rollback: bool) -> Result<()> {
    ensure!(
        digest(&home.join("config.json"))? == j["config_sha256"],
        "Installation configuration changed; preserved"
    );
    let selected = if rollback {
        &j["previous"]
    } else {
        &j["candidate"]
    };
    check_package(selected)?;
    let actual = digest(&binary(home))?;
    ensure!(
        actual == j["previous"]["binary_sha256"] || actual == j["candidate"]["binary_sha256"],
        "Owned binary changed; preserved"
    );
    let old = binding_bytes(j, true)?;
    let new = binding_bytes(j, false)?;
    let current = companion(home)?;
    ensure!(
        current == old || current == new,
        "Companion configuration changed; preserved"
    );
    write_binary(
        &path(&selected["bundle"]["entrypoints"], "service")?,
        &binary(home),
    )?;
    if let Some(bytes) = if rollback { old } else { new } {
        config::private_write(&home.join("companion.json"), &bytes)
            .context("Cannot commit Companion bundle bindings")?;
    }
    save(
        &home.join(INSTALLED),
        &json!({"schema":1,"id":j["installation_id"],"package":selected}),
    )
}
pub async fn apply(home: &Path, transaction: &str, plan: &Value, rollback: bool) -> Result<Value> {
    let _owner = lock(home, "native-update.lock")?;
    let mut j = journal(home, transaction)?;
    ensure!(
        j["restore_started"] != true && j["rollback_aborted"] != true && !terminal(&j),
        "Restoration or rollback abort began; only that recovery may be retried"
    );
    ensure!(
        matches!(
            j["phase"].as_str(),
            Some("prepared" | "applied" | "switching" | "verifying" | "verified")
        ),
        "Preparation has not established an idle boundary"
    );
    let admission = update_admission::status(home)?;
    ensure!(
        admission["ready"] == true && admission["transaction_id"] == transaction,
        "The transaction no longer owns its settled admission boundary"
    );
    let explicit_rollback = rollback && j["context"]["plan"]["operation"] == "rollback";
    if explicit_rollback {
        ensure!(
            plan["schema"] == 1
                && plan["installation_id"] == j["installation_id"]
                && plan["candidate"] == j["previous"]
                && plan["config_sha256"] == j["config_sha256"],
            "Frozen rollback plan does not match current installation"
        );
        if !j["candidate"].is_null() {
            ensure!(
                j["candidate"] == plan["previous"],
                "Rollback target changed"
            );
        }
        j["candidate"] = plan["previous"].clone();
    } else if !rollback {
        ensure!(
            plan["schema"] == 1
                && plan["installation_id"] == j["installation_id"]
                && plan["previous"] == j["previous"]
                && plan["config_sha256"] == j["config_sha256"],
            "Frozen update plan differs from prepared installation"
        );
        ensure!(
            plan["companion_sha256"]
                == j["companion_before"]
                    .as_str()
                    .map(|b| hash(b.as_bytes()))
                    .map_or(Value::Null, Value::String),
            "Frozen Companion binding changed"
        );
        if !j["candidate"].is_null() {
            ensure!(
                j["candidate"] == plan["candidate"],
                "Candidate changed during transaction"
            );
        }
        j["candidate"] = plan["candidate"].clone();
    } else if j["candidate"].is_null() {
        j["candidate"] = j["previous"].clone();
    }
    check_package(&j["previous"])?;
    check_package(&j["candidate"])?;
    if j["frozen_plan"].is_null() && !plan.is_null() {
        j["frozen_plan"] = plan.clone();
    }
    j["direction"] = json!(if rollback { "rolled_back" } else { "applied" });
    let previous = rollback && !explicit_rollback;
    j["selected"] = json!(if previous { "previous" } else { "candidate" });
    // Journal publication serializes revocation against the final startup
    // check/AppState initialization, then releases that guard BEFORE waiting
    // for service.lock. A revoked process still hashing its executable refuses.
    j["launch_nonce"] = Value::Null;
    phase(home, &mut j, "switching")?;
    let _service = stop_running(home, &j).await?;
    switch(home, &j, previous)?;
    phase(home, &mut j, "applied")?;
    Ok(json!({"applied":!rollback,"rolled_back":rollback,"transaction_id":transaction}))
}

/// Called AFTER acquiring service.lock and BEFORE AppState::open/recovery.
pub fn managed_startup_ticket(home: &Path) -> Result<Option<(String, String)>> {
    if !home.join(JOURNAL).exists() {
        return Ok(None);
    }
    let j = read(&home.join(JOURNAL))?;
    if j["managed_start_requested"] == true
        && matches!(j["phase"].as_str(), Some("verifying" | "restoring"))
    {
        ensure!(
            j["startup"]["managed"] == true,
            "No managed startup was captured"
        );
        return Ok(Some((
            j["transaction_id"]
                .as_str()
                .context("Missing transaction")?
                .to_owned(),
            j["launch_nonce"]
                .as_str()
                .context("Missing startup nonce")?
                .to_owned(),
        )));
    }
    Ok(None)
}

/// Called AFTER acquiring service.lock. Retain the returned guard through
/// AppState::open/recovery so journal revocation cannot overtake initialization.
pub fn validate_start(home: &Path, startup: Option<(&str, &str)>) -> Result<InstallationLock> {
    validate_start_with_digest(home, startup, || digest(&std::env::current_exe()?))
}
fn validate_start_with_digest(
    home: &Path,
    startup: Option<(&str, &str)>,
    executable_digest: impl FnOnce() -> Result<String>,
) -> Result<InstallationLock> {
    // Reject already-invalid launches before hashing, but do not confuse that
    // observation with permission to initialize after slow executable I/O.
    validate_start_record(home, startup, None)?;
    let actual = startup.map(|_| executable_digest()).transpose()?;
    let guard = launch_guard(home)?;
    validate_start_record(home, startup, actual.as_deref())?;
    Ok(guard)
}
fn validate_start_record(
    home: &Path,
    startup: Option<(&str, &str)>,
    executable_digest: Option<&str>,
) -> Result<()> {
    if !home.join(JOURNAL).exists() {
        ensure!(startup.is_none(), "No update startup is pending");
        return Ok(());
    }
    let j = read(&home.join(JOURNAL))?;
    match startup {
        None => ensure!(
            terminal(&j)
                || (j["phase"] == "deferred" && update_admission::status(home)?["closed"] == false),
            "An update owns this installation; recover that transaction first"
        ),
        Some((transaction, nonce)) => {
            ensure!(
                j["transaction_id"] == transaction
                    && j["launch_nonce"] == nonce
                    && matches!(j["phase"].as_str(), Some("verifying" | "restoring")),
                "This verification startup is no longer authorized"
            );
            if let Some(actual) = executable_digest {
                ensure!(
                    actual == current_record(&j)["binary_sha256"],
                    "Verification executable changed"
                );
            }
        }
    }
    Ok(())
}
fn start(home: &Path, j: &mut Value) -> Result<Option<ServiceChild>> {
    let nonce = auth::random_token();
    j["launch_nonce"] = json!(nonce);
    j["child_pid"] = Value::Null;
    j["restore_boundary_verified"] = json!(false);
    j["managed_start_requested"] =
        json!(j["original_running"] == true && j["startup"]["managed"] == true);
    save(&home.join(JOURNAL), j)?;
    if j["managed_start_requested"] == true {
        native_startup_state::start(&j["startup"])?;
        return Ok(None);
    }
    let mut child = native_update_process::spawn_service(
        &binary(home),
        home,
        j["transaction_id"].as_str().unwrap(),
        &nonce,
    )?;
    j["child_pid"] = json!(child.id());
    if let Err(e) = save(&home.join(JOURNAL), j) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(Some(child))
}
fn stop_child(child: &mut ServiceChild) -> Result<()> {
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    child.wait()?;
    Ok(())
}
async fn healthy(home: &Path, j: &Value, mut child: Option<&mut ServiceChild>) -> Result<Value> {
    let deadline =
        std::time::Instant::now() + crate::native_update_clock::cap(Duration::from_secs(30))?;
    loop {
        crate::native_update_clock::check()?;
        if let Some(c) = child.as_deref_mut() {
            ensure!(
                c.try_wait()?.is_none(),
                "Candidate exited before verification"
            );
        }
        if let Ok(s) = local_api(home, "GET", "status", None).await
            && s["id"] == j["installation_id"]
            && s["binary_sha256"] == current_record(j)["binary_sha256"]
            && s["transaction_id"] == j["transaction_id"]
            && s["launch_nonce"] == j["launch_nonce"]
            && (s["admission"]["closed"] == true
                || (j["restore_started"] == true && s["admission"]["closed"] == false))
        {
            return Ok(s);
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "Candidate did not prove installation and package identity"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub async fn verify(home: &Path, transaction: &str) -> Result<Value> {
    let pending = journal(home, transaction)?;
    if matches!(pending["phase"].as_str(), Some("intent" | "preparing")) {
        prepare(home, transaction, &pending["context"]).await?;
    }
    let _owner = lock(home, "native-update.lock")?;
    let mut j = journal(home, transaction)?;
    if j["phase"] == "deferred" {
        ensure!(
            update_admission::status(home)?["closed"] == false,
            "Declined transaction unexpectedly owns an admission gate"
        );
        return Ok(json!({"verified":true,"transaction_id":transaction,"deferred":true}));
    }
    if j["phase"] == "verified" {
        return Ok(json!({"verified":true,"transaction_id":transaction}));
    }
    ensure!(
        j["restore_started"] != true
            && matches!(
                j["phase"].as_str(),
                Some("prepared" | "applied" | "verifying")
            ),
        "Transaction cannot enter verification"
    );
    check_package(current_record(&j))?;
    if !j["startup"].is_null() {
        native_startup_state::validate(&j["startup"])?;
    }
    ensure!(
        digest(&binary(home))? == current_record(&j)["binary_sha256"],
        "Installed binary differs from selected bundle"
    );
    ensure!(
        digest(&home.join("config.json"))? == j["config_sha256"],
        "Configuration changed"
    );
    let mut child = None;
    if lock(home, "service.lock").is_ok() {
        phase(home, &mut j, "verifying")?;
        child = start(home, &mut j)?;
    } else {
        ensure!(
            j["phase"] == "verifying",
            "Unexpected running application during verification"
        );
    }
    let result = healthy(home, &j, child.as_mut()).await;
    if let Err(e) = result {
        if let Some(c) = child.as_mut() {
            stop_child(c)?;
        }
        return Err(e);
    }
    if j["original_running"] == false {
        let _stopped = stop_running(home, &j).await?;
        if let Some(c) = child.as_mut() {
            c.wait()?;
        }
    }
    phase(home, &mut j, "verified")?;
    Ok(
        json!({"verified":true,"transaction_id":transaction,"original_running":j["original_running"]}),
    )
}
pub async fn restore(home: &Path, transaction: &str, outcome: &str) -> Result<Value> {
    let _owner = lock(home, "native-update.lock")?;
    let mut j = journal(home, transaction)?;
    ensure!(
        j["rollback_aborted"] != true || outcome == "unchanged",
        "An aborted rollback can only restore unchanged"
    );
    if j["phase"] == "deferred" {
        ensure!(
            outcome == "unchanged" && update_admission::status(home)?["closed"] == false,
            "A declined preparation can only settle unchanged without a gate"
        );
        j["restore_outcome"] = json!(outcome);
        phase(home, &mut j, "restored")?;
        return Ok(json!({"restored":true,"transaction_id":transaction,"deferred":true}));
    }
    ensure!(
        matches!(outcome, "unchanged" | "applied" | "rolled_back")
            && (j["direction"] == outcome || j["direction"] == "unchanged"),
        "Restoration outcome differs from verified direction"
    );
    // A shared-only update changes no local bundle. Its overall outcome still
    // belongs to the immutable transaction and must agree on every retry.
    if let Some(previous) = j["restore_outcome"].as_str() {
        ensure!(previous == outcome, "Restoration outcome changed");
    }
    if terminal(&j) {
        return Ok(json!({"restored":true,"transaction_id":transaction}));
    }
    ensure!(
        j["phase"] == "verified" || j["restore_started"] == true,
        "Verification required before restoration"
    );
    j["restore_started"] = json!(true);
    j["restore_outcome"] = json!(outcome);
    phase(home, &mut j, "restoring")?;
    if j["original_running"] == true {
        let mut child = None;
        if lock(home, "service.lock").is_ok() {
            child = start(home, &mut j)?;
        }
        healthy(home, &j, child.as_mut()).await?;
        if j["restore_boundary_verified"] != true {
            if !j["startup"].is_null() {
                native_startup_state::verify_restored(&j["startup"])?;
            }
            j["restore_boundary_verified"] = json!(true);
            save(&home.join(JOURNAL), &j)?;
        }
        local_api(
            home,
            "POST",
            "finish",
            Some(json!({"transaction_id":transaction,"launch_nonce":j["launch_nonce"]})),
        )
        .await?;
    } else {
        let _stopped = lock(home, "service.lock")?;
        if j["restore_boundary_verified"] != true {
            if !j["startup"].is_null() {
                native_startup_state::verify_restored(&j["startup"])?;
            }
            j["restore_boundary_verified"] = json!(true);
            save(&home.join(JOURNAL), &j)?;
        }
        update_admission::restore(home, transaction)?;
    }
    phase(home, &mut j, "restored")?;
    Ok(json!({"restored":true,"transaction_id":transaction}))
}
fn standalone_worker(home: &Path) -> Result<()> {
    ensure!(
        !home.join("native-update-owner.json").exists(),
        "This home uses its enrolled updater; use that updater's existing controls"
    );
    ensure!(
        dunce::canonicalize(std::env::current_exe()?)? != dunce::canonicalize(binary(home))?,
        "Run the updater from the new package, not the installed executable it replaces"
    );
    Ok(())
}
pub async fn run(home: &Path, package: &Path) -> Result<Value> {
    standalone_worker(home)?;
    let p = plan(home, package)?;
    if p["available"] == false {
        return Ok(json!({"changed":false,"version":p["version"]}));
    }
    let transaction = uuid::Uuid::new_v4().to_string();
    let ready = prepare(
        home,
        &transaction,
        &json!({"id":transaction,"owner":"standalone"}),
    )
    .await?;
    if ready["ready"] != true {
        verify(home, &transaction).await?;
        restore(home, &transaction, "unchanged").await?;
        return Ok(ready);
    }
    if let Err(error) = async {
        apply(home, &transaction, &p, false).await?;
        verify(home, &transaction).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await
    {
        apply(home, &transaction, &p, true)
            .await
            .context("Update failed; rollback bundle switch also failed")?;
        verify(home, &transaction)
            .await
            .context("Update failed; previous bundle verification also failed")?;
        restore(home, &transaction, "rolled_back")
            .await
            .context("Update failed; previous availability restoration also failed")?;
        anyhow::bail!("Update failed and previous bundle was restored: {error}");
    }
    restore(home, &transaction, "applied").await
}

/// Abort a standalone explicit B -> A rollback back to its captured B bundle.
/// Selection is durable before files move; verification/restoration are separate
/// and admission remains closed. An enrolled owner cannot use this path.
pub async fn abort_rollback(home: &Path, transaction: &str, reason: &str) -> Result<Value> {
    standalone_worker(home)?;
    let _owner = lock(home, "native-update.lock")?;
    let mut j = journal(home, transaction)?;
    ensure!(
        j["context"]["owner"] == "standalone" && j["context"]["plan"]["operation"] == "rollback",
        "This is not a standalone explicit rollback transaction"
    );
    ensure!(
        j["restore_started"] != true && !terminal(&j),
        "Restoration began; only restoration may be retried"
    );
    ensure!(
        matches!(
            j["phase"].as_str(),
            Some("prepared" | "applied" | "switching" | "verifying" | "verified")
        ),
        "Preparation has not established an idle boundary"
    );
    let admission = update_admission::status(home)?;
    ensure!(
        admission["ready"] == true && admission["transaction_id"] == transaction,
        "The rollback no longer owns its settled admission boundary"
    );
    check_package(&j["previous"])?;
    if j["rollback_aborted"] == true {
        ensure!(
            j["selected"] == "previous" && j["direction"] == "unchanged",
            "The retained rollback abort selection changed"
        );
        if matches!(
            j["phase"].as_str(),
            Some("applied" | "verifying" | "verified")
        ) {
            return Ok(
                json!({"selected_previous":true,"rollback_aborted":true,"transaction_id":transaction}),
            );
        }
    }
    // A crash before the first apply may not yet have a candidate. B is still
    // the only selected bundle; do not fabricate or discover an A replacement.
    if j["candidate"].is_null() {
        j["candidate"] = j["previous"].clone();
    }
    j["rollback_aborted"] = json!(true);
    if j["rollback_failure"].is_null() {
        j["rollback_failure"] = json!(reason.chars().take(2000).collect::<String>());
    }
    j["direction"] = json!("unchanged");
    j["selected"] = json!("previous");
    j["launch_nonce"] = Value::Null;
    phase(home, &mut j, "switching")?;
    let _service = stop_running(home, &j).await?;
    switch(home, &j, true)?;
    phase(home, &mut j, "applied")?;
    Ok(json!({"selected_previous":true,"rollback_aborted":true,"transaction_id":transaction}))
}

async fn restore_aborted_rollback(home: &Path, transaction: &str, reason: &str) -> Result<Value> {
    abort_rollback(home, transaction, reason).await?;
    verify(home, transaction).await?;
    let mut result = restore(home, transaction, "unchanged").await?;
    result["rollback_aborted"] = json!(true);
    result["outcome"] = json!("unchanged");
    Ok(result)
}

async fn continue_explicit_rollback(home: &Path, transaction: &str) -> Result<Value> {
    let j = journal(home, transaction)?;
    let result = async {
        if matches!(j["phase"].as_str(), Some("prepared" | "switching")) {
            let plan = if j["frozen_plan"].is_null() {
                &j["context"]["rollback_plan"]
            } else {
                &j["frozen_plan"]
            };
            ensure!(
                !plan.is_null(),
                "The retained rollback target plan is unavailable"
            );
            apply(home, transaction, plan, true).await?;
        }
        verify(home, transaction).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        restore_aborted_rollback(home, transaction, &error.to_string()).await?;
        anyhow::bail!(
            "Requested rollback failed; the pre-rollback bundle was verified and restored unchanged: {error}"
        );
    }
    restore(home, transaction, "rolled_back").await
}

/// Deliberate rollback is a new idle transaction, never a rewind of accepted
/// work or a retry of the already completed update.
pub async fn rollback(home: &Path, previous_transaction: &str) -> Result<Value> {
    standalone_worker(home)?;
    let previous = {
        let _owner = lock(home, "native-update.lock")?;
        let j = journal(home, previous_transaction)?;
        ensure!(
            j["phase"] == "restored" && j["direction"] == "applied",
            "Only the most recent completed applied bundle can be rolled back"
        );
        ensure!(
            !j["frozen_plan"].is_null(),
            "The original frozen plan is unavailable"
        );
        ensure!(
            installed(home)?["package"] == j["candidate"],
            "Installed bundle changed"
        );
        j
    };
    let transaction = uuid::Uuid::new_v4().to_string();
    let ready = prepare(
        home,
        &transaction,
        &json!({"id":transaction,"owner":"standalone","plan":{"operation":"rollback"},
            "previous_transaction":previous_transaction,"rollback_plan":previous["frozen_plan"]}),
    )
    .await?;
    if ready["ready"] != true {
        verify(home, &transaction).await?;
        restore(home, &transaction, "unchanged").await?;
        return Ok(ready);
    }
    continue_explicit_rollback(home, &transaction).await
}

pub async fn recover(home: &Path, transaction: &str, rollback: bool) -> Result<Value> {
    standalone_worker(home)?;
    let mut j = journal(home, transaction)?;
    if terminal(&j) {
        return Ok(json!({"restored":true,"transaction_id":transaction,
            "outcome":j["restore_outcome"],"rollback_aborted":j["rollback_aborted"] == true}));
    }
    if j["restore_started"] == true {
        ensure!(
            !rollback,
            "Restoration already began; rollback is no longer allowed"
        );
        let mut result = restore(
            home,
            transaction,
            j["restore_outcome"]
                .as_str()
                .context("Missing restoration outcome")?,
        )
        .await?;
        if j["rollback_aborted"] == true {
            result["rollback_aborted"] = json!(true);
            result["outcome"] = json!("unchanged");
        }
        return Ok(result);
    }
    if matches!(j["phase"].as_str(), Some("intent" | "preparing")) {
        let readiness = prepare(home, transaction, &j["context"]).await?;
        if readiness["ready"] != true {
            verify(home, transaction).await?;
            restore(home, transaction, "unchanged").await?;
            return Ok(readiness);
        }
        j = journal(home, transaction)?;
    }
    if j["context"]["owner"] == "standalone"
        && j["context"]["plan"]["operation"] == "rollback"
        && j["phase"] != "deferred"
    {
        return if rollback || j["rollback_aborted"] == true {
            restore_aborted_rollback(
                home,
                transaction,
                "Explicit rollback aborted during recovery",
            )
            .await
        } else {
            continue_explicit_rollback(home, transaction).await
        };
    }
    if rollback || j["phase"] == "switching" {
        apply(home, transaction, &j["frozen_plan"], true).await?;
        verify(home, transaction).await?;
        return restore(home, transaction, "rolled_back").await;
    }
    verify(home, transaction).await?;
    restore(
        home,
        transaction,
        j["direction"].as_str().context("Missing outcome")?,
    )
    .await
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/native-update/status", get(http_status))
        .route("/api/v1/native-update/stop", post(http_stop))
        .route("/api/v1/native-update/finish", post(http_finish))
}
async fn http_status(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
) -> crate::error::ApiResult<Json<Value>> {
    auth::check(&s, &headers, "admin", false)?;
    let runtime = read(&s.home.join("runtime.json"))?;
    let live_work =
        !s.running.lock().unwrap().is_empty() || !s.companion.controls.lock().unwrap().is_empty();
    let mut v = json!({"id":s.config.id,"version":env!("CARGO_PKG_VERSION"),"binary_sha256":digest(&std::env::current_exe()?)?,"pid":std::process::id(),"process_nonce":runtime["process_nonce"],"transaction_id":runtime["transaction_id"],"launch_nonce":runtime["launch_nonce"],"admission":update_admission::status_db(&s.db.lock().unwrap())?,"live_work":live_work});
    #[cfg(windows)]
    {
        v["process_birth"] = json!(windows_exit::birth(std::process::id())?);
    }
    #[cfg(not(windows))]
    {
        let _ = &mut v;
    }
    Ok(Json(v))
}
async fn http_stop(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> crate::error::ApiResult<Json<Value>> {
    auth::check(&s, &headers, "admin", true)?;
    let result = (|| -> Result<Value> {
        let transaction = body["transaction_id"]
            .as_str()
            .context("Missing transaction")?;
        let j = journal(&s.home, transaction)?;
        ensure!(!terminal(&j), "Update already restored");
        let runtime = read(&s.home.join("runtime.json"))?;
        ensure!(
            body["process_nonce"] == runtime["process_nonce"],
            "Process identity changed"
        );
        let a = update_admission::status_db(&s.db.lock().unwrap())?;
        ensure!(
            a["closed"] == true && a["transaction_id"] == transaction && a["ready"] == true,
            "Application work has not settled"
        );
        ensure!(
            s.running.lock().unwrap().is_empty() && s.companion.controls.lock().unwrap().is_empty(),
            "Application still owns active work"
        );
        s.native_shutdown.send_replace(true);
        Ok(json!({"accepted":true,"transaction_id":transaction}))
    })();
    Ok(Json(result?))
}
async fn http_finish(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> crate::error::ApiResult<Json<Value>> {
    auth::check(&s, &headers, "admin", true)?;
    let result = (|| -> Result<Value> {
        let transaction = body["transaction_id"]
            .as_str()
            .context("Missing transaction")?;
        let j = journal(&s.home, transaction)?;
        ensure!(
            j["restore_started"] == true
                && j["restore_boundary_verified"] == true
                && j["phase"] == "restoring"
                && body["launch_nonce"] == j["launch_nonce"]
                && digest(&std::env::current_exe()?)? == current_record(&j)["binary_sha256"],
            "Restore identity or verification changed"
        );
        update_admission::restore_db(&mut s.db.lock().unwrap(), transaction)?;
        Ok(json!({"restored":true,"transaction_id":transaction}))
    })();
    Ok(Json(result?))
}

#[cfg(windows)]
mod windows_exit {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, HANDLE, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };
    pub struct ProcessExit(HANDLE);
    unsafe impl Send for ProcessExit {}
    unsafe impl Sync for ProcessExit {}
    impl Drop for ProcessExit {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn created(h: HANDLE) -> Result<u64> {
        let mut c: FILETIME = unsafe { std::mem::zeroed() };
        let mut e = c;
        let mut k = c;
        let mut u = c;
        ensure!(
            unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } != 0,
            "Cannot inspect application birth identity"
        );
        Ok((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
    }
    pub fn birth(pid: u32) -> Result<u64> {
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        ensure!(!h.is_null(), "Cannot inspect owned application");
        let h = ProcessExit(h);
        created(h.0)
    }
    const RECEIPT: &str = "native-update-process-exit.json";

    fn validate_receipt(j: &Value, receipt: &Value) -> Result<()> {
        ensure!(
            receipt["schema"] == 1
                && receipt["installation_id"] == j["installation_id"]
                && receipt["transaction_id"] == j["transaction_id"]
                && (receipt["binary_sha256"] == j["previous"]["binary_sha256"]
                    || receipt["binary_sha256"] == j["candidate"]["binary_sha256"])
                && receipt["pid"]
                    .as_u64()
                    .is_some_and(|p| p > 0 && p <= u32::MAX as u64)
                && receipt["process_birth"].as_u64().is_some_and(|p| p > 0)
                && receipt["process_nonce"]
                    .as_str()
                    .is_some_and(|s| (16..=256).contains(&s.len()))
                && receipt["exit_confirmed"].is_boolean(),
            "Retained Windows exit identity differs; executable preserved"
        );
        Ok(())
    }
    fn receipt_from_status(j: &Value, status: &Value) -> Result<Value> {
        ensure!(
            status["id"] == j["installation_id"],
            "Captured process belongs to another installation"
        );
        let receipt = json!({"schema":1,"installation_id":j["installation_id"],
            "transaction_id":j["transaction_id"],"binary_sha256":status["binary_sha256"],
            "pid":status["pid"],"process_birth":status["process_birth"],
            "process_nonce":status["process_nonce"],"exit_confirmed":false});
        validate_receipt(j, &receipt)?;
        Ok(receipt)
    }
    pub fn confirm(home: &Path, j: &Value) -> Result<()> {
        let path = home.join(RECEIPT);
        let mut receipt = read(&path)?;
        validate_receipt(j, &receipt)?;
        receipt["exit_confirmed"] = json!(true);
        save(&path, &receipt)
    }
    pub async fn reconcile(home: &Path, j: &Value) -> Result<()> {
        let path = home.join(RECEIPT);
        let prior = if path.exists() {
            Some(read(&path)?)
        } else {
            None
        };
        let receipt = if let Some(receipt) = prior
            .as_ref()
            .filter(|r| r["transaction_id"] == j["transaction_id"])
        {
            receipt.clone()
        } else {
            ensure!(
                prior.as_ref().is_none_or(|r| r["exit_confirmed"] == true),
                "A previous Windows process exit remains unresolved; executable preserved"
            );
            if j["original_process"].is_null() {
                ensure!(
                    j["original_running"] != true,
                    "Original Windows exit identity is unavailable; executable preserved"
                );
                return Ok(());
            }
            // The service may have exited naturally after prepare authenticated
            // it but before a stop could persist its own receipt. Keep exactly
            // that proven generation; do not substitute the latest PID by name.
            let receipt = receipt_from_status(j, &j["original_process"])?;
            save(&path, &receipt)?;
            receipt
        };
        validate_receipt(j, &receipt)?;
        if receipt["exit_confirmed"] == true {
            return Ok(());
        }
        if let Some(process) = ProcessExit::resume(&receipt)? {
            process.wait().await?;
        }
        confirm(home, j)
    }
    /// Persist the authenticated process generation before any request which can
    /// release service.lock. The receipt is separate from the phase journal so a
    /// caller's later phase write cannot accidentally erase this exit evidence.
    pub async fn retain(home: &Path, j: &Value, status: &Value) -> Result<Option<ProcessExit>> {
        let path = home.join(RECEIPT);
        let receipt = receipt_from_status(j, status)?;
        if path.exists() {
            let prior = read(&path)?;
            if prior["transaction_id"] == j["transaction_id"] {
                validate_receipt(j, &prior)?;
                if prior["pid"] != receipt["pid"]
                    || prior["process_birth"] != receipt["process_birth"]
                {
                    // Do not overwrite an unresolved prior generation merely
                    // because a newer process already acquired the service lock.
                    reconcile(home, j).await?;
                } else {
                    ensure!(
                        prior["process_nonce"] == receipt["process_nonce"]
                            && prior["binary_sha256"] == receipt["binary_sha256"],
                        "Authenticated process identity changed"
                    );
                }
            } else {
                ensure!(
                    prior["exit_confirmed"] == true,
                    "A previous Windows process exit remains unresolved; executable preserved"
                );
            }
        }
        save(&path, &receipt)?;
        ProcessExit::resume(&receipt)
    }
    impl ProcessExit {
        fn resume(s: &Value) -> Result<Option<Self>> {
            let pid = u32::try_from(s["pid"].as_u64().context("Missing process identity")?)?;
            ensure!(pid > 0, "Invalid process identity");
            let expected = s["process_birth"]
                .as_u64()
                .context("Missing process birth identity")?;
            let h = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if h.is_null() {
                // Windows reports an absent PID as ERROR_INVALID_PARAMETER.
                // Access denied and all other failures are uncertainty, not exit.
                let code = unsafe { GetLastError() };
                ensure!(
                    code == ERROR_INVALID_PARAMETER,
                    "Cannot inspect retained application exit ({code}); executable preserved"
                );
                return Ok(None);
            }
            let h = Self(h);
            // A successfully inspected different creation time proves the
            // original generation is gone. Never wait for or affect the reused PID.
            if created(h.0)? != expected {
                return Ok(None);
            }
            Ok(Some(h))
        }
        pub async fn wait(&self) -> Result<()> {
            let end = std::time::Instant::now()
                + crate::native_update_clock::cap(Duration::from_secs(5))?;
            loop {
                crate::native_update_clock::check()?;
                match unsafe { WaitForSingleObject(self.0, 0) } {
                    WAIT_OBJECT_0 => return Ok(()),
                    WAIT_TIMEOUT => {}
                    _ => anyhow::bail!("Cannot confirm application exit"),
                };
                ensure!(
                    std::time::Instant::now() < end,
                    "Owned process has not exited; binary preserved"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use rusqlite::{Connection, params};
    use std::sync::mpsc;

    #[test]
    fn data_receipts_publish_while_open_readers_keep_their_snapshot() {
        use std::io::Read;
        let home = tempfile::tempdir().unwrap();
        for name in [JOURNAL, INSTALLED, "native-update-process-exit.json"] {
            let path = home.path().join(name);
            let previous = json!({"generation":1,"historical_outcome":"unknown"});
            let current = json!({"generation":2,"historical_outcome":"unknown"});
            save(&path, &previous).unwrap();
            let mut reader = File::open(&path).unwrap();
            save(&path, &current).unwrap();
            let mut retained = Vec::new();
            reader.read_to_end(&mut retained).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&retained).unwrap(),
                previous
            );
            assert_eq!(read(&path).unwrap(), current);
        }
    }

    #[cfg(windows)]
    #[test]
    fn binary_replacement_still_refuses_an_open_reader() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("candidate.exe");
        let target = home.path().join("installed.exe");
        config::private_write(&source, b"candidate executable bytes").unwrap();
        config::private_write(&target, b"previous executable bytes").unwrap();
        let reader = File::open(&target).unwrap();
        assert!(write_binary(&source, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"previous executable bytes");
        drop(reader);
        write_binary(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"candidate executable bytes");
    }

    fn initialized_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let config: Config = serde_json::from_value(json!({
            "id":uuid::Uuid::new_v4().to_string(),"label":"Startup fixture","owner":"owner",
            "bind":"127.0.0.1","port":0,"public_url":"https://example.test",
            "pam_service":"agent-conversations","tls_cert":home.path().join("cert.pem"),
            "tls_key":home.path().join("key.pem"),"import_source":"fixture","import_surface":"fixture",
            "agent":{"label":"Retained agent","command":std::env::current_exe().unwrap(),
                "args":[],"cwd":home.path(),"env":{},"mcp_servers":[],"timeout_seconds":30}
        })).unwrap();
        config.save(home.path()).unwrap();
        let state = AppState::open(home.path().to_owned()).unwrap();
        let room = state
            .create_room("Retained work", "owner", &["default".into()])
            .unwrap();
        {
            let db = state.db.lock().unwrap();
            db.execute(
                "UPDATE conversations SET status='working' WHERE id=?1",
                [room["id"].as_str().unwrap()],
            )
            .unwrap();
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at) VALUES('in-flight',?1,'assistant','Retain this unknown progress','streaming',1,1)", [room["id"].as_str().unwrap()]).unwrap();
            // AppState::open would replace this exact credential row as well as
            // interrupt the streaming message. A stale launch must reach neither.
            db.execute(
                "UPDATE tokens SET expires_at=17 WHERE id='local-control'",
                [],
            )
            .unwrap();
        }
        drop(state);
        home
    }

    fn protected(home: &Path) -> Vec<Vec<u8>> {
        [
            "state.sqlite3",
            "secrets/control-token",
            "secrets/context-token",
        ]
        .iter()
        .map(|p| fs::read(home.join(p)).unwrap())
        .collect()
    }

    #[test]
    fn revoked_launch_during_executable_hash_cannot_initialize_or_recover_state() {
        for change_nonce_only in [false, true] {
            let home = initialized_home();
            let tx = uuid::Uuid::new_v4().to_string();
            // The race is at the digest boundary, not in SHA-256 itself. Hash
            // small fixture bytes instead of the entire debug test executable.
            let executable = home.path().join("candidate-executable");
            fs::write(&executable, b"fixture executable").unwrap();
            let actual = digest(&executable).unwrap();
            let mut j = json!({"transaction_id":tx,"phase":"verifying","launch_nonce":"original",
                "selected":"previous","previous":{"binary_sha256":actual}});
            save(&home.path().join(JOURNAL), &j).unwrap();
            let before = protected(home.path());
            let (hash_entered, hashing) = mpsc::channel();
            let (resume, revoked) = mpsc::channel();
            let path = home.path().to_owned();
            let child_tx = tx.clone();
            let startup = std::thread::spawn(move || -> Result<()> {
                let _service = lock(&path, "service.lock")?;
                let _launch =
                    validate_start_with_digest(&path, Some((&child_tx, "original")), || {
                        hash_entered.send(())?;
                        revoked.recv_timeout(Duration::from_secs(5))?;
                        digest(&executable)
                    })?;
                AppState::open(path)?;
                Ok(())
            });
            hashing.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(lock(home.path(), "service.lock").is_err());
            if change_nonce_only {
                j["launch_nonce"] = json!("replacement");
                save(&home.path().join(JOURNAL), &j).unwrap();
            } else {
                j["launch_nonce"] = Value::Null;
                phase(home.path(), &mut j, "switching").unwrap();
            }
            resume.send(()).unwrap();
            let error = startup.join().unwrap().unwrap_err().to_string();
            assert!(error.contains("no longer authorized"), "{error}");
            assert_eq!(protected(home.path()), before);
            let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT status FROM messages WHERE id='in-flight'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "streaming"
            );
            assert_eq!(
                db.query_row(
                    "SELECT expires_at FROM tokens WHERE id=?1",
                    params!["local-control"],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                17
            );
            assert!(lock(home.path(), "service.lock").is_ok());
        }
    }

    #[test]
    fn normal_startup_guard_serializes_first_prepare_intent_until_initialization_boundary() {
        let home = initialized_home();
        let _service = lock(home.path(), "service.lock").unwrap();
        let launch = validate_start(home.path(), None).unwrap();
        let (contended, observed) = mpsc::channel();
        let path = home.path().to_owned();
        let writer = std::thread::spawn(move || {
            assert!(lock(&path, "native-launch.lock").is_err());
            contended.send(()).unwrap();
            save(&path.join(JOURNAL), &json!({"phase":"intent"}))
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!home.path().join(JOURNAL).exists());
        // This is the exact boundary at which server::serve_inner drops the
        // guard after AppState::open, before any listener or health operation.
        drop(launch);
        writer.join().unwrap().unwrap();
        assert_eq!(read(&home.path().join(JOURNAL)).unwrap()["phase"], "intent");
        assert!(validate_start(home.path(), None).is_err());
    }

    #[test]
    fn post_spawn_pid_publication_waits_past_two_seconds_for_initialization() {
        let home = initialized_home();
        let tx = uuid::Uuid::new_v4().to_string();
        let executable = home.path().join("candidate-executable");
        fs::write(&executable, b"fixture executable").unwrap();
        let actual = digest(&executable).unwrap();
        let mut j = json!({"transaction_id":tx,"phase":"verifying","launch_nonce":"original",
            "selected":"previous","previous":{"binary_sha256":actual},"child_pid":null});
        save(&home.path().join(JOURNAL), &j).unwrap();
        let before = protected(home.path());
        let before_journal = j.clone();
        let _service = lock(home.path(), "service.lock").unwrap();
        let launch = validate_start_with_digest(home.path(), Some((&tx, "original")), || {
            digest(&executable)
        })
        .unwrap();
        let (contended, observed) = mpsc::channel();
        let path = home.path().to_owned();
        j["child_pid"] = json!(123);
        let writer = std::thread::spawn(move || {
            let _launch = launch_guard_waiting(&path, || contended.send(()).unwrap())?;
            // The same guarded publication as save(); the callback makes the
            // first failed acquisition observable without a scheduling guess.
            amplifier_native_platform::private_write_data(
                &path.join(JOURNAL),
                &serde_json::to_vec_pretty(&j)?,
            )
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        // The wait deadline already exists and acquisition has actually failed.
        // A valid migration may exceed two seconds; no publication is allowed
        // while its final authorization/database boundary is still held.
        std::thread::sleep(Duration::from_millis(2200));
        assert_eq!(read(&home.path().join(JOURNAL)).unwrap(), before_journal);
        assert_eq!(protected(home.path()), before);
        drop(launch);
        writer.join().unwrap().unwrap();
        let receipt = read(&home.path().join(JOURNAL)).unwrap();
        assert_eq!(receipt["child_pid"], 123);
        assert_eq!(receipt["launch_nonce"], "original");
        assert_eq!(receipt["phase"], "verifying");
        assert_eq!(protected(home.path()), before);
    }
}

#[cfg(test)]
mod lock_tests {
    use super::lock;

    #[test]
    fn installation_lock_rejects_a_live_owner() {
        let home = tempfile::tempdir().unwrap();
        let owner = lock(home.path(), "native-update.lock").unwrap();
        assert!(lock(home.path(), "native-update.lock").is_err());
        drop(owner);
        assert!(lock(home.path(), "native-update.lock").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn installation_lock_releases_while_an_inherited_description_remains_open() {
        let home = tempfile::tempdir().unwrap();
        let owner = lock(home.path(), "native-update.lock").unwrap();
        // try_clone duplicates the same open file description, deterministically
        // reproducing the descriptor inherited by a child before its exec.
        let inherited = owner.file.try_clone().unwrap();
        assert!(lock(home.path(), "native-update.lock").is_err());
        drop(owner);
        let next_owner = lock(home.path(), "native-update.lock").unwrap();
        assert!(lock(home.path(), "native-update.lock").is_err());
        // Closing an older duplicate must not release the new owner's lock.
        drop(inherited);
        assert!(lock(home.path(), "native-update.lock").is_err());
        drop(next_owner);
        assert!(lock(home.path(), "native-update.lock").is_ok());
    }
}
