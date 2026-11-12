//! Filesystem/SQLite integration for native bundle selection. The fixture uses
//! small synthetic native-format headers for service/Node; they are NOT runnable.
//! HTTP health, authenticated stop/start and actual verification belong to the
//! separate CLI end-to-end suite. One test explicitly simulates a prior verified
//! receipt to exercise a NEW rollback transaction after completed installation.
use agent_conversations::{
    config::{self, Agent, Config},
    native_bundle, native_update, native_update_protocol,
    state::AppState,
    update_admission,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

fn digest(path: &Path) -> String {
    let mut input = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    format!("{:x}", hash.finalize())
}
fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write_json(path: &Path, value: &Value) {
    config::private_write(path, &serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn native_header() -> Vec<u8> {
    // Same bounded architecture/header fixture as native_bundle's own tests.
    // It exercises integrity/selection, never executable startup qualification.
    let mut bytes = vec![0u8; 128];
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "linux") {
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[16] = 2;
        bytes[18..20].copy_from_slice(&(if arm { 183u16 } else { 62u16 }).to_le_bytes());
        bytes[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56] = 1;
        bytes[64] = 1;
        bytes[68] = 5;
        bytes[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&128u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&128u64.to_le_bytes());
    } else if cfg!(target_os = "macos") {
        bytes[..4].copy_from_slice(b"\xcf\xfa\xed\xfe");
        bytes[4..8]
            .copy_from_slice(&(if arm { 0x0100000cu32 } else { 0x01000007u32 }).to_le_bytes());
        bytes[12] = 2;
    } else if cfg!(windows) {
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 64;
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&(if arm { 0xaa64u16 } else { 0x8664u16 }).to_le_bytes());
        bytes[86] = 2;
        bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
    }
    bytes
}
fn package(root: &Path, version: &str) {
    let service = if cfg!(windows) {
        "agent-conversations.exe"
    } else {
        "agent-conversations"
    };
    let node = if cfg!(windows) {
        "runtime/node.exe"
    } else {
        "runtime/node"
    };
    let paths = [service, node, "bridge/live.mjs", "bridge/companion-mcp.mjs"];
    let mut files = Vec::new();
    for relative in paths {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let executable = relative == service || relative == node;
        if executable {
            let mut bytes = native_header();
            bytes.extend_from_slice(format!("\nfixture-{version}-{relative}\n").as_bytes());
            fs::write(&path, bytes).unwrap();
        } else {
            fs::write(&path, format!("// fixture {version} {relative}\n")).unwrap();
        }
        files.push(json!({
            "path":relative,"sha256":digest(&path),
            "bytes":fs::metadata(&path).unwrap().len(),"executable":executable
        }));
    }
    write_json(
        &root.join("manifest.json"),
        &json!({"schema_version":1,"component":"conversations","version":version,
            "source_revision":"a".repeat(40),"target":native_bundle::target().unwrap(),
            "entrypoints":{"service":service,"node":node,"bridge":paths[2],"mcp":paths[3]},
            "files":files}),
    );
}

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    original: Value,
    protected: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("person's Companion — home");
        fs::create_dir(&home).unwrap();
        Config {
            id: uuid::Uuid::new_v4().to_string(),
            label: "Retained installation".into(),
            owner: "owner".into(),
            allowed_users: vec!["participant".into()],
            embed_origins: vec![],
            bind: "127.0.0.1".into(),
            port: 0,
            public_url: "https://example.test".into(),
            pam_service: "agent-conversations".into(),
            tls_cert: home.join("cert.pem"),
            tls_key: home.join("key.pem"),
            tls_ca: None,
            import_source: "fixture".into(),
            import_surface: "fixture".into(),
            agent: Agent {
                label: "Existing independently selected agent".into(),
                command: std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                args: vec![],
                cwd: home.to_string_lossy().into_owned(),
                session_cwd: None,
                env: BTreeMap::new(),
                mcp_servers: vec![],
                timeout_seconds: 30,
                client: Default::default(),
            },
        }
        .save(&home)
        .unwrap();
        let state = AppState::open(home.clone()).unwrap();
        let room = state
            .create_room("Saved conversation", "owner", &["default".into()])
            .unwrap();
        {
            let db = state.db.lock().unwrap();
            db.execute("UPDATE conversations SET session_id='original-session',endpoint='retained-endpoint' WHERE id=?1", [room["id"].as_str().unwrap()]).unwrap();
            db.execute("UPDATE room_agents SET session_id='original-session',endpoint='retained-endpoint',last_seq=17 WHERE conversation_id=?1", [room["id"].as_str().unwrap()]).unwrap();
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,targets) VALUES('retained-message',?1,'user','Earlier accepted work','complete',1,1,'owner','[\"default\"]')", [room["id"].as_str().unwrap()]).unwrap();
        }
        drop(state);
        config::private_write(
            &home.join("historical-unknown.json"),
            b"{\"submission\":null,\"receipt\":\"unknown\"}",
        )
        .unwrap();
        let source = root.path().join("acquired-a");
        package(&source, "fixture-a");
        let source_receipt = native_bundle::verify(&source).unwrap();
        write_json(
            &home.join("companion.json"),
            &json!({
                "enabled":false,"node":source_receipt["entrypoints"]["node"],
                "bridge":source_receipt["entrypoints"]["bridge"],
                "profiles":{"default":{"voice":"marin"}},
                "retained_custom":{"camera":false,"future_field":[1,"unchanged"]}
            }),
        );
        let original = native_update::bootstrap(&home, &source).unwrap();
        // Installed immutable bytes are sufficient for every later operation.
        fs::remove_dir_all(source).unwrap();
        let protected = protected_state(&home);
        Self {
            _root: root,
            home,
            original,
            protected,
        }
    }
    fn baseline_package(&self) -> PathBuf {
        PathBuf::from(
            self.original["package"]["bundle"]["bundle"]
                .as_str()
                .unwrap(),
        )
    }
    fn plan(&self) -> Value {
        let source = self._root.path().join("acquired-b");
        package(&source, "fixture-b");
        let result = native_update::plan(&self.home, &source).unwrap();
        fs::remove_dir_all(source).unwrap();
        result
    }
    fn retained(&self) {
        assert_eq!(protected_state(&self.home), self.protected);
        assert!(
            !self.home.join("runtime.json").exists(),
            "No service was launched"
        );
    }
    fn selected(&self, record: &Value) {
        let installed = read_json(&self.home.join("native-installation.json"));
        assert_eq!(installed["package"], *record);
        let bundle = &record["bundle"];
        assert_eq!(
            native_bundle::verify(Path::new(bundle["bundle"].as_str().unwrap())).unwrap(),
            *bundle
        );
        let binary =
            amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, &self.home);
        assert_eq!(digest(&binary), record["binary_sha256"].as_str().unwrap());
        let companion = read_json(&self.home.join("companion.json"));
        assert_eq!(companion["node"], bundle["entrypoints"]["node"]);
        assert_eq!(companion["bridge"], bundle["entrypoints"]["bridge"]);
        assert_eq!(companion["profiles"], json!({"default":{"voice":"marin"}}));
        assert_eq!(
            companion["retained_custom"],
            json!({"camera":false,"future_field":[1,"unchanged"]})
        );
    }
}

