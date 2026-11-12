//! Fixed installation-owned adapters for existing command-provider protocol 2 and
//! lifecycle participant protocol 1. This module performs no OS service operation
//! and never resolves a release or command supplied by chat, the browser or ACP.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Holds the fixed participant CLI deadline through result serialization/output.
/// Provider/enrollment commands retain their existing independent caller bounds.
pub struct InvocationClock(Option<crate::native_update_clock::Invocation>);
impl InvocationClock {
    pub fn complete(self) -> Result<()> {
        match self.0 {
            Some(clock) => clock.complete(),
            None => Ok(()),
        }
    }
}
pub fn invocation_clock(mode: &str, action: &str) -> Result<InvocationClock> {
    Ok(InvocationClock(if mode == "participant" {
        Some(crate::native_update_clock::install(action)?)
    } else {
        None
    }))
}

const CONFIG_LIMIT: u64 = 64 * 1024;
const ENVELOPE_LIMIT: u64 = 1024 * 1024;
const OWNER_FILE: &str = "native-update-owner.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnerConfig {
    schema: u32,
    installation_id: String,
    home: PathBuf,
    component_id: String,
    participant_id: String,
    components: Vec<String>,
    candidate: PathBuf,
    helper_package: PathBuf,
    helper_manifest_sha256: String,
    helper_executable: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct OwnerMarker {
    schema: u32,
    owner: String,
    installation_id: String,
    config: PathBuf,
    config_sha256: String,
    component_id: String,
    participant_id: String,
    helper_package: PathBuf,
    helper_manifest_sha256: String,
    helper_executable: PathBuf,
}
struct BoundOwner {
    config: OwnerConfig,
    marker: OwnerMarker,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEnvelope {
    id: String,
    component: String,
    plan: FrozenPlan,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenPlan {
    schema: u32,
    available: bool,
    version: String,
    installation_id: String,
    component_id: String,
    owner_config_sha256: String,
    helper_manifest_sha256: String,
    bundle_plan: Value,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParticipantContext {
    id: String,
    participant: String,
    plan: ParticipantPlan,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParticipantPlan {
    schema: u32,
    transaction_id: String,
    operation: String,
    components: Vec<String>,
    restart_scope: RestartScope,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestartScope {
    services: Vec<String>,
    instance: bool,
    slack_homes: Vec<String>,
}
#[derive(Default)]
struct Environment {
    plan: Option<PathBuf>,
    transaction: Option<String>,
    outcome: Option<String>,
}
impl Environment {
    fn current() -> Result<Self> {
        let text = |name: &str| -> Result<Option<String>> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(_) => anyhow::bail!("Invalid update environment encoding"),
            }
        };
        Ok(Self {
            plan: text("CONSTELLATION_UPDATE_PLAN")?.map(PathBuf::from),
            transaction: text("CONSTELLATION_UPDATE_TRANSACTION")?,
            outcome: text("CONSTELLATION_UPDATE_OUTCOME")?,
        })
    }
    fn transaction(&self) -> Result<&str> {
        let value = self
            .transaction
            .as_deref()
            .context("Missing update transaction")?;
        exact_uuid(value)?;
        Ok(value)
    }
    fn plan(&self) -> Result<Value> {
        let path = self
            .plan
            .as_deref()
            .context("Missing frozen update context")?;
        absolute(path)?;
        Ok(serde_json::from_slice(&read_regular(
            path,
            ENVELOPE_LIMIT,
            false,
        )?)?)
    }
    fn outcome(&self) -> Result<&str> {
        let outcome = self.outcome.as_deref().context("Missing update outcome")?;
        ensure!(
            matches!(outcome, "unchanged" | "applied" | "rolled_back"),
            "Invalid update outcome"
        );
        Ok(outcome)
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn exact_uuid(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "Use canonical update identifiers"
    );
    Ok(())
}
fn valid_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn absolute(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute() && !path.as_os_str().is_empty(),
        "Use absolute installation-owned paths"
    );
    ensure!(
        !path.to_string_lossy().chars().any(char::is_control),
        "Invalid installation path"
    );
    Ok(())
}
fn ordinary(metadata: &fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}
fn read_regular(path: &Path, limit: u64, private: bool) -> Result<Vec<u8>> {
    ensure!(
        ordinary(&fs::symlink_metadata(path)?),
        "Update records must be ordinary files"
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        ordinary(&metadata) && metadata.len() <= limit,
        "Invalid update record"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "Update records belong to another OS account"
        );
        if private {
            ensure!(
                metadata.mode() & 0o077 == 0,
                "Protect the owner update configuration"
            );
        }
    }
    #[cfg(windows)]
    let _ = private; // The enrollment marker uses the shared owner/SYSTEM protected writer.
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= limit,
        "Update record exceeds its size limit"
    );
    Ok(data)
}
fn validate_config(config: &OwnerConfig) -> Result<()> {
    ensure!(config.schema == 1, "Unsupported owner configuration");
    exact_uuid(&config.installation_id)?;
    ensure!(
        valid_id(&config.component_id) && valid_id(&config.participant_id),
        "Invalid enrolled component or participant ID"
    );
    let unique: BTreeSet<_> = config.components.iter().collect();
    ensure!(
        !config.components.is_empty()
            && config.components.len() <= 64
            && unique.len() == config.components.len()
            && config.components.iter().all(|s| valid_id(s))
            && config.components.contains(&config.component_id),
        "Invalid enrolled component intersection"
    );
    for path in [
        &config.home,
        &config.candidate,
        &config.helper_package,
        &config.helper_executable,
    ] {
        absolute(path)?;
    }
    ensure!(
        valid_digest(&config.helper_manifest_sha256),
        "Invalid immutable helper manifest digest"
    );
    Ok(())
}
fn load_owner(path: &Path) -> Result<BoundOwner> {
    absolute(path)?;
    let bytes = read_regular(path, CONFIG_LIMIT, true)?;
    let mut config: OwnerConfig = serde_json::from_slice(&bytes)?;
    validate_config(&config)?;
    config.home = dunce::canonicalize(&config.home)?;
    config.helper_package = dunce::canonicalize(&config.helper_package)?;
    config.helper_executable = dunce::canonicalize(&config.helper_executable)?;
    let stable = amplifier_native_platform::installed_binary(
        &crate::native_bundle::APPLICATION,
        &config.home,
    );
    ensure!(
        config.helper_executable != dunce::canonicalize(stable)?,
        "The owner helper cannot be the stable executable it replaces"
    );
    ensure!(
        crate::config::Config::load(&config.home)?.id == config.installation_id,
        "Installation identity changed"
    );
    let helper = crate::native_bundle::verify(&config.helper_package)?;
    ensure!(
        helper["manifest_sha256"] == config.helper_manifest_sha256,
        "Owner helper package changed"
    );
    let executable = helper["entrypoints"]["service"]
        .as_str()
        .context("Owner helper entrypoint is absent")?;
    ensure!(
        dunce::canonicalize(executable)? == config.helper_executable,
        "Owner helper is not the immutable bundle entrypoint"
    );
    let marker = OwnerMarker {
        schema: 1,
        owner: "enrolled".into(),
        installation_id: config.installation_id.clone(),
        config: dunce::canonicalize(path)?,
        config_sha256: digest(&bytes),
        component_id: config.component_id.clone(),
        participant_id: config.participant_id.clone(),
        helper_package: config.helper_package.clone(),
        helper_manifest_sha256: config.helper_manifest_sha256.clone(),
        helper_executable: config.helper_executable.clone(),
    };
    Ok(BoundOwner { config, marker })
}
fn check_owner(owner: &BoundOwner) -> Result<()> {
    let bytes = read_regular(&owner.config.home.join(OWNER_FILE), CONFIG_LIMIT, true)?;
    let marker: OwnerMarker = serde_json::from_slice(&bytes)?;
    ensure!(
        marker == owner.marker,
        "This home belongs to another update owner or configuration"
    );
    Ok(())
}
fn registration_value(owner: &BoundOwner) -> Value {
    let command = |mode: &str, action: &str| {
        json!([
            owner.config.helper_executable,
            "native-update-protocol",
            "--config",
            owner.marker.config,
            "--mode",
            mode,
            "--action",
            action
        ])
    };
    let commands = |mode: &str, actions: &[&str]| -> Value {
        Value::Object(
            actions
                .iter()
                .map(|a| (a.to_string(), command(mode, a)))
                .collect(),
        )
    };
    json!({"schema":1,"components":[{"id":owner.config.component_id,"provider":"command","protocol":2,
        "commands":commands("provider", &["status","plan","apply","verify","rollback"]),
        "restart_scope":{"services":[],"instance":false,"slack_homes":[]}}],
        "participants":[{"id":owner.config.participant_id,"protocol":1,"components":owner.config.components,
        "commands":commands("participant", &["prepare","verify","restore"])}]})
}
/// Export the fixed registration delta only. The caller merges by enrolled ID;
/// this does not submit it, create a scheduler or change any update policy.
pub fn registration(config_path: &Path) -> Result<Value> {
    let owner = load_owner(config_path)?;
    check_owner(&owner)?;
    Ok(registration_value(&owner))
}
/// Explicit local enrollment claims this one home for this exact external owner.
/// It neither registers with an Instance nor activates scheduled installation.
pub fn claim_owner(config_path: &Path) -> Result<Value> {
    let owner = load_owner(config_path)?;
    let _owner = crate::native_update::lock(&owner.config.home, "native-update.lock")?;
    let marker_path = owner.config.home.join(OWNER_FILE);
    if marker_path.exists() {
        // An identical repeated enrollment has no side effect, including during
        // a transaction. Changed ownership still refuses rather than rebinds.
        check_owner(&owner)?;
        return Ok(registration_value(&owner));
    }
    let status = crate::native_update::status(&owner.config.home)?;
    ensure!(
        status["id"] == owner.config.installation_id,
        "Installation identity changed"
    );
    ensure!(
        status["transaction_id"].is_null() || status["phase"] == "restored",
        "Finish the current update before changing ownership"
    );
    amplifier_native_platform::private_create(
        &marker_path,
        &serde_json::to_vec_pretty(&owner.marker)?,
    )?;
    Ok(registration_value(&owner))
}
fn freeze(owner: &BoundOwner, plan: Value) -> Result<Value> {
    let available = plan["available"]
        .as_bool()
        .context("Bundle plan has no availability")?;
    let version = plan["version"]
        .as_str()
        .context("Bundle plan has no version")?
        .to_owned();
    ensure!(
        !version.is_empty() && version.len() <= 128,
        "Invalid bundle version"
    );
    Ok(serde_json::to_value(FrozenPlan {
        schema: 1,
        available,
        version,
        installation_id: owner.config.installation_id.clone(),
        component_id: owner.config.component_id.clone(),
        owner_config_sha256: owner.marker.config_sha256.clone(),
        helper_manifest_sha256: owner.config.helper_manifest_sha256.clone(),
        bundle_plan: plan,
    })?)
}
fn provider_envelope(owner: &BoundOwner, value: Value) -> Result<ProviderEnvelope> {
    let envelope: ProviderEnvelope = serde_json::from_value(value)?;
    exact_uuid(&envelope.id)?;
    let plan = &envelope.plan;
    ensure!(
        envelope.component == owner.config.component_id
            && plan.schema == 1
            && plan.installation_id == owner.config.installation_id
            && plan.component_id == owner.config.component_id
            && plan.owner_config_sha256 == owner.marker.config_sha256
            && plan.helper_manifest_sha256 == owner.config.helper_manifest_sha256,
        "Frozen plan owner or component changed"
    );
    ensure!(
        plan.bundle_plan["available"] == plan.available
            && plan.bundle_plan["version"] == plan.version,
        "Frozen plan metadata changed"
    );
    Ok(envelope)
}
fn participant_context(
    owner: &BoundOwner,
    value: &Value,
    transaction: &str,
) -> Result<(ParticipantContext, bool)> {
    let context: ParticipantContext = serde_json::from_value(value.clone())?;
    exact_uuid(&context.id)?;
    exact_uuid(&context.plan.transaction_id)?;
    ensure!(
        context.participant == owner.config.participant_id
            && context.plan.schema == 1
            && context.plan.transaction_id == transaction
            && matches!(context.plan.operation.as_str(), "apply" | "rollback"),
        "Participant context identity changed"
    );
    ensure!(
        context.plan.components.len() <= 64 && context.plan.components.iter().all(|s| valid_id(s)),
        "Invalid changed components"
    );
    let unique: BTreeSet<_> = context.plan.components.iter().collect();
    ensure!(
        unique.len() == context.plan.components.len(),
        "Duplicate changed component"
    );
    // Shared host changes legitimately carry a wider generic restart scope.
    // This participant does not execute those service or Slack paths.
    ensure!(
        context.plan.restart_scope.services.len() <= 64
            && context.plan.restart_scope.slack_homes.len() <= 64,
        "Invalid shared restart scope"
    );
    let _ = context.plan.restart_scope.instance;
    let selected = context
        .plan
        .components
        .iter()
        .any(|c| owner.config.components.contains(c));
    Ok((context, selected))
}
fn current_transaction(
    owner: &BoundOwner,
    status: &Value,
    transaction: &str,
    context_id: &str,
) -> Result<()> {
    ensure!(
        status["id"] == owner.config.installation_id
            && status["transaction_id"] == transaction
            && status["context_id"] == context_id,
        "The prepared installation transaction differs from this command"
    );
    Ok(())
}
fn matching_context(
    owner: &BoundOwner,
    status: &Value,
    transaction: &str,
    context: &ParticipantContext,
    value: &Value,
) -> Result<()> {
    current_transaction(owner, status, transaction, &context.id)?;
    ensure!(
        status["context_hash"] == digest(&serde_json::to_vec(value)?),
        "Prepared participant context changed"
    );
    Ok(())
}
/// Called only by the fixed protected local command, never an HTTP/ACP command.
pub async fn run(config_path: &Path, mode: &str, action: &str) -> Result<Value> {
    if mode == "enrollment" {
        return match action {
            "enroll" => claim_owner(config_path),
            "registration" => registration(config_path),
            _ => anyhow::bail!("Unsupported enrollment action"),
        };
    }
    let owner = load_owner(config_path)?;
    check_owner(&owner)?;
    let environment = Environment::current()?;
    match (mode, action) {
        ("provider", "status") => crate::native_update::status(&owner.config.home),
        ("provider", "plan") => freeze(
            &owner,
            crate::native_update::plan(&owner.config.home, &owner.config.candidate)?,
        ),
        ("provider", "apply" | "rollback" | "verify") => {
            let transaction = environment.transaction()?;
            let envelope = provider_envelope(&owner, environment.plan()?)?;
            let status = crate::native_update::status(&owner.config.home)?;
            current_transaction(&owner, &status, transaction, &envelope.id)?;
            if action == "verify" {
                crate::native_update::verify(&owner.config.home, transaction).await
            } else {
                crate::native_update::apply(
                    &owner.config.home,
                    transaction,
                    &envelope.plan.bundle_plan,
                    action == "rollback",
                )
                .await
            }
        }
        ("participant", "prepare" | "verify" | "restore") => {
            let transaction = environment.transaction()?;
            let value = environment.plan()?;
            let (context, selected) = participant_context(&owner, &value, transaction)?;
            let outcome = environment.outcome()?;
            if !selected {
                let key = match action {
                    "prepare" => "ready",
                    "verify" => "verified",
                    _ => "restored",
                };
                return Ok(json!({key:true,"ignored":true}));
            }
            if action == "prepare" {
                ensure!(
                    outcome == "unchanged",
                    "Preparation cannot claim an update outcome"
                );
                crate::native_update::prepare(&owner.config.home, transaction, &value).await
            } else {
                let status = crate::native_update::status(&owner.config.home)?;
                matching_context(&owner, &status, transaction, &context, &value)?;
                if action == "verify" {
                    crate::native_update::verify(&owner.config.home, transaction).await
                } else {
                    crate::native_update::restore(&owner.config.home, transaction, outcome).await
                }
            }
        }
        _ => anyhow::bail!("Unsupported fixed update command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> BoundOwner {
        let home = std::env::temp_dir().join("Companion protocol café");
        let config = OwnerConfig {
            schema: 1,
            installation_id: "c0ed61c0-741d-4f55-9912-f6f3eb27ca26".into(),
            home: home.clone(),
            component_id: "companion".into(),
            participant_id: "companion".into(),
            components: vec!["companion".into(), "instance".into()],
            candidate: home.join("candidate"),
            helper_package: home.join("helper"),
            helper_manifest_sha256: "a".repeat(64),
            helper_executable: home.join("helper/agent-conversations"),
        };
        let marker = OwnerMarker {
            schema: 1,
            owner: "enrolled".into(),
            installation_id: config.installation_id.clone(),
            config: home.join("owner/config.json"),
            config_sha256: "b".repeat(64),
            component_id: config.component_id.clone(),
            participant_id: config.participant_id.clone(),
            helper_package: config.helper_package.clone(),
            helper_manifest_sha256: config.helper_manifest_sha256.clone(),
            helper_executable: config.helper_executable.clone(),
        };
        BoundOwner { config, marker }
    }
    fn context() -> Value {
        json!({"id":"849c23dd-b27b-4709-a8e7-18728dcc1db6","participant":"companion","plan":{"schema":1,"transaction_id":"fcad255c-090a-4cd5-9aaf-67af67663ef7","operation":"apply","components":["instance","unrecognized-future-component"],"restart_scope":{"services":["known-service"],"instance":true,"slack_homes":[]}}})
    }
    const TX: &str = "fcad255c-090a-4cd5-9aaf-67af67663ef7";
    fn envelope(owner: &BoundOwner) -> Value {
        json!({"id":"849c23dd-b27b-4709-a8e7-18728dcc1db6","component":"companion","plan":freeze(owner,json!({"available":true,"version":"0.12.9","candidate":{"manifest":"frozen"}})).unwrap()})
    }
    #[test]
    fn fixed_registration_has_owner_only_commands_and_empty_restart_scope() {
        let owner = owner();
        validate_config(&owner.config).unwrap();
        let value = registration_value(&owner);
        assert_eq!(value["components"][0]["protocol"], 2);
        assert_eq!(value["participants"][0]["protocol"], 1);
        assert_eq!(
            value["components"][0]["restart_scope"],
            json!({"services":[],"instance":false,"slack_homes":[]})
        );
        let command = &value["components"][0]["commands"]["apply"];
        assert_eq!(
            command,
            &json!([
                owner.config.helper_executable,
                "native-update-protocol",
                "--config",
                owner.marker.config,
                "--mode",
                "provider",
                "--action",
                "apply"
            ])
        );
        assert!(value.get("schedule").is_none());
        assert!(value.get("policy").is_none());
    }
    #[test]
    fn owner_configuration_requires_exact_ids_and_absolute_paths() {
        let mut c = owner().config;
        for bad in ["", "Upper", "a/b", "../thing", "a_b", "café"] {
            c.component_id = bad.into();
            assert!(validate_config(&c).is_err());
        }
        let mut c = owner().config;
        c.candidate = PathBuf::from("relative");
        assert!(validate_config(&c).is_err());
        let mut c = owner().config;
        c.components.push("instance".into());
        assert!(validate_config(&c).is_err());
    }
    #[test]
    fn frozen_provider_plan_preserves_candidate_and_rejects_different_owner() {
        let owner = owner();
        let value = envelope(&owner);
        let parsed = provider_envelope(&owner, value.clone()).unwrap();
        assert_eq!(parsed.plan.bundle_plan["candidate"]["manifest"], "frozen");
        for pointer in [
            "/component",
            "/plan/component_id",
            "/plan/owner_config_sha256",
            "/plan/helper_manifest_sha256",
            "/plan/installation_id",
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("changed");
            assert!(provider_envelope(&owner, bad).is_err());
        }
        let mut bad = value;
        bad["plan"]["bundle_plan"]["version"] = json!("other");
        assert!(provider_envelope(&owner, bad).is_err());
    }
    #[test]
    fn shared_component_selection_ignores_unknown_names_without_granting_operations() {
        let owner = owner();
        let mut value = context();
        assert!(participant_context(&owner, &value, TX).unwrap().1);
        value["plan"]["components"] = json!(["unrecognized-future-component"]);
        assert!(!participant_context(&owner, &value, TX).unwrap().1);
        value["plan"]["components"] = json!(["instance", "instance"]);
        assert!(participant_context(&owner, &value, TX).is_err());
    }
    #[test]
    fn participant_rejects_wrong_transaction_identity_and_malformed_wire() {
        let owner = owner();
        let value = context();
        for pointer in [
            "/id",
            "/participant",
            "/plan/transaction_id",
            "/plan/operation",
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = json!("other");
            assert!(participant_context(&owner, &bad, TX).is_err());
        }
        let mut bad = value;
        bad["plan"]["restart_scope"]["instance"] = json!("true");
        assert!(participant_context(&owner, &bad, TX).is_err());
    }
    #[test]
    fn verify_restore_require_original_prepared_context_and_shared_plan_identity() {
        let owner = owner();
        let value = context();
        let (context, _) = participant_context(&owner, &value, TX).unwrap();
        let status = json!({"id":owner.config.installation_id,"transaction_id":TX,"context_id":context.id,"context_hash":digest(&serde_json::to_vec(&value).unwrap())});
        matching_context(&owner, &status, TX, &context, &value).unwrap();
        let mut changed = value;
        changed["plan"]["components"] = json!(["instance"]);
        assert!(matching_context(&owner, &status, TX, &context, &changed).is_err());
        assert!(current_transaction(&owner, &status, TX, "different-plan").is_err());
    }
    #[test]
    fn malformed_or_missing_environment_does_not_create_an_operation() {
        let env = Environment::default();
        assert!(env.transaction().is_err());
        assert!(env.plan().is_err());
        assert!(env.outcome().is_err());
        let env = Environment {
            transaction: Some(TX.to_uppercase()),
            outcome: Some("success".into()),
            ..Environment::default()
        };
        assert!(env.transaction().is_err());
        assert!(env.outcome().is_err());
        let env = Environment {
            transaction: Some(TX.into()),
            outcome: Some("rolled_back".into()),
            ..Environment::default()
        };
        assert_eq!(env.transaction().unwrap(), TX);
        assert_eq!(env.outcome().unwrap(), "rolled_back");
    }
    #[test]
    fn protected_records_refuse_symlinks_and_size_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.json");
        fs::write(&path, b"{} ").unwrap();
        assert!(read_regular(&path, 2, false).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_regular(&link, 64, false).is_err());
        }
    }
}