fn protected_state(home: &Path) -> Value {
    let db = Connection::open(home.join("state.sqlite3")).unwrap();
    let rows = |sql: &str| -> Vec<String> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    };
    json!({
        "config":digest(&home.join("config.json")),
        "token":digest(&home.join("secrets/control-token")),
        "unknown":digest(&home.join("historical-unknown.json")),
        "conversations":rows("SELECT json_array(id,title,status,session_id,endpoint,owner) FROM conversations ORDER BY id"),
        "bindings":rows("SELECT json_array(conversation_id,agent_id,session_id,endpoint,last_seq) FROM room_agents ORDER BY conversation_id,agent_id"),
        "messages":rows("SELECT json_array(id,conversation_id,role,text,status,created_at,updated_at,actor,targets) FROM messages ORDER BY seq"),
        "tokens":rows("SELECT json_array(hash,id,scopes,expires_at) FROM tokens ORDER BY id")
    })
}
fn context(transaction: &str, operation: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"participant":"companion",
        "plan":{"schema":1,"transaction_id":transaction,"operation":operation,
            "components":["companion"],"restart_scope":{"services":[],"instance":false,"slack_homes":[]}}})
}
async fn prepare(f: &Fixture, operation: &str) -> String {
    let tx = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        native_update::prepare(&f.home, &tx, &context(&tx, operation))
            .await
            .unwrap()["ready"],
        true
    );
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    assert_eq!(
        native_update::status(&f.home).unwrap()["original_running"],
        false
    );
    tx
}

#[cfg(unix)]
#[test]
fn bootstrap_finishes_execute_permission_after_interrupted_binary_publication() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let installed =
        amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, &f.home);
    let binary = fs::read(&installed).unwrap();
    let companion = fs::read(f.home.join("companion.json")).unwrap();
    // Reproduce the durable state between private_write's atomic publication
    // (mode 0600) and write_binary's chmod, before the baseline receipt exists.
    fs::remove_file(f.home.join("native-installation.json")).unwrap();
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        native_update::bootstrap(&f.home, &f.baseline_package()).unwrap(),
        f.original
    );
    assert_eq!(fs::read(&installed).unwrap(), binary);
    assert_eq!(
        fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read(f.home.join("companion.json")).unwrap(), companion);
    f.selected(&f.original["package"]);
    f.retained();
    assert!(!f.home.join("native-update-owner.json").exists());
    assert!(!f.home.join("native-update.json").exists());
}

#[cfg(unix)]
#[test]
fn bootstrap_does_not_make_changed_existing_bytes_executable() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let installed =
        amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, &f.home);
    let companion = fs::read(f.home.join("companion.json")).unwrap();
    fs::remove_file(f.home.join("native-installation.json")).unwrap();
    fs::write(&installed, b"different existing application").unwrap();
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(native_update::bootstrap(&f.home, &f.baseline_package()).is_err());
    assert_eq!(
        fs::read(&installed).unwrap(),
        b"different existing application"
    );
    assert_eq!(
        fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!f.home.join("native-installation.json").exists());
    assert_eq!(fs::read(f.home.join("companion.json")).unwrap(), companion);
    f.retained();
}

#[test]
fn bootstrap_is_idempotent_after_complete_and_lost_installation_receipt() {
    let f = Fixture::new();
    let companion = fs::read(f.home.join("companion.json")).unwrap();
    assert_eq!(
        native_update::bootstrap(&f.home, &f.baseline_package()).unwrap(),
        f.original
    );
    // Simulate a crash after the executable/bindings were written but before the
    // receipt became durable. Already-owned selected paths must be accepted.
    fs::remove_file(f.home.join("native-installation.json")).unwrap();
    assert_eq!(
        native_update::bootstrap(&f.home, &f.baseline_package()).unwrap(),
        f.original
    );
    assert_eq!(fs::read(f.home.join("companion.json")).unwrap(), companion);
    f.selected(&f.original["package"]);
    f.retained();
    assert!(!f.home.join("native-update-owner.json").exists());
    assert!(!f.home.join("native-update.json").exists());
}

#[tokio::test]
async fn complete_bundle_apply_and_same_transaction_compensation_preserve_state() {
    let f = Fixture::new();
    let plan = f.plan();
    let baseline_companion = fs::read(f.home.join("companion.json")).unwrap();
    let tx = prepare(&f, "apply").await;
    let mut wrong = plan.clone();
    wrong["installation_id"] = json!(uuid::Uuid::new_v4().to_string());
    assert!(
        native_update::apply(&f.home, &tx, &wrong, false)
            .await
            .is_err()
    );
    f.selected(&plan["previous"]);
    native_update::apply(&f.home, &tx, &plan, false)
        .await
        .unwrap();
    f.selected(&plan["candidate"]);
    // Lost apply replies may repeat the exact frozen selection without a lookup.
    native_update::apply(&f.home, &tx, &plan, false)
        .await
        .unwrap();
    let mut changed = plan.clone();
    changed["candidate"]["bundle"]["version"] = json!("different-candidate");
    assert!(
        native_update::apply(&f.home, &tx, &changed, false)
            .await
            .is_err()
    );
    native_update::apply(&f.home, &tx, &plan, true)
        .await
        .unwrap();
    f.selected(&plan["previous"]);
    assert_eq!(
        fs::read(f.home.join("companion.json")).unwrap(),
        baseline_companion
    );
    assert_eq!(native_update::status(&f.home).unwrap()["phase"], "applied");
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    assert!(
        native_update::restore(&f.home, &tx, "rolled_back")
            .await
            .is_err(),
        "Unverified rollback must not reopen admission"
    );
    f.retained();
}

#[tokio::test]
async fn explicit_rollback_uses_original_frozen_previous_not_newly_captured_current() {
    let f = Fixture::new();
    let plan = f.plan();
    let tx = prepare(&f, "apply").await;
    native_update::apply(&f.home, &tx, &plan, false)
        .await
        .unwrap();
    // This fixture tests source selection, not service health. Supply the
    // documented pre-existing verified receipt, then use actual stopped restore
    // so the next Worker rollback can capture a new transaction with B current.
    let journal_path = f.home.join("native-update.json");
    let mut journal = read_json(&journal_path);
    journal["phase"] = json!("verified");
    write_json(&journal_path, &journal);
    native_update::restore(&f.home, &tx, "applied")
        .await
        .unwrap();
    let rollback_tx = prepare(&f, "rollback").await;
    assert_ne!(rollback_tx, tx);
    assert_eq!(read_json(&journal_path)["previous"], plan["candidate"]);
    let mut wrong = plan.clone();
    wrong["candidate"] = plan["previous"].clone();
    assert!(
        native_update::apply(&f.home, &rollback_tx, &wrong, true)
            .await
            .is_err()
    );
    native_update::apply(&f.home, &rollback_tx, &plan, true)
        .await
        .unwrap();
    native_update::apply(&f.home, &rollback_tx, &plan, true)
        .await
        .unwrap();
    f.selected(&plan["previous"]);
    let journal = read_json(&journal_path);
    assert_eq!(journal["previous"], plan["candidate"]);
    assert_eq!(journal["candidate"], plan["previous"]);
    assert_eq!(journal["selected"], "candidate");
    assert_eq!(journal["direction"], "rolled_back");
    assert_eq!(
        native_update::status(&f.home).unwrap()["version"],
        "fixture-a"
    );
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    f.retained();
}

#[tokio::test]
async fn edits_after_plan_or_prepare_are_preserved_and_stop_selection() {
    let f = Fixture::new();
    let plan = f.plan();
    let tx = prepare(&f, "apply").await;
    let config_path = f.home.join("config.json");
    let original_config = fs::read(&config_path).unwrap();
    let mut changed = read_json(&config_path);
    changed["label"] = json!("User edit while stopped");
    write_json(&config_path, &changed);
    let edited = fs::read(&config_path).unwrap();
    assert!(
        native_update::apply(&f.home, &tx, &plan, false)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&config_path).unwrap(), edited);
    f.selected(&plan["previous"]);
    config::private_write(&config_path, &original_config).unwrap();
    let companion_path = f.home.join("companion.json");
    let mut companion = read_json(&companion_path);
    companion["profiles"]["default"]["voice"] = json!("user-selected-voice");
    write_json(&companion_path, &companion);
    let edited = fs::read(&companion_path).unwrap();
    assert!(
        native_update::apply(&f.home, &tx, &plan, false)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&companion_path).unwrap(), edited);
    assert_eq!(
        read_json(&f.home.join("native-installation.json"))["package"],
        plan["previous"]
    );
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    f.retained();
}

#[tokio::test]
async fn enrollment_binds_one_fixed_owner_without_policy_or_second_scheduler() {
    let f = Fixture::new();
    let owner_config = f._root.path().join("owner-config.json");
    let package = &f.original["package"]["bundle"];
    let config = json!({
        "schema":1,"installation_id":f.original["id"],"home":f.home,
        "component_id":"companion","participant_id":"companion",
        "components":["companion","instance"],
        "candidate":f.baseline_package(),"helper_package":package["bundle"],
        "helper_manifest_sha256":package["manifest_sha256"],
        "helper_executable":package["entrypoints"]["service"]
    });
    write_json(&owner_config, &config);
    let self_replacing_config = f._root.path().join("self-replacing-owner.json");
    let mut self_replacing = config.clone();
    self_replacing["helper_executable"] = json!(amplifier_native_platform::installed_binary(
        &native_bundle::APPLICATION,
        &f.home
    ));
    write_json(&self_replacing_config, &self_replacing);
    assert!(
        native_update_protocol::claim_owner(&self_replacing_config)
            .unwrap_err()
            .to_string()
            .contains("stable executable")
    );
    assert!(!f.home.join("native-update-owner.json").exists());
    let delta = native_update_protocol::claim_owner(&owner_config).unwrap();
    let marker = fs::read(f.home.join("native-update-owner.json")).unwrap();
    assert_eq!(
        native_update_protocol::claim_owner(&owner_config).unwrap(),
        delta
    );
    assert_eq!(
        native_update_protocol::registration(&owner_config).unwrap(),
        delta
    );
    assert_eq!(delta["components"][0]["protocol"], 2);
    assert_eq!(delta["participants"][0]["protocol"], 1);
    assert_eq!(
        delta["components"][0]["restart_scope"],
        json!({"services":[],"instance":false,"slack_homes":[]})
    );
    assert!(delta.get("policy").is_none() && delta.get("schedule").is_none());
    assert!(
        native_update::run(&f.home, &f.baseline_package())
            .await
            .is_err(),
        "Standalone apply cannot race an enrolled owner"
    );
    let replacement = f._root.path().join("another-owner-config.json");
    write_json(&replacement, &config);
    assert!(native_update_protocol::claim_owner(&replacement).is_err());
    let mut changed = config;
    changed["component_id"] = json!("other-component");
    changed["components"] = json!(["other-component", "instance"]);
    write_json(&owner_config, &changed);
    assert!(native_update_protocol::registration(&owner_config).is_err());
    assert_eq!(
        fs::read(f.home.join("native-update-owner.json")).unwrap(),
        marker
    );
    assert!(!f.home.join("native-update.json").exists());
    f.retained();
}

#[tokio::test]
async fn enrollment_after_standalone_plan_prevents_prepare_without_mutation() {
    let f = Fixture::new();
    let plan = f.plan();
    assert_eq!(plan["available"], true);
    assert!(!f.home.join("native-update-owner.json").exists());
    assert!(!f.home.join("native-update.json").exists());

    // Deterministically insert a real enrollment into the interval between the
    // standalone caller's initial ownership check/plan and its locked prepare.
    let owner_config = f._root.path().join("owner-after-plan.json");
    let package = &f.original["package"]["bundle"];
    write_json(
        &owner_config,
        &json!({
            "schema":1,"installation_id":f.original["id"],"home":f.home,
            "component_id":"companion","participant_id":"companion",
            "components":["companion","instance"],
            "candidate":plan["candidate"]["bundle"]["bundle"],
            "helper_package":package["bundle"],
            "helper_manifest_sha256":package["manifest_sha256"],
            "helper_executable":package["entrypoints"]["service"]
        }),
    );
    let registration = native_update_protocol::claim_owner(&owner_config).unwrap();
    let marker = fs::read(f.home.join("native-update-owner.json")).unwrap();
    let installed = fs::read(f.home.join("native-installation.json")).unwrap();
    let companion = fs::read(f.home.join("companion.json")).unwrap();
    let admission = update_admission::status(&f.home).unwrap();
    for operation in ["apply", "rollback"] {
        let transaction = uuid::Uuid::new_v4().to_string();
        let context = json!({"id":transaction,"owner":"standalone",
            "plan":{"operation":operation}});
        assert!(
            native_update::prepare(&f.home, &transaction, &context)
                .await
                .unwrap_err()
                .to_string()
                .contains("enrolled updater")
        );
        assert!(!f.home.join("native-update.json").exists());
        assert_eq!(update_admission::status(&f.home).unwrap(), admission);
    }
    let db = Connection::open(f.home.join("state.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM native_update_admission", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        native_update_protocol::registration(&owner_config).unwrap(),
        registration
    );
    assert_eq!(
        fs::read(f.home.join("native-update-owner.json")).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(f.home.join("native-installation.json")).unwrap(),
        installed
    );
    assert_eq!(fs::read(f.home.join("companion.json")).unwrap(), companion);
    f.selected(&plan["previous"]);
    f.retained();
}

#[tokio::test]
async fn shared_only_outcomes_restore_same_bundle_and_remain_immutable_on_retry() {
    let f = Fixture::new();
    for outcome in ["applied", "rolled_back"] {
        let tx = uuid::Uuid::new_v4().to_string();
        let mut shared = context(&tx, "apply");
        shared["plan"]["components"] = json!(["instance"]);
        assert_eq!(
            native_update::prepare(&f.home, &tx, &shared).await.unwrap()["ready"],
            true
        );
        // No local bundle apply happened. Simulate successful service health,
        // as above; actual restart/health is covered only by the CLI suite.
        let journal_path = f.home.join("native-update.json");
        let mut journal = read_json(&journal_path);
        assert_eq!(journal["direction"], "unchanged");
        journal["phase"] = json!("verified");
        write_json(&journal_path, &journal);
        assert_eq!(
            native_update::restore(&f.home, &tx, outcome).await.unwrap()["restored"],
            true
        );
        assert_eq!(
            native_update::restore(&f.home, &tx, outcome).await.unwrap()["restored"],
            true
        );
        let different = if outcome == "applied" {
            "rolled_back"
        } else {
            "applied"
        };
        assert!(
            native_update::restore(&f.home, &tx, different)
                .await
                .is_err()
        );
        assert_eq!(update_admission::status(&f.home).unwrap()["closed"], false);
        f.selected(&f.original["package"]);
        f.retained();
    }
}

#[tokio::test]
async fn declined_busy_prepare_can_be_verified_and_restored_without_touching_work() {
    let f = Fixture::new();
    let plan = f.plan();
    let db = Connection::open(f.home.join("state.sqlite3")).unwrap();
    db.execute("UPDATE conversations SET status='working'", [])
        .unwrap();
    let before = protected_state(&f.home);
    let tx = uuid::Uuid::new_v4().to_string();
    let pending = context(&tx, "apply");
    let result = native_update::prepare(&f.home, &tx, &pending)
        .await
        .unwrap();
    assert_eq!(result["ready"], false);
    assert_eq!(native_update::status(&f.home).unwrap()["phase"], "deferred");
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], false);
    let deferred = fs::read(f.home.join("native-update.json")).unwrap();
    for rollback in [false, true] {
        assert!(
            native_update::apply(&f.home, &tx, &plan, rollback)
                .await
                .is_err()
        );
        assert_eq!(
            fs::read(f.home.join("native-update.json")).unwrap(),
            deferred
        );
    }
    let mut changed = pending.clone();
    changed["plan"]["components"] = json!(["instance"]);
    assert!(
        native_update::prepare(&f.home, &tx, &changed)
            .await
            .is_err()
    );
    assert_eq!(
        native_update::verify(&f.home, &tx).await.unwrap()["deferred"],
        true
    );
    assert!(
        native_update::restore(&f.home, &tx, "applied")
            .await
            .is_err()
    );
    assert_eq!(
        native_update::restore(&f.home, &tx, "unchanged")
            .await
            .unwrap()["restored"],
        true
    );
    assert_eq!(
        native_update::restore(&f.home, &tx, "unchanged")
            .await
            .unwrap()["restored"],
        true
    );
    assert_eq!(protected_state(&f.home), before);
    assert!(!f.home.join("runtime.json").exists());
    f.selected(&f.original["package"]);
}

#[tokio::test]
async fn worker_can_retry_same_deferred_transaction_when_work_becomes_idle() {
    let f = Fixture::new();
    let db = Connection::open(f.home.join("state.sqlite3")).unwrap();
    let original: String = db
        .query_row("SELECT status FROM conversations", [], |row| row.get(0))
        .unwrap();
    db.execute("UPDATE conversations SET status='working'", [])
        .unwrap();
    let tx = uuid::Uuid::new_v4().to_string();
    let pending = context(&tx, "apply");
    assert_eq!(
        native_update::prepare(&f.home, &tx, &pending)
            .await
            .unwrap()["ready"],
        false
    );
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], false);
    // The application, not the updater, finishes existing accepted work.
    db.execute("UPDATE conversations SET status=?1", [&original])
        .unwrap();
    assert_eq!(
        native_update::prepare(&f.home, &tx, &pending)
            .await
            .unwrap()["ready"],
        true
    );
    assert_eq!(native_update::status(&f.home).unwrap()["phase"], "prepared");
    assert_eq!(
        update_admission::status(&f.home).unwrap()["transaction_id"],
        tx
    );
    f.selected(&f.original["package"]);
    f.retained();
}

#[tokio::test]
async fn startup_boundary_failure_keeps_admission_closed_and_allows_only_restore_recovery() {
    let f = Fixture::new();
    let tx = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        native_update::prepare(&f.home, &tx, &context(&tx, "apply"))
            .await
            .unwrap()["ready"],
        true
    );
    let journal_path = f.home.join("native-update.json");
    let mut journal = read_json(&journal_path);
    assert_eq!(journal["original_running"], false);
    let startup = journal["startup"].clone();
    // Synthetic files are not executable: simulate only the already verified
    // checkpoint. This test establishes restore ordering, not OS manager health.
    journal["phase"] = json!("verified");
    for invalid in [
        {
            let mut snapshot = startup.clone();
            snapshot["account"] = json!("different-account");
            snapshot
        },
        {
            let mut snapshot = startup.clone();
            snapshot["registered"] = json!(true);
            snapshot["definition"] = json!({
                "path":null,"kind":"fixture","text":"changed definition",
                "sha256":"unverified-digest"
            });
            snapshot
        },
    ] {
        journal["startup"] = invalid;
        write_json(&journal_path, &journal);
        assert!(
            native_update::restore(&f.home, &tx, "unchanged")
                .await
                .is_err()
        );
        journal = read_json(&journal_path);
        assert_eq!(journal["phase"], "restoring");
        assert_eq!(journal["restore_started"], true);
        assert_ne!(journal["restore_boundary_verified"], true);
        assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
        let plan = json!({});
        assert!(
            native_update::apply(&f.home, &tx, &plan, true)
                .await
                .is_err()
        );
        assert!(native_update::verify(&f.home, &tx).await.is_err());
        assert!(!f.home.join("runtime.json").exists());
        f.retained();
    }
    // Repair only the deliberately altered test snapshot. No production
    // definition, service, process, task, or received work is reconstructed.
    journal["startup"] = startup;
    write_json(&journal_path, &journal);
    assert_eq!(
        native_update::restore(&f.home, &tx, "unchanged")
            .await
            .unwrap()["restored"],
        true
    );
    assert_eq!(read_json(&journal_path)["restore_boundary_verified"], true);
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], false);
    assert!(!f.home.join("runtime.json").exists());
    f.selected(&f.original["package"]);
    f.retained();
}

async fn completed_fixture_update(f: &Fixture, plan: &Value) -> String {
    let tx = prepare(f, "apply").await;
    native_update::apply(&f.home, &tx, plan, false)
        .await
        .unwrap();
    // These fixtures contain native-format headers, not runnable programs.
    // Only simulate the health checkpoint; actual CLI health has its own suite.
    let path = f.home.join("native-update.json");
    let mut journal = read_json(&path);
    journal["phase"] = json!("verified");
    write_json(&path, &journal);
    native_update::restore(&f.home, &tx, "applied")
        .await
        .unwrap();
    tx
}

fn standalone_rollback_context(tx: &str, previous: &str, plan: &Value) -> Value {
    json!({"id":tx,"owner":"standalone","plan":{"operation":"rollback"},
        "previous_transaction":previous,"rollback_plan":plan})
}

#[tokio::test]
async fn explicit_rollback_retains_target_before_apply_and_can_resume_to_that_target() {
    let f = Fixture::new();
    let plan = f.plan();
    let first = completed_fixture_update(&f, &plan).await;
    let tx = uuid::Uuid::new_v4().to_string();
    let context = standalone_rollback_context(&tx, &first, &plan);
    native_update::prepare(&f.home, &tx, &context)
        .await
        .unwrap();
    let path = f.home.join("native-update.json");
    let mut journal = read_json(&path);
    assert!(journal["frozen_plan"].is_null());
    assert_eq!(journal["context"]["rollback_plan"], plan);
    let context_hash = journal["context_hash"].clone();
    // An interrupted intent can reconstruct prepare without losing A's target.
    journal["phase"] = json!("intent");
    write_json(&path, &journal);
    native_update::prepare(&f.home, &tx, &context)
        .await
        .unwrap();
    let retained = read_json(&path)["context"]["rollback_plan"].clone();
    native_update::apply(&f.home, &tx, &retained, true)
        .await
        .unwrap();
    f.selected(&plan["previous"]);
    journal = read_json(&path);
    assert_eq!(journal["frozen_plan"], plan);
    assert_eq!(journal["context"], context);
    assert_eq!(journal["context_hash"], context_hash);
    journal["phase"] = json!("verified");
    write_json(&path, &journal);
    assert_eq!(
        native_update::recover(&f.home, &tx, false).await.unwrap()["restored"],
        true
    );
    let finished = read_json(&path);
    assert_eq!(finished["restore_outcome"], "rolled_back");
    assert_ne!(finished["rollback_aborted"], true);
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], false);
    f.selected(&plan["previous"]);
    f.retained();
}

#[tokio::test]
async fn interrupted_explicit_rollback_abort_preserves_b_until_verified_restore() {
    let f = Fixture::new();
    let plan = f.plan();
    let first = completed_fixture_update(&f, &plan).await;
    let tx = uuid::Uuid::new_v4().to_string();
    let context = standalone_rollback_context(&tx, &first, &plan);
    native_update::prepare(&f.home, &tx, &context)
        .await
        .unwrap();
    native_update::apply(&f.home, &tx, &plan, true)
        .await
        .unwrap();
    f.selected(&plan["previous"]);
    let path = f.home.join("native-update.json");
    let mut journal = read_json(&path);
    let context_hash = journal["context_hash"].clone();
    // Simulate the abort journal's durable selection, followed by a crash before
    // any A -> B file exchange. This is not a synthetic health success.
    journal["rollback_aborted"] = json!(true);
    journal["rollback_failure"] = json!("fixture target failed health");
    journal["selected"] = json!("previous");
    journal["direction"] = json!("unchanged");
    journal["phase"] = json!("switching");
    journal["launch_nonce"] = Value::Null;
    write_json(&path, &journal);
    config::private_write(&f.home.join("native-update-owner.json"), b"{}").unwrap();
    assert!(
        native_update::abort_rollback(&f.home, &tx, "retry")
            .await
            .is_err()
    );
    assert_eq!(
        read_json(&path),
        journal,
        "An enrolled home cannot bypass its owner"
    );
    fs::remove_file(f.home.join("native-update-owner.json")).unwrap();
    assert_eq!(
        native_update::abort_rollback(&f.home, &tx, "retry")
            .await
            .unwrap()["rollback_aborted"],
        true
    );
    f.selected(&plan["candidate"]);
    journal = read_json(&path);
    assert_eq!(journal["previous"], plan["candidate"]);
    assert_eq!(journal["candidate"], plan["previous"]);
    assert_eq!(journal["frozen_plan"], plan);
    assert_eq!(journal["context"], context);
    assert_eq!(journal["context_hash"], context_hash);
    assert_eq!(journal["rollback_failure"], "fixture target failed health");
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    assert!(
        native_update::restore(&f.home, &tx, "unchanged")
            .await
            .is_err()
    );
    assert!(
        native_update::apply(&f.home, &tx, &plan, true)
            .await
            .is_err()
    );
    assert_eq!(
        read_json(&path),
        journal,
        "Neither unverified restore nor A replay may advance it"
    );
    // Simulate B's separate successful health checkpoint; it still cannot be
    // reported as applying the failed target or completing the requested rollback.
    journal["phase"] = json!("verified");
    write_json(&path, &journal);
    for wrong in ["applied", "rolled_back"] {
        assert!(
            native_update::restore(&f.home, &tx, wrong)
                .await
                .unwrap_err()
                .to_string()
                .contains("aborted rollback can only restore unchanged")
        );
        assert_eq!(read_json(&path), journal);
        assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    }
    // Now simulate interrupted restore start. Only forward restoration may
    // reopen admission; changing the selected bundle is no longer allowed.
    journal["phase"] = json!("restoring");
    journal["restore_started"] = json!(true);
    journal["restore_outcome"] = json!("unchanged");
    write_json(&path, &journal);
    assert!(
        native_update::abort_rollback(&f.home, &tx, "too late")
            .await
            .is_err()
    );
    assert!(native_update::recover(&f.home, &tx, true).await.is_err());
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], true);
    let restored = native_update::recover(&f.home, &tx, false).await.unwrap();
    assert_eq!(restored["restored"], true);
    assert_eq!(restored["rollback_aborted"], true);
    assert_eq!(restored["outcome"], "unchanged");
    let finished = read_json(&path);
    assert_eq!(finished["phase"], "restored");
    assert_eq!(finished["direction"], "unchanged");
    assert_eq!(finished["selected"], "previous");
    assert_eq!(update_admission::status(&f.home).unwrap()["closed"], false);
    f.selected(&plan["candidate"]);
    f.retained();
}
