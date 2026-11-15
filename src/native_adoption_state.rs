//! Raw state preservation for the explicitly supported, already stopped 0.9
//! installation. This module never opens AppState, migrates original state,
//! starts a router, uses credentials, acknowledges an outcome, or resumes
//! delivery. Explicit prepare_gate adds only the existing admission schema.
//! Fixed secret storage is read only to preserve hashes and validate original
//! token bindings; secret contents are never returned or written.
//!
//! The caller must first establish process/listener/child absence and hold the
//! installation's service and operation locks. BEGIN IMMEDIATE then owns the
//! existing SQLite writer reservation for the entire guard lifetime. It does
//! not replace those external locks or prove that external effects completed.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const FORMAT: &str = "conversations-legacy-0.9-state-v1";
const STARTUP_TOKEN_LIFETIME_MS: i64 = 100 * 366 * 86400 * 1000;
const MARKERS: &[&str] = &[
    "default-agent-import-sources",
    "named-import-sources",
    "shared-rooms",
];

// The old 7afec816 schema after its three completed migrations. The declared
// type is checked as well as the complete ordered column set. We do not infer
// safety from an unknown table or a newer schema that happens to be idle.
const TABLES: &[(&str, &str)] = &[
    ("agent_endpoints", "id:T config:T updated_at:I"),
    (
        "attachments",
        "id:T conversation_id:T name:T mime_type:T data:B bytes:I created_by:T created_at:I message_id:T",
    ),
    ("audit", "seq:I at:I kind:T conversation_id:T data:T"),
    ("browser_sessions", "hash:T owner:T csrf:T expires_at:I"),
    (
        "conversations",
        "id:T title:T created_at:I updated_at:I status:T note:T session_id:T endpoint:T agent_info:T surface:T read_only:I owner:T shared:I",
    ),
    (
        "grants",
        "hash:T conversation_id:T expires_at:I owner_scope:I",
    ),
    (
        "import_sources",
        "source:T surface:T token_id:T enabled:I created_at:I updated_at:I",
    ),
    ("imports", "source:T session_id:T conversation_id:T"),
    (
        "input_authority",
        "message_id:T browser_hash:T token_hash:T",
    ),
    (
        "input_deliveries",
        "sequence:I id:T message_id:T conversation_id:T agent_id:T participation:T state:T mode:T target_turn:T session_id:T endpoint:T receipt:T attention:T updated_at:I",
    ),
    ("login_attempts", "key:T failures:I locked_until:I"),
    (
        "messages",
        "seq:I id:T conversation_id:T role:T text:T status:T created_at:I updated_at:I actor:T agent_id:T targets:T",
    ),
    ("migrations", "name:T"),
    (
        "room_agents",
        "conversation_id:T agent_id:T session_id:T endpoint:T last_seq:I",
    ),
    (
        "room_members",
        "conversation_id:T username:T role:T joined_at:I",
    ),
    ("sqlite_sequence", "name: seq:"),
    ("tokens", "hash:T id:T scopes:T expires_at:I"),
    ("views", "id:T value:T revision:I updated_at:I owner:T"),
];

// Only empty additive tables created by the accepted successor's AppState
// migrations are tolerated by the post-migration preservation check. Work or
// registration in any of them requires another, explicit lifecycle boundary.
const EMPTY_ADDITIONS: &[&str] = &[
    "companion_action_observers",
    "companion_actions",
    "companion_awareness",
    "companion_awareness_appends",
    "companion_awareness_decisions",
    "companion_browser_diagnostics",
    "companion_calls",
    "companion_delegations",
    "companion_events",
    "companion_feedback",
    "companion_observation_revisions",
    "companion_observations",
    "companion_observers",
    "companion_surface_capabilities",
    "companion_surface_commands",
    "companion_surfaces",
    "companion_tool_grants",
    "companion_uploads",
    "companion_voice_batches",
    "companion_voice_settings",
    "huddle_auto_members",
    "huddle_auto_rooms",
    "huddle_ensures",
    "huddle_installations",
    "huddle_participants",
    "huddle_peer_deliveries",
    "huddle_posts",
    "huddle_preferences",
    "huddles",
    "native_update_activity",
    "native_update_admission",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Readiness {
    pub ready: bool,
    pub working_conversations: u64,
    pub streaming_messages: u64,
    pub active_deliveries: u64,
    pub incomplete_incorporations: u64,
    pub retained_unknown: u64,
    pub retained_not_accepted: u64,
    pub unsupported_states: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TableSnapshot {
    pub name: String,
    pub columns: Vec<String>,
    pub original_schema_sha256: String,
    pub rows: u64,
    pub sha256: String,
}

/// Caller-observed wall-clock interval containing every controlled AppState
/// initialization covered by this verification, including the serving start.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InitializationWindow {
    pub min_utc_ms: i64,
    pub max_utc_ms: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatePhase {
    Prepared,
    Restored,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedGate {
    pub transaction: String,
    pub phase: GatePhase,
}

/// Original delivery identity and a hash of every original typed row field.
/// This is retained uncertainty, never a completion or replay permission.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoricalInput {
    pub id: String,
    pub row_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretKind {
    Directory,
    File,
}

/// Paths are relative to the fixed secrets directory. No credential content
/// or normalized token digest is recorded separately from these file hashes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SecretEntry {
    pub path: String,
    pub kind: SecretKind,
    pub sha256: Option<String>,
    pub bytes: u64,
    pub uid: u32,
    pub mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StartupRefresh {
    local_control_expiry: i64,
    context_provider_expiry: i64,
    default_endpoint_updated_at: i64,
}

/// Contains hashes, schema names, delivery IDs and counts, never transcripts,
/// token values, grant hashes, session IDs or endpoint commands.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub format: String,
    pub config_sha256: String,
    pub schema_sha256: String,
    pub saved_state_sha256: String,
    pub tables: Vec<TableSnapshot>,
    pub readiness: Readiness,
    pub historical_inputs: Vec<HistoricalInput>,
    pub secrets: Vec<SecretEntry>,
    startup_refresh: StartupRefresh,
}

pub(crate) struct AuditGuard {
    db: Connection,
    home: PathBuf,
    snapshot: Snapshot,
}

impl AuditGuard {
    /// Opens an existing database only. There is no CREATE flag and no schema
    /// or row mutation; query_only is enabled after obtaining the reservation.
    /// Busy acquisition refuses immediately rather than waiting for live work.
    pub(crate) fn begin(home: &Path) -> Result<Self> {
        let (db, config_bytes) = begin_existing(home)?;
        validate_schema(&db, false)?;
        let snapshot = capture(&db, &config_bytes, home)?;
        ensure!(
            config_bytes == config_bytes_at(home)?,
            "Legacy configuration changed during audit"
        );
        Ok(Self {
            db,
            home: home.to_owned(),
            snapshot,
        })
    }

    pub(crate) fn report(&self) -> &Readiness {
        &self.snapshot.readiness
    }

    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Recheck immediately before the caller's exchange. SQLite writers stay
    /// excluded throughout. This also rereads the configuration bytes; it is
    /// not a hostile-filesystem CAS or proof of absent external processes.
    pub(crate) fn verify_current(&self) -> Result<()> {
        validate_schema(&self.db, false)?;
        let current = capture(&self.db, &config_bytes_at(&self.home)?, &self.home)?;
        ensure!(
            current == self.snapshot,
            "Legacy saved state changed during adoption"
        );
        Ok(())
    }

    /// A second already-locked original copy must match the complete saved
    /// semantic state, including schema and the original configuration bytes.
    #[cfg(test)]
    pub(crate) fn verify_matches(&self, expected: &Snapshot) -> Result<()> {
        validate_snapshot(expected)?;
        self.verify_current()?;
        ensure!(
            &self.snapshot == expected,
            "Legacy saved-state snapshot differs"
        );
        Ok(())
    }

    /// Convert this continuously held writer reservation into the successor's
    /// durable admission gate before permitting AppState initialization. No old
    /// table or row is changed. The caller retains service/operation ownership.
    /// A commit error is uncertain; do not redispatch or clear a possible gate.
    pub(crate) fn prepare_gate(self, transaction: &str) -> Result<Snapshot> {
        uuid::Uuid::parse_str(transaction).context("Use an adoption transaction UUID")?;
        self.verify_current()?;
        ensure!(
            self.report().ready,
            "Legacy state contains active or unsupported work"
        );
        self.db.execute_batch("PRAGMA query_only=OFF;")?;
        crate::update_admission::migrate(&self.db)?;
        self.db.execute("INSERT INTO native_update_admission(transaction_id,phase,acquired_at,restored_at) VALUES(?1,'prepared',?2,NULL)", rusqlite::params![transaction,crate::state::now_ms()])?;
        // COMMIT is the first point where the database reservation may end;
        // the new durable barrier is visible atomically with that release.
        self.db.execute_batch("COMMIT")?;
        Ok(self.snapshot.clone())
    }
}

impl Drop for AuditGuard {
    fn drop(&mut self) {
        let _ = self.db.execute_batch("ROLLBACK");
    }
}

/// Inspect an interrupted adoption before any successor initialization. Only
/// the exact original state plus the existing admission schema is permitted;
/// even an empty successor feature table or refreshed timestamp refuses. The
/// caller owns origin/old-image proof and any explicit restoration Start.
pub(crate) fn verify_before_startup(
    home: &Path,
    before: &Snapshot,
    expected_gate: Option<&ExpectedGate>,
) -> Result<()> {
    validate_snapshot(before)?;
    ensure!(
        before.readiness.ready,
        "Original state was not ready for adoption"
    );
    let (db, config_bytes) = begin_existing(home)?;
    let result = (|| {
        let mut query = db.prepare("SELECT name FROM sqlite_schema WHERE type='table'")?;
        for name in query.query_map([], |r| r.get::<_, String>(0))? {
            let name = name?;
            ensure!(
                TABLES.iter().any(|(original, _)| *original == name)
                    || matches!(
                        name.as_str(),
                        "native_update_activity" | "native_update_admission"
                    ),
                "Successor schema exists before expected initialization"
            );
        }
        validate_schema_with_gate(&db, true, expected_gate)?;
        let schema = query_digest(&db,"SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE tbl_name NOT IN ('native_update_admission','native_update_activity') ORDER BY type,name",None)?.1;
        ensure!(
            schema == before.schema_sha256,
            "Original schema changed before expected initialization"
        );
        ensure!(
            hash(&config_bytes) == before.config_sha256,
            "Original configuration changed before expected initialization"
        );
        ensure!(
            table_snapshots(&db, None)? == before.tables && readiness(&db)? == before.readiness,
            "Original rows changed before expected initialization"
        );
        ensure!(
            secret_snapshot(home, &db)? == before.secrets,
            "Original secret files changed before expected initialization"
        );
        ensure!(
            config_bytes == config_bytes_at(home)?,
            "Configuration changed during pre-startup inspection"
        );
        Ok(())
    })();
    let rollback = db.execute_batch("ROLLBACK");
    result?;
    rollback?;
    Ok(())
}

/// Verify original state under a writer reservation after controlled startup.
/// The candidate may be alive with its admission gate already closed; the
/// caller retains installation operation ownership. This verifies saved state,
/// not process absence or external effect completion. The only
/// permitted changes to old rows are the two known token expiry refreshes and
/// default endpoint updated_at within the observed initialization interval.
/// Historical uncertain outcomes remain exactly as recorded.
pub(crate) fn verify_preserved(
    home: &Path,
    before: &Snapshot,
    window: InitializationWindow,
    expected_gate: Option<&ExpectedGate>,
) -> Result<()> {
    validate_snapshot(before)?;
    ensure!(
        before.readiness.ready,
        "Original state was not ready for adoption"
    );
    let (db, config_bytes) = begin_existing(home)?;
    let result = (|| {
        validate_schema_with_gate(&db, true, expected_gate)?;
        ensure!(
            hash(&config_bytes) == before.config_sha256,
            "Original configuration bytes changed"
        );
        validate_default_endpoint(&db, &config_bytes)?;
        ensure!(
            secret_snapshot(home, &db)? == before.secrets,
            "Original secret files or private policy changed"
        );
        validate_initialization_window(&db, window)?;
        ensure!(
            readiness(&db)? == before.readiness,
            "Original work or uncertainty state changed"
        );
        let actual = table_snapshots(&db, Some(&before.startup_refresh))?;
        ensure!(
            actual == before.tables,
            "Original protected rows changed during migration"
        );
        ensure!(
            config_bytes == config_bytes_at(home)?,
            "Configuration changed during preservation check"
        );
        Ok(())
    })();
    let rollback = db.execute_batch("ROLLBACK");
    result?;
    rollback?;
    Ok(())
}

fn begin_existing(home: &Path) -> Result<(Connection, Vec<u8>)> {
    ensure!(
        cfg!(target_os = "linux"),
        "Legacy saved-state adoption currently requires Linux"
    );
    let config = config_bytes_at(home)?;
    ordinary_file(&home.join("state.sqlite3"))?;
    let db = Connection::open_with_flags(
        home.join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Cannot open existing legacy state")?;
    db.busy_timeout(Duration::ZERO)?;
    db.execute_batch("BEGIN IMMEDIATE; PRAGMA query_only=ON;")
        .context("Cannot reserve stopped legacy state")?;
    Ok((db, config))
}

fn config_bytes_at(home: &Path) -> Result<Vec<u8>> {
    let path = home.join("config.json");
    ordinary_file(&path)?;
    ensure!(
        fs::metadata(&path)?.len() <= 1024 * 1024,
        "Legacy configuration is too large"
    );
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Legacy configuration is too large"
    );
    Ok(bytes)
}

fn ordinary_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Legacy state requires an ordinary file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Legacy state cannot use a reparse file"
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn secret_snapshot(_home: &Path, _db: &Connection) -> Result<Vec<SecretEntry>> {
    anyhow::bail!("Legacy secret preservation currently requires Linux")
}

#[cfg(target_os = "linux")]
fn secret_snapshot(home: &Path, db: &Connection) -> Result<Vec<SecretEntry>> {
    protected_secrets::snapshot(home, db)
}

#[cfg(target_os = "linux")]
mod protected_secrets {
    use super::*;
    use std::{
        fs::{File, Metadata, OpenOptions},
        io::Read,
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
    };
    use zeroize::Zeroizing;

    fn expected_token(db: &Connection, id: &str, scopes: &str) -> Result<Option<String>> {
        let mut query = db.prepare("SELECT hash,scopes FROM tokens WHERE id=?1")?;
        let found = query
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            found.len() <= 1,
            "Original secret token identity is ambiguous"
        );
        let Some((digest, actual_scopes)) = found.into_iter().next() else {
            return Ok(None);
        };
        ensure!(
            actual_scopes == scopes
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Original secret token scope or hash is unsupported"
        );
        Ok(Some(digest))
    }

    fn bindings(db: &Connection) -> Result<BTreeMap<String, String>> {
        let mut expected = BTreeMap::new();
        for (path, id, scopes) in [
            ("control-token", "local-control", "[\"admin\"]"),
            ("context-token", "context-provider", "[\"context\"]"),
        ] {
            expected.insert(
                path.to_owned(),
                expected_token(db, id, scopes)?
                    .context("Original fixed secret token is missing")?,
            );
        }
        let mut query =
            db.prepare("SELECT source,token_id,enabled FROM import_sources ORDER BY source")?;
        let sources = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut known_ids = BTreeSet::new();
        for (source, id, enabled) in sources {
            ensure!(
                !source.is_empty()
                    && source.len() <= 80
                    && source
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    && matches!(enabled, 0 | 1),
                "Unsupported original import source identity"
            );
            let relative = if id == "source-importer" {
                "import-token".to_owned()
            } else {
                ensure!(
                    id == format!("source-importer:{source}"),
                    "Unsupported original import token identity"
                );
                format!("import-sources/{source}-token")
            };
            ensure!(
                known_ids.insert(id.clone()),
                "Ambiguous original import token identity"
            );
            let token = expected_token(db, &id, "[\"import\"]")?;
            ensure!(
                token.is_some() == (enabled == 1),
                "Original import token authority differs from source state"
            );
            // Revocation removes the database token, not its file. Preserve any
            // retained file but do not require or recreate a revoked secret.
            if let Some(digest) = token {
                expected.insert(relative, digest);
            }
        }
        let mut query = db.prepare(
            "SELECT id FROM tokens WHERE id='source-importer' OR id LIKE 'source-importer:%'",
        )?;
        for id in query.query_map([], |r| r.get::<_, String>(0))? {
            ensure!(
                known_ids.contains(&id?),
                "Original import token has no matching source"
            );
        }
        Ok(expected)
    }

    fn private(metadata: &Metadata, directory: bool) -> Result<()> {
        ensure!(
            !metadata.file_type().is_symlink()
                && if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                },
            "Secret storage must be ordinary files and directories"
        );
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
            "Secret storage must be private to the current OS user"
        );
        Ok(())
    }

    fn same_file(before: &Metadata, after: &Metadata) -> bool {
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.uid() == after.uid()
            && before.gid() == after.gid()
            && before.mode() == after.mode()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }

    fn opened(path: &Path, directory: bool) -> Result<File> {
        let before = fs::symlink_metadata(path)?;
        private(&before, directory)?;
        let flags = libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if directory { libc::O_DIRECTORY } else { 0 };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path)?;
        let actual = file.metadata()?;
        private(&actual, directory)?;
        ensure!(
            same_file(&before, &actual),
            "Secret storage identity changed before reading"
        );
        Ok(file)
    }

    fn file_entry(
        mut file: File,
        relative: String,
        token_hash: Option<&String>,
    ) -> Result<SecretEntry> {
        let before = file.metadata()?;
        let mut buffer = Zeroizing::new([0u8; 16 * 1024]);
        let mut token = Zeroizing::new(Vec::new());
        let mut digest = Sha256::new();
        let mut bytes = 0_u64;
        loop {
            let size = file.read(buffer.as_mut())?;
            if size == 0 {
                break;
            }
            bytes = bytes
                .checked_add(size as u64)
                .context("Secret size overflow")?;
            digest.update(&buffer[..size]);
            if token_hash.is_some() {
                token.extend_from_slice(&buffer[..size]);
            }
        }
        let after = file.metadata()?;
        private(&after, false)?;
        ensure!(
            same_file(&before, &after) && bytes == before.len(),
            "Secret storage changed while reading"
        );
        if let Some(expected) = token_hash {
            let value = std::str::from_utf8(&token)
                .context("Original token file is not UTF-8")?
                .trim();
            ensure!(
                value.len() >= 32 && hash(value.as_bytes()) == *expected,
                "Original token file does not match its saved token hash"
            );
        }
        Ok(SecretEntry {
            path: relative,
            kind: SecretKind::File,
            sha256: Some(format!("{:x}", digest.finalize())),
            bytes,
            uid: before.uid(),
            mode: before.mode() & 0o7777,
        })
    }

    fn walk(
        directory: File,
        relative: &str,
        expected: &BTreeMap<String, String>,
        entries: &mut Vec<SecretEntry>,
    ) -> Result<()> {
        let before = directory.metadata()?;
        private(&before, true)?;
        entries.push(SecretEntry {
            path: relative.to_owned(),
            kind: SecretKind::Directory,
            sha256: None,
            bytes: 0,
            uid: before.uid(),
            mode: before.mode() & 0o7777,
        });
        // The already-open directory anchors every child lookup. No filesystem
        // path from a saved record is used, and child symlinks are never followed.
        let anchor = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let mut names = fs::read_dir(&anchor)?
            .map(|item| item.map(|item| item.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        names.sort();
        for name in names {
            let name = name.to_str().context("Secret filename is not UTF-8")?;
            ensure!(
                !name.is_empty() && name != "." && name != ".." && !name.contains('/'),
                "Invalid secret filename"
            );
            let relative_name = if relative.is_empty() {
                name.to_owned()
            } else {
                format!("{relative}/{name}")
            };
            let path = anchor.join(name);
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                walk(opened(&path, true)?, &relative_name, expected, entries)?;
            } else {
                let file = opened(&path, false)?;
                entries.push(file_entry(
                    file,
                    relative_name.clone(),
                    expected.get(&relative_name),
                )?);
            }
        }
        let after = directory.metadata()?;
        private(&after, true)?;
        ensure!(
            same_file(&before, &after),
            "Secret directory changed during inspection"
        );
        Ok(())
    }

    pub(super) fn snapshot(home: &Path, db: &Connection) -> Result<Vec<SecretEntry>> {
        let expected = bindings(db)?;
        let mut entries = Vec::new();
        walk(
            opened(&home.join("secrets"), true)?,
            "",
            &expected,
            &mut entries,
        )?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for relative in expected.keys() {
            ensure!(
                entries
                    .iter()
                    .any(|entry| &entry.path == relative && entry.kind == SecretKind::File),
                "Original required secret file is missing"
            );
        }
        Ok(entries)
    }
}

fn columns(spec: &str) -> Vec<(&str, &str)> {
    spec.split_whitespace()
        .map(|s| s.split_once(':').expect("fixed column specification"))
        .collect()
}

fn validate_schema(db: &Connection, after_migration: bool) -> Result<()> {
    validate_schema_with_gate(db, after_migration, None)
}

fn validate_schema_with_gate(
    db: &Connection,
    after_migration: bool,
    expected_gate: Option<&ExpectedGate>,
) -> Result<()> {
    let mut statement =
        db.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")?;
    let actual = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    let expected: BTreeSet<_> = TABLES.iter().map(|(name, _)| (*name).to_owned()).collect();
    ensure!(
        expected.is_subset(&actual),
        "Unsupported legacy schema: missing original table"
    );
    for extra in actual.difference(&expected) {
        if extra == "native_update_admission"
            && let Some(gate) = expected_gate
        {
            validate_gate(db, gate)?;
            continue;
        }
        ensure!(
            after_migration && EMPTY_ADDITIONS.contains(&extra.as_str()),
            "Unsupported legacy schema: unexpected table"
        );
        ensure!(
            count(db, &format!("SELECT COUNT(*) FROM \"{extra}\""))? == 0,
            "Successor work tables are not empty"
        );
    }
    ensure!(
        expected_gate.is_none() || actual.contains("native_update_admission"),
        "Expected adoption admission gate is missing"
    );
    ensure!(
        count(
            db,
            "SELECT COUNT(*) FROM sqlite_schema WHERE type IN ('trigger','view')"
        )? == 0,
        "Unsupported legacy schema: executable schema object"
    );
    for (name, spec) in TABLES {
        let mut q = db.prepare(&format!("PRAGMA table_xinfo(\"{name}\")"))?;
        let found = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut wanted = columns(spec);
        if after_migration && *name == "input_deliveries" && found.len() > wanted.len() {
            wanted.extend(columns("ordinary_not_before:I steering_target_turn:T steering_session_id:T source_metadata:T source_binding:T source_receipt:T"));
            ensure!(
                count(
                    db,
                    "SELECT COUNT(*) FROM input_deliveries WHERE typeof(ordinary_not_before)<>'integer' OR ordinary_not_before<>0 OR steering_target_turn IS NOT NULL OR steering_session_id IS NOT NULL OR source_metadata IS NOT NULL OR source_binding IS NOT NULL OR source_receipt IS NOT NULL"
                )? == 0,
                "Successor delivery fields contain new work or bindings"
            );
        }
        ensure!(
            found.len() == wanted.len(),
            "Unsupported legacy schema: changed column count"
        );
        for ((name, ty, hidden), (expected_name, code)) in found.iter().zip(wanted) {
            let expected_ty = match code {
                "T" => "TEXT",
                "I" => "INTEGER",
                "B" => "BLOB",
                "" => "",
                _ => unreachable!(),
            };
            ensure!(
                name == expected_name && ty.eq_ignore_ascii_case(expected_ty) && *hidden == 0,
                "Unsupported legacy schema: changed original column"
            );
        }
    }
    let mut q = db.prepare("SELECT name FROM migrations ORDER BY name")?;
    let markers = q
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        markers
            .iter()
            .map(String::as_str)
            .eq(MARKERS.iter().copied()),
        "Unsupported or incomplete legacy migrations"
    );
    // Do not silently tolerate broken bindings that the successor could treat
    // differently. foreign_key_check is inspection; foreign_keys is not set.
    ensure!(
        !db.prepare("PRAGMA foreign_key_check")?.exists([])?,
        "Legacy state has broken foreign-key bindings"
    );
    Ok(())
}

fn validate_gate(db: &Connection, expected: &ExpectedGate) -> Result<()> {
    uuid::Uuid::parse_str(&expected.transaction)
        .context("Invalid expected adoption transaction")?;
    ensure!(
        count(db, "SELECT COUNT(*) FROM native_update_admission")? == 1,
        "Unexpected adoption admission history"
    );
    let (transaction, phase, acquired, restored): (String, String, i64, Option<i64>) = db
        .query_row(
            "SELECT transaction_id,phase,acquired_at,restored_at FROM native_update_admission",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    let phase_matches = match expected.phase {
        GatePhase::Prepared => phase == "prepared" && restored.is_none(),
        GatePhase::Restored => phase == "restored" && restored.is_some_and(|at| at >= acquired),
    };
    ensure!(
        transaction == expected.transaction && acquired >= 0 && phase_matches,
        "Adoption admission gate differs from expected transition"
    );
    Ok(())
}

fn validate_initialization_window(db: &Connection, window: InitializationWindow) -> Result<()> {
    ensure!(
        window.min_utc_ms >= 0 && window.max_utc_ms >= window.min_utc_ms,
        "Invalid observed initialization interval"
    );
    let earliest = window
        .min_utc_ms
        .checked_add(STARTUP_TOKEN_LIFETIME_MS)
        .context("Initialization expiry interval overflow")?;
    let latest = window
        .max_utc_ms
        .checked_add(STARTUP_TOKEN_LIFETIME_MS)
        .context("Initialization expiry interval overflow")?;
    let current = startup_refresh(db)?;
    ensure!(
        (earliest..=latest).contains(&current.local_control_expiry)
            && (earliest..=latest).contains(&current.context_provider_expiry)
            && (window.min_utc_ms..=window.max_utc_ms)
                .contains(&current.default_endpoint_updated_at),
        "Startup refresh lies outside the observed initialization interval"
    );
    Ok(())
}

fn count(db: &Connection, sql: &str) -> Result<u64> {
    Ok(u64::try_from(
        db.query_row(sql, [], |r| r.get::<_, i64>(0))?,
    )?)
}

fn readiness(db: &Connection) -> Result<Readiness> {
    let working_conversations = count(
        db,
        "SELECT COUNT(*) FROM conversations WHERE status='working'",
    )?;
    let streaming_messages = count(db, "SELECT COUNT(*) FROM messages WHERE status='streaming'")?;
    let active_deliveries = count(
        db,
        "SELECT COUNT(*) FROM input_deliveries WHERE state IN ('received','promptRequired','preparing','offering','submitted','received_remote','remoteQueued','ack_pending')",
    )?;
    let incomplete_incorporations = count(
        db,
        "SELECT COUNT(*) FROM input_deliveries d WHERE state='incorporated' AND NOT EXISTS(SELECT 1 FROM messages m WHERE m.id=d.target_turn AND m.conversation_id=d.conversation_id AND m.agent_id=d.agent_id AND m.role='assistant' AND m.status='complete')",
    )?;
    let unsupported_states = count(
        db,
        "SELECT COUNT(*) FROM conversations WHERE status IS NULL OR status NOT IN ('idle','error','working')",
    )? + count(
        db,
        "SELECT COUNT(*) FROM messages WHERE status IS NULL OR status NOT IN ('complete','interrupted','streaming')",
    )? + count(
        db,
        "SELECT COUNT(*) FROM input_deliveries WHERE state IS NULL OR state NOT IN ('received','promptRequired','preparing','offering','submitted','received_remote','remoteQueued','ack_pending','incorporated','completed','cancelled','dismissed','unknown','notAccepted')",
    )?;
    Ok(Readiness {
        ready: working_conversations == 0
            && streaming_messages == 0
            && active_deliveries == 0
            && incomplete_incorporations == 0
            && unsupported_states == 0,
        working_conversations,
        streaming_messages,
        active_deliveries,
        incomplete_incorporations,
        retained_unknown: count(
            db,
            "SELECT COUNT(*) FROM input_deliveries WHERE state='unknown'",
        )?,
        retained_not_accepted: count(
            db,
            "SELECT COUNT(*) FROM input_deliveries WHERE state='notAccepted'",
        )?,
        unsupported_states,
    })
}

fn validate_default_endpoint(db: &Connection, config_bytes: &[u8]) -> Result<()> {
    let config: crate::config::Config = serde_json::from_slice(config_bytes)?;
    config.validate()?;
    let agent = config
        .agent
        .context("Legacy configuration has no original default agent")?;
    let existing: String = db.query_row(
        "SELECT config FROM agent_endpoints WHERE id='default'",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        existing == serde_json::to_string(&agent)?,
        "Legacy default endpoint differs from canonical configuration"
    );
    Ok(())
}

fn startup_refresh(db: &Connection) -> Result<StartupRefresh> {
    let expiry = |id: &str| -> Result<i64> {
        let mut q = db.prepare("SELECT expires_at FROM tokens WHERE id=?1")?;
        let rows = q
            .query_map([id], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            rows.len() == 1,
            "Legacy startup token is missing or ambiguous"
        );
        Ok(rows[0])
    };
    Ok(StartupRefresh {
        local_control_expiry: expiry("local-control")?,
        context_provider_expiry: expiry("context-provider")?,
        default_endpoint_updated_at: db.query_row(
            "SELECT updated_at FROM agent_endpoints WHERE id='default'",
            [],
            |r| r.get(0),
        )?,
    })
}

fn capture(db: &Connection, config: &[u8], home: &Path) -> Result<Snapshot> {
    validate_default_endpoint(db, config)?;
    let schema_sha256 = query_digest(
        db,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
        None,
    )?
    .1;
    let mut snapshot = Snapshot {
        format: FORMAT.to_owned(),
        config_sha256: hash(config),
        schema_sha256,
        saved_state_sha256: String::new(),
        tables: table_snapshots(db, None)?,
        readiness: readiness(db)?,
        historical_inputs: historical_inputs(db)?,
        secrets: secret_snapshot(home, db)?,
        startup_refresh: startup_refresh(db)?,
    };
    snapshot.saved_state_sha256 = snapshot_digest(&snapshot)?;
    Ok(snapshot)
}

fn snapshot_digest(snapshot: &Snapshot) -> Result<String> {
    let mut value = snapshot.clone();
    value.saved_state_sha256.clear();
    Ok(hash(&serde_json::to_vec(&value)?))
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<()> {
    ensure!(
        snapshot.format == FORMAT && snapshot.saved_state_sha256 == snapshot_digest(snapshot)?,
        "Invalid saved-state snapshot"
    );
    ensure!(
        snapshot.tables.len() == TABLES.len(),
        "Invalid saved-state table inventory"
    );
    for (actual, (name, spec)) in snapshot.tables.iter().zip(TABLES) {
        ensure!(
            &actual.name == name
                && actual
                    .columns
                    .iter()
                    .map(String::as_str)
                    .eq(columns(spec).into_iter().map(|(n, _)| n)),
            "Invalid saved-state columns"
        );
    }
    ensure!(
        snapshot.historical_inputs.len() as u64
            == snapshot.readiness.retained_unknown + snapshot.readiness.retained_not_accepted,
        "Invalid historical uncertainty inventory"
    );
    ensure!(
        snapshot
            .historical_inputs
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
            && snapshot
                .historical_inputs
                .iter()
                .all(|item| !item.id.is_empty()
                    && item.row_sha256.len() == 64
                    && item.row_sha256.bytes().all(|b| b.is_ascii_hexdigit())),
        "Invalid historical uncertainty identities"
    );
    ensure!(
        snapshot
            .secrets
            .first()
            .is_some_and(|entry| entry.path.is_empty() && entry.kind == SecretKind::Directory)
            && snapshot
                .secrets
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path),
        "Invalid protected secret inventory"
    );
    for entry in &snapshot.secrets {
        ensure!(
            entry.mode & 0o077 == 0
                && (entry.path.is_empty()
                    || entry.path.split('/').all(|part| !part.is_empty()
                        && part != "."
                        && part != ".."
                        && !part.contains('\0'))),
            "Invalid protected secret path or policy"
        );
        match entry.kind {
            SecretKind::Directory => ensure!(
                entry.sha256.is_none() && entry.bytes == 0,
                "Invalid protected directory snapshot"
            ),
            SecretKind::File => ensure!(
                entry.sha256.as_ref().is_some_and(
                    |hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                ),
                "Invalid protected file hash"
            ),
        }
    }
    Ok(())
}

fn historical_inputs(db: &Connection) -> Result<Vec<HistoricalInput>> {
    let spec = TABLES
        .iter()
        .find(|(name, _)| *name == "input_deliveries")
        .expect("fixed delivery table")
        .1;
    let names = columns(spec);
    let projection = names
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    let mut query = db.prepare(&format!("SELECT {projection} FROM input_deliveries WHERE state IN ('unknown','notAccepted') ORDER BY id"))?;
    let mut rows = query.query([])?;
    let mut saved = Vec::new();
    while let Some(row) = rows.next()? {
        let digest = typed_row_digest(row, names.len(), None)?;
        saved.push(HistoricalInput {
            id: row.get(1)?,
            row_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
        });
    }
    ensure!(
        saved.windows(2).all(|pair| pair[0].id < pair[1].id),
        "Ambiguous historical uncertainty identities"
    );
    Ok(saved)
}

/// Return only unchanged legacy uncertainty IDs inside the caller's existing
/// admission transaction. The caller must first validate a prepared/committed
/// adoption origin for this exact installation/home, then supply its bound
/// saved-state digest. A matching string alone is not origin authorization.
///
/// Use these IDs only in the existing unknown/notAccepted busy predicate; do
/// not subtract their count from general busy work. New/changed uncertainty,
/// added source/steering metadata and peer bindings get no exception. Nothing
/// here changes a row, proves completion, or authorizes replay/new effects.
pub(crate) fn matching_historical_inputs(
    db: &Connection,
    before: &Snapshot,
    origin_saved_state_sha256: &str,
) -> Result<BTreeSet<String>> {
    ensure!(
        !db.is_autocommit(),
        "Historical uncertainty inspection requires the admission transaction"
    );
    validate_snapshot(before)?;
    ensure!(
        before.readiness.ready && before.saved_state_sha256 == origin_saved_state_sha256,
        "Historical uncertainty is not bound to the validated adoption origin"
    );
    let expected_spec = TABLES
        .iter()
        .find(|(name, _)| *name == "input_deliveries")
        .expect("fixed delivery table")
        .1;
    let mut expected: Vec<_> = columns(expected_spec)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let actual = db
        .prepare("PRAGMA table_info(input_deliveries)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let successor_columns = actual.len() > expected.len();
    if successor_columns {
        expected.extend([
            "ordinary_not_before",
            "steering_target_turn",
            "steering_session_id",
            "source_metadata",
            "source_binding",
            "source_receipt",
        ]);
    }
    ensure!(
        actual.iter().map(String::as_str).eq(expected),
        "Unsupported historical delivery schema"
    );
    let has_peers: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='huddle_peer_deliveries')", [], |r| r.get(0))?;
    let original: BTreeMap<_, _> = before
        .historical_inputs
        .iter()
        .map(|item| (item.id.as_str(), item.row_sha256.as_str()))
        .collect();
    let mut matching = BTreeSet::new();
    for current in historical_inputs(db)? {
        if original.get(current.id.as_str()).copied() != Some(current.row_sha256.as_str()) {
            continue;
        }
        if successor_columns && !db.query_row("SELECT typeof(ordinary_not_before)='integer' AND ordinary_not_before=0 AND steering_target_turn IS NULL AND steering_session_id IS NULL AND source_metadata IS NULL AND source_binding IS NULL AND source_receipt IS NULL FROM input_deliveries WHERE id=?1", [&current.id], |r| r.get::<_,bool>(0))? { continue; }
        if has_peers
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM huddle_peer_deliveries WHERE input_id=?1)",
                [&current.id],
                |r| r.get::<_, bool>(0),
            )?
        {
            continue;
        }
        matching.insert(current.id);
    }
    Ok(matching)
}

fn table_snapshots(
    db: &Connection,
    refresh: Option<&StartupRefresh>,
) -> Result<Vec<TableSnapshot>> {
    if let Some(original) = refresh {
        let current = startup_refresh(db)?;
        ensure!(
            current.local_control_expiry >= original.local_control_expiry
                && current.context_provider_expiry >= original.context_provider_expiry
                && current.default_endpoint_updated_at >= original.default_endpoint_updated_at,
            "Startup refresh moved original expiry or time backwards"
        );
    }
    TABLES
        .iter()
        .map(|(name, spec)| {
            let names: Vec<_> = columns(spec)
                .into_iter()
                .map(|(name, _)| name.to_owned())
                .collect();
            let projection = names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(",");
            let (rows, sha256) = query_digest(
                db,
                &format!("SELECT {projection} FROM \"{name}\""),
                refresh.map(|r| (*name, r)),
            )?;
            let original_schema_sha256 = table_schema_digest(db, name, names.len())?;
            Ok(TableSnapshot {
                name: (*name).to_owned(),
                columns: names,
                original_schema_sha256,
                rows,
                sha256,
            })
        })
        .collect()
}

fn table_schema_digest(db: &Connection, name: &str, original_columns: usize) -> Result<String> {
    // All identifiers originate in TABLES. Preserve original nullability,
    // defaults, keys, foreign keys and indexes as well as row contents. Only
    // the six explicit appended delivery columns lie outside this projection.
    let columns = query_digest(
        db,
        &format!(
            "SELECT cid,name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo('{name}') WHERE cid < {original_columns}"
        ),
        None,
    )?;
    let foreign_keys = query_digest(
        db,
        &format!("SELECT * FROM pragma_foreign_key_list('{name}')"),
        None,
    )?;
    let indexes = query_digest(
        db,
        &format!("SELECT name,sql FROM sqlite_schema WHERE type='index' AND tbl_name='{name}'"),
        None,
    )?;
    let mut declaration: String = db.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
        [name],
        |row| row.get(0),
    )?;
    if name == "input_deliveries"
        && count(
            db,
            "SELECT COUNT(*) FROM pragma_table_xinfo('input_deliveries')",
        )? > original_columns as u64
    {
        // SQLite inserts these exact six declarations before the original
        // trailing table constraint. Remove only that single known fragment;
        // every byte of the original CHECK/UNIQUE declaration still hashes.
        let added = ", ordinary_not_before INTEGER NOT NULL DEFAULT 0, steering_target_turn TEXT, steering_session_id TEXT, source_metadata TEXT, source_binding TEXT, source_receipt TEXT";
        ensure!(
            declaration.matches(added).count() == 1,
            "Unexpected successor delivery schema declaration"
        );
        declaration = declaration.replacen(added, "", 1);
    }
    Ok(hash(&serde_json::to_vec(&(
        columns,
        foreign_keys,
        indexes,
        declaration,
    ))?))
}

// Typed, length-framed row hashing preserves NULL vs empty, integer vs text,
// blob bytes and duplicate rows. Sorting row digests removes physical SQLite
// order without dropping multiplicity. No private row value escapes this scan.
fn query_digest(
    db: &Connection,
    sql: &str,
    refresh: Option<(&str, &StartupRefresh)>,
) -> Result<(u64, String)> {
    let mut q = db.prepare(sql)?;
    let column_count = q.column_count();
    let mut rows = q.query([])?;
    let mut digests = Vec::new();
    while let Some(row) = rows.next()? {
        digests.push(typed_row_digest(row, column_count, refresh)?);
    }
    digests.sort_unstable();
    let count = digests.len() as u64;
    let mut digest = Sha256::new();
    digest.update(count.to_le_bytes());
    for row in digests {
        digest.update(row);
    }
    Ok((count, format!("{:x}", digest.finalize())))
}

fn typed_row_digest(
    row: &rusqlite::Row<'_>,
    column_count: usize,
    refresh: Option<(&str, &StartupRefresh)>,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    digest.update((column_count as u64).to_le_bytes());
    for index in 0..column_count {
        let value = match refresh {
            Some(("tokens", r)) if index == 3 => match row.get_ref(1)? {
                ValueRef::Text(b"local-control") => ValueRef::Integer(r.local_control_expiry),
                ValueRef::Text(b"context-provider") => ValueRef::Integer(r.context_provider_expiry),
                _ => row.get_ref(index)?,
            },
            Some(("agent_endpoints", r))
                if index == 2 && row.get_ref(0)? == ValueRef::Text(b"default") =>
            {
                ValueRef::Integer(r.default_endpoint_updated_at)
            }
            _ => row.get_ref(index)?,
        };
        match value {
            ValueRef::Null => digest.update([0]),
            ValueRef::Integer(v) => {
                digest.update([1]);
                digest.update(v.to_le_bytes());
            }
            ValueRef::Real(v) => {
                digest.update([2]);
                digest.update(v.to_bits().to_le_bytes());
            }
            ValueRef::Text(v) => {
                digest.update([3]);
                digest.update((v.len() as u64).to_le_bytes());
                digest.update(v);
            }
            ValueRef::Blob(v) => {
                digest.update([4]);
                digest.update((v.len() as u64).to_le_bytes());
                digest.update(v);
            }
        }
    }
    Ok(<[u8; 32]>::from(digest.finalize()))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    // Synthetic legacy data only. Construct the old final schema directly;
    // never call AppState::open or delivery::migrate in the raw-audit tests.
    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let agent = json!({"label":"Fixture agent","command":"never-executed-fixture",
            "args":[],"cwd":home.path().to_str().unwrap(),"env":{},"mcp_servers":[],"timeout_seconds":60});
        let config = json!({"id":uuid::Uuid::new_v4().to_string(),"label":"Legacy fixture",
            "owner":"fixture-owner","allowed_users":[],"embed_origins":[],"bind":"127.0.0.1",
            "port":8443,"public_url":"https://fixture.invalid","pam_service":"login",
            "tls_cert":"fixture-cert","tls_key":"fixture-key","import_source":"fixture","import_surface":"fixture",
            "agent":agent});
        fs::write(
            home.path().join("config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE conversations(id TEXT PRIMARY KEY,title TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,status TEXT NOT NULL,note TEXT NOT NULL,session_id TEXT,endpoint TEXT,agent_info TEXT,surface TEXT NOT NULL DEFAULT 'web',read_only INTEGER NOT NULL DEFAULT 0,owner TEXT NOT NULL DEFAULT '',shared INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE messages(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,role TEXT NOT NULL,text TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,actor TEXT NOT NULL DEFAULT '',agent_id TEXT,targets TEXT NOT NULL DEFAULT '[]');
            CREATE INDEX messages_conversation ON messages(conversation_id,seq);
            CREATE TABLE tokens(hash TEXT PRIMARY KEY,id TEXT NOT NULL,scopes TEXT NOT NULL,expires_at INTEGER NOT NULL);
            CREATE TABLE browser_sessions(hash TEXT PRIMARY KEY,owner TEXT NOT NULL,csrf TEXT NOT NULL,expires_at INTEGER NOT NULL);
            CREATE TABLE login_attempts(key TEXT PRIMARY KEY,failures INTEGER NOT NULL,locked_until INTEGER NOT NULL);
            CREATE TABLE grants(hash TEXT PRIMARY KEY,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,expires_at INTEGER NOT NULL,owner_scope INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE views(id TEXT PRIMARY KEY,value TEXT NOT NULL,revision INTEGER NOT NULL,updated_at INTEGER NOT NULL,owner TEXT NOT NULL DEFAULT '');
            CREATE TABLE audit(seq INTEGER PRIMARY KEY AUTOINCREMENT,at INTEGER NOT NULL,kind TEXT NOT NULL,conversation_id TEXT,data TEXT NOT NULL);
            CREATE TABLE imports(source TEXT NOT NULL,session_id TEXT NOT NULL,conversation_id TEXT UNIQUE NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,PRIMARY KEY(source,session_id));
            CREATE TABLE import_sources(source TEXT PRIMARY KEY,surface TEXT NOT NULL,token_id TEXT UNIQUE NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
            CREATE TABLE migrations(name TEXT PRIMARY KEY);
            CREATE TABLE room_members(conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,username TEXT NOT NULL,role TEXT NOT NULL,joined_at INTEGER NOT NULL,PRIMARY KEY(conversation_id,username));
            CREATE TABLE agent_endpoints(id TEXT PRIMARY KEY,config TEXT NOT NULL,updated_at INTEGER NOT NULL);
            CREATE TABLE room_agents(conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,agent_id TEXT NOT NULL REFERENCES agent_endpoints(id),session_id TEXT,endpoint TEXT,last_seq INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(conversation_id,agent_id));
            CREATE TABLE attachments(id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,name TEXT NOT NULL,mime_type TEXT NOT NULL,data BLOB NOT NULL,bytes INTEGER NOT NULL,created_by TEXT NOT NULL,created_at INTEGER NOT NULL,message_id TEXT REFERENCES messages(id) ON DELETE CASCADE);
            CREATE TABLE input_authority(message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,browser_hash TEXT,token_hash TEXT);
            CREATE TABLE input_deliveries(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,agent_id TEXT NOT NULL,participation TEXT NOT NULL,state TEXT NOT NULL,mode TEXT,target_turn TEXT,session_id TEXT,endpoint TEXT,receipt TEXT,attention TEXT,updated_at INTEGER NOT NULL,UNIQUE(message_id,agent_id));
            CREATE INDEX input_delivery_queue ON input_deliveries(conversation_id,sequence);
            INSERT INTO conversations VALUES('room','Original title',1,2,'idle','original note','original-session','original-endpoint',NULL,'web',0,'fixture-owner',0);
            INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES('user','room','user','Original instruction','complete',1,2,'fixture-owner');
            INSERT INTO room_members VALUES('room','fixture-owner','owner',1);
            INSERT INTO tokens VALUES('control-hash','local-control','[\"admin\"]',1000),('context-hash','context-provider','[\"context\"]',1000),('import-hash','source-importer','[\"import\"]',1000);
            INSERT INTO browser_sessions VALUES('browser-hash','fixture-owner','csrf-value',1000);
            INSERT INTO grants VALUES('grant-hash','room',1000,1);
            INSERT INTO login_attempts VALUES('login-key',1,5);
            INSERT INTO views VALUES('view','{\"value\":\"original\"}',2,3,'fixture-owner');
            INSERT INTO audit(at,kind,conversation_id,data) VALUES(1,'historical','room','{\"outcome\":null}');
            INSERT INTO imports VALUES('fixture','import-session','room');
            INSERT INTO import_sources VALUES('fixture','fixture','source-importer',1,1,2);
            INSERT INTO migrations VALUES('shared-rooms'),('default-agent-import-sources'),('named-import-sources');
            INSERT INTO input_authority VALUES('user','browser-hash',NULL);
            INSERT INTO attachments VALUES('attachment','room','original.bin','application/octet-stream',X'0001ff',3,'fixture-owner',1,'user');").unwrap();
        let agent: crate::config::Agent = serde_json::from_value(agent).unwrap();
        db.execute(
            "INSERT INTO agent_endpoints VALUES('default',?1,2)",
            [serde_json::to_string(&agent).unwrap()],
        )
        .unwrap();
        db.execute("INSERT INTO room_agents VALUES('room','default','original-session','original-endpoint',1)", []).unwrap();
        db.execute(
            "UPDATE tokens SET expires_at=?1 WHERE id IN ('local-control','context-provider')",
            [STARTUP_TOKEN_LIFETIME_MS + 2],
        )
        .unwrap();
        crate::config::private_dir(&home.path().join("secrets")).unwrap();
        for (file, id) in [
            ("control-token", "local-control"),
            ("context-token", "context-provider"),
            ("import-token", "source-importer"),
        ] {
            let token = format!("  synthetic-{id}-{}\n", "x".repeat(64));
            crate::config::private_write(&home.path().join("secrets").join(file), token.as_bytes())
                .unwrap();
            db.execute(
                "UPDATE tokens SET hash=?1 WHERE id=?2",
                params![hash(token.trim().as_bytes()), id],
            )
            .unwrap();
        }
        home
    }

    fn test_window() -> InitializationWindow {
        InitializationWindow {
            min_utc_ms: 2,
            max_utc_ms: 3,
        }
    }

    fn change(home: &Path, sql: &str) {
        Connection::open(home.join("state.sqlite3"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    fn add_delivery(home: &Path, id: &str, state: &str) {
        let db = Connection::open(home.join("state.sqlite3")).unwrap();
        db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor) VALUES(?1,'room','user','Do not replay','complete',1,2,'fixture-owner')", [id]).unwrap();
        db.execute("INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,target_turn,session_id,endpoint,receipt,attention,updated_at) VALUES(?1,?1,'room','default','direct',?2,'prompt',NULL,'saved-session','saved-endpoint','{\"submitted\":null}','{\"unknown\":true}',2)", params![id,state]).unwrap();
    }

    #[test]
    fn historical_unknown_and_not_accepted_are_retained_without_becoming_completed() {
        let home = fixture();
        add_delivery(home.path(), "unknown-input", "unknown");
        add_delivery(home.path(), "not-accepted-input", "notAccepted");
        change(home.path(), "UPDATE conversations SET status='error'");
        let database_before = fs::read(home.path().join("state.sqlite3")).unwrap();
        let before;
        {
            let guard = AuditGuard::begin(home.path()).unwrap();
            assert!(guard.report().ready);
            assert_eq!(guard.report().retained_unknown, 1);
            assert_eq!(guard.report().retained_not_accepted, 1);
            guard.verify_current().unwrap();
            before = guard.snapshot().clone();
            let public = serde_json::to_string(&before).unwrap();
            for private in [
                "Do not replay",
                "grant-hash",
                "csrf-value",
                "saved-session",
                "saved-endpoint",
            ] {
                assert!(!public.contains(private));
            }
        }
        assert_eq!(
            database_before,
            fs::read(home.path().join("state.sqlite3")).unwrap()
        );
        AuditGuard::begin(home.path())
            .unwrap()
            .verify_matches(&before)
            .unwrap();
    }

    #[test]
    fn queued_and_inflight_delivery_states_refuse_even_when_an_unknown_blocks_the_queue() {
        for state in [
            "received",
            "promptRequired",
            "preparing",
            "offering",
            "submitted",
            "received_remote",
            "remoteQueued",
            "ack_pending",
        ] {
            let home = fixture();
            add_delivery(home.path(), "unknown-input", "unknown");
            add_delivery(home.path(), "active-input", state);
            let guard = AuditGuard::begin(home.path()).unwrap();
            assert!(!guard.report().ready, "{state}");
            assert_eq!(guard.report().active_deliveries, 1, "{state}");
            assert_eq!(guard.report().retained_unknown, 1);
        }
    }

    #[test]
    fn working_or_streaming_is_refused_without_startup_normalization() {
        for sql in [
            "UPDATE conversations SET status='working'",
            "UPDATE messages SET status='streaming'",
        ] {
            let home = fixture();
            change(home.path(), sql);
            let guard = AuditGuard::begin(home.path()).unwrap();
            assert!(!guard.report().ready);
            guard.verify_current().unwrap();
            assert_eq!(
                guard.report().working_conversations + guard.report().streaming_messages,
                1
            );
        }
    }

    #[test]
    fn incorporation_requires_completed_matching_assistant_room_and_agent() {
        let home = fixture();
        add_delivery(home.path(), "incorporated-input", "incorporated");
        change(
            home.path(),
            "INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('answer','room','assistant','Saved answer','complete',1,2,'Agent','default'); UPDATE input_deliveries SET target_turn='answer'",
        );
        assert!(AuditGuard::begin(home.path()).unwrap().report().ready);
        for sql in [
            "UPDATE messages SET agent_id='another-agent' WHERE id='answer'",
            "UPDATE messages SET agent_id='default',role='user' WHERE id='answer'",
            "UPDATE messages SET role='assistant',status='interrupted' WHERE id='answer'",
            "UPDATE input_deliveries SET target_turn=NULL",
        ] {
            change(home.path(), sql);
            let guard = AuditGuard::begin(home.path()).unwrap();
            assert!(!guard.report().ready);
            assert_eq!(guard.report().incomplete_incorporations, 1);
        }
    }

    #[test]
    fn unsupported_state_and_schema_are_not_treated_as_idle() {
        let home = fixture();
        add_delivery(home.path(), "future-input", "future-state");
        assert_eq!(
            AuditGuard::begin(home.path())
                .unwrap()
                .report()
                .unsupported_states,
            1
        );
        for sql in [
            "CREATE TABLE unexpected_work(value TEXT)",
            "CREATE TABLE companion_calls(id TEXT,state TEXT)",
            "ALTER TABLE input_deliveries ADD COLUMN unknown_field TEXT",
            "CREATE VIEW shadow AS SELECT * FROM conversations",
            "DELETE FROM migrations WHERE name='shared-rooms'",
        ] {
            let home = fixture();
            change(home.path(), sql);
            assert!(AuditGuard::begin(home.path()).is_err(), "{sql}");
        }
    }

    #[test]
    fn writer_reservation_lasts_until_guard_drop_and_never_allows_audit_writes() {
        let home = fixture();
        let guard = AuditGuard::begin(home.path()).unwrap();
        let other = Connection::open(home.path().join("state.sqlite3")).unwrap();
        other.busy_timeout(Duration::ZERO).unwrap();
        assert!(
            other
                .execute("UPDATE conversations SET title='changed'", [])
                .is_err()
        );
        assert!(
            guard
                .db
                .execute("UPDATE conversations SET title='changed'", [])
                .is_err()
        );
        guard.verify_current().unwrap();
        drop(guard);
        other
            .execute("UPDATE conversations SET title='changed'", [])
            .unwrap();
    }

    #[test]
    fn configuration_and_protected_rows_drift_is_detected() {
        let home = fixture();
        let guard = AuditGuard::begin(home.path()).unwrap();
        let before = guard.snapshot().clone();
        let config = fs::read(home.path().join("config.json")).unwrap();
        let mut changed = config.clone();
        changed.push(b' ');
        fs::write(home.path().join("config.json"), changed).unwrap();
        assert!(guard.verify_current().is_err());
        fs::write(home.path().join("config.json"), config).unwrap();
        guard.verify_current().unwrap();
        drop(guard);
        change(
            home.path(),
            "UPDATE room_agents SET session_id='replacement'",
        );
        assert!(
            AuditGuard::begin(home.path())
                .unwrap()
                .verify_matches(&before)
                .is_err()
        );
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
    }

    #[test]
    fn post_migration_allows_only_explicit_expiry_and_default_timestamp_refresh() {
        let home = fixture();
        add_delivery(home.path(), "uncertain", "unknown");
        let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
        change(
            home.path(),
            "UPDATE agent_endpoints SET updated_at=3 WHERE id='default';
            ALTER TABLE input_deliveries ADD COLUMN ordinary_not_before INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE input_deliveries ADD COLUMN steering_target_turn TEXT;
            ALTER TABLE input_deliveries ADD COLUMN steering_session_id TEXT;
            ALTER TABLE input_deliveries ADD COLUMN source_metadata TEXT;
            ALTER TABLE input_deliveries ADD COLUMN source_binding TEXT;
            ALTER TABLE input_deliveries ADD COLUMN source_receipt TEXT;
            CREATE TABLE companion_calls(id TEXT,state TEXT);
            CREATE TABLE huddle_peer_deliveries(input_id TEXT,message_id TEXT,binding TEXT);",
        );
        verify_preserved(home.path(), &before, test_window(), None).unwrap();
        change(
            home.path(),
            "UPDATE input_deliveries SET source_receipt='{}'",
        );
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
        change(
            home.path(),
            "UPDATE input_deliveries SET source_receipt=NULL; UPDATE input_deliveries SET receipt='{}'",
        );
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
    }

    #[test]
    fn migration_cannot_clear_uncertainty_change_grants_or_start_successor_work() {
        for sql in [
            "UPDATE input_deliveries SET state='completed'",
            "DELETE FROM grants",
            "UPDATE tokens SET scopes='[\"admin\",\"context\"]' WHERE id='context-provider'",
            "UPDATE tokens SET expires_at=999 WHERE id='local-control'",
            "UPDATE tokens SET expires_at=2000 WHERE id='source-importer'",
            "UPDATE room_agents SET session_id=NULL",
            "UPDATE sqlite_sequence SET seq=10000 WHERE name='messages'",
            "CREATE TABLE companion_calls(id TEXT,state TEXT); INSERT INTO companion_calls VALUES('new','starting')",
        ] {
            let home = fixture();
            add_delivery(home.path(), "uncertain", "unknown");
            let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
            change(home.path(), sql);
            assert!(
                verify_preserved(home.path(), &before, test_window(), None).is_err(),
                "{sql}"
            );
        }
    }

    #[test]
    fn typed_hash_preserves_null_empty_blob_integer_and_duplicate_rows() {
        let db = Connection::open_in_memory().unwrap();
        db.execute("CREATE TABLE values_fixture(value)", [])
            .unwrap();
        let mut hashes = BTreeSet::new();
        for value in [
            rusqlite::types::Value::Null,
            String::new().into(),
            vec![].into(),
            1_i64.into(),
            String::from("1").into(),
        ] {
            db.execute("DELETE FROM values_fixture", []).unwrap();
            db.execute("INSERT INTO values_fixture VALUES(?1)", [&value])
                .unwrap();
            assert!(
                hashes.insert(
                    query_digest(&db, "SELECT value FROM values_fixture", None)
                        .unwrap()
                        .1
                )
            );
        }
        let before = query_digest(&db, "SELECT value FROM values_fixture", None).unwrap();
        db.execute(
            "INSERT INTO values_fixture SELECT value FROM values_fixture",
            [],
        )
        .unwrap();
        let after = query_digest(&db, "SELECT value FROM values_fixture", None).unwrap();
        assert_eq!(before.0, 1);
        assert_eq!(after.0, 2);
        assert_ne!(before.1, after.1);
    }

    #[test]
    fn initialization_window_refuses_unbounded_timestamp_and_expiry_advance() {
        for field in ["local-control", "context-provider", "default-endpoint"] {
            let home = fixture();
            let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
            verify_preserved(home.path(), &before, test_window(), None).unwrap();
            let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
            if field == "default-endpoint" {
                db.execute(
                    "UPDATE agent_endpoints SET updated_at=4 WHERE id='default'",
                    [],
                )
                .unwrap();
            } else {
                db.execute(
                    "UPDATE tokens SET expires_at=?1 WHERE id=?2",
                    params![STARTUP_TOKEN_LIFETIME_MS + 4, field],
                )
                .unwrap();
            }
            drop(db);
            assert!(
                verify_preserved(home.path(), &before, test_window(), None).is_err(),
                "{field}"
            );
        }
        let home = fixture();
        let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
        for window in [
            InitializationWindow {
                min_utc_ms: -1,
                max_utc_ms: 3,
            },
            InitializationWindow {
                min_utc_ms: 3,
                max_utc_ms: 2,
            },
            InitializationWindow {
                min_utc_ms: 2,
                max_utc_ms: i64::MAX,
            },
        ] {
            assert!(verify_preserved(home.path(), &before, window, None).is_err());
        }
    }

    #[test]
    fn prepare_gate_commits_barrier_before_writer_reservation_is_released() {
        let home = fixture();
        add_delivery(home.path(), "historical", "unknown");
        let guard = AuditGuard::begin(home.path()).unwrap();
        let before = guard.snapshot().clone();
        let other = Connection::open(home.path().join("state.sqlite3")).unwrap();
        other.busy_timeout(Duration::ZERO).unwrap();
        assert!(other.execute_batch("BEGIN IMMEDIATE").is_err());
        let transaction = uuid::Uuid::new_v4().to_string();
        let returned = guard.prepare_gate(&transaction).unwrap();
        assert_eq!(before, returned);
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let actual: String = other
            .query_row(
                "SELECT transaction_id FROM native_update_admission WHERE phase='prepared'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(actual, transaction);
        assert_eq!(
            matching_historical_inputs(&other, &before, &before.saved_state_sha256).unwrap(),
            BTreeSet::from(["historical".to_owned()])
        );
        other.execute_batch("ROLLBACK").unwrap();
        let expected = ExpectedGate {
            transaction: transaction.clone(),
            phase: GatePhase::Prepared,
        };
        verify_preserved(home.path(), &before, test_window(), Some(&expected)).unwrap();
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
        let wrong = ExpectedGate {
            transaction: uuid::Uuid::new_v4().to_string(),
            phase: GatePhase::Prepared,
        };
        assert!(verify_preserved(home.path(), &before, test_window(), Some(&wrong)).is_err());
        let restored = ExpectedGate {
            transaction,
            phase: GatePhase::Restored,
        };
        assert!(verify_preserved(home.path(), &before, test_window(), Some(&restored)).is_err());
        other
            .execute(
                "UPDATE native_update_admission SET phase='restored',restored_at=acquired_at+1",
                [],
            )
            .unwrap();
        verify_preserved(home.path(), &before, test_window(), Some(&restored)).unwrap();
        assert!(verify_preserved(home.path(), &before, test_window(), Some(&expected)).is_err());
        other
            .execute(
                "INSERT INTO native_update_admission VALUES(?1,'restored',0,1)",
                [uuid::Uuid::new_v4().to_string()],
            )
            .unwrap();
        assert!(verify_preserved(home.path(), &before, test_window(), Some(&restored)).is_err());
    }

    #[test]
    fn prepare_gate_refuses_active_work_or_drift_before_creating_admission_schema() {
        for drift in [false, true] {
            let home = fixture();
            if !drift {
                add_delivery(home.path(), "queued", "received");
            }
            let guard = AuditGuard::begin(home.path()).unwrap();
            if drift {
                let mut bytes = fs::read(home.path().join("config.json")).unwrap();
                bytes.push(b' ');
                fs::write(home.path().join("config.json"), bytes).unwrap();
            }
            assert!(
                guard
                    .prepare_gate(&uuid::Uuid::new_v4().to_string())
                    .is_err()
            );
            let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
            assert_eq!(count(&db, "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('native_update_admission','native_update_activity')").unwrap(), 0);
            if !drift {
                assert_eq!(
                    count(
                        &db,
                        "SELECT COUNT(*) FROM input_deliveries WHERE state='received'"
                    )
                    .unwrap(),
                    1
                );
            }
        }
    }

    #[test]
    fn historical_admission_requires_origin_digest_transaction_and_identical_rows() {
        let home = fixture();
        add_delivery(home.path(), "same", "unknown");
        add_delivery(home.path(), "changed", "notAccepted");
        let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
        add_delivery(home.path(), "new-unknown", "unknown");
        change(
            home.path(),
            "UPDATE input_deliveries SET receipt='{}' WHERE id='changed'",
        );
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        assert!(matching_historical_inputs(&db, &before, &before.saved_state_sha256).is_err());
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(matching_historical_inputs(&db, &before, "wrong-origin").is_err());
        let changes = db.total_changes();
        assert_eq!(
            matching_historical_inputs(&db, &before, &before.saved_state_sha256).unwrap(),
            BTreeSet::from(["same".to_owned()])
        );
        assert_eq!(db.total_changes(), changes);
        assert_eq!(readiness(&db).unwrap().retained_unknown, 2);
        assert_eq!(readiness(&db).unwrap().retained_not_accepted, 1);
        db.execute_batch("ROLLBACK").unwrap();
        change(
            home.path(),
            "UPDATE input_deliveries SET state='notAccepted' WHERE id='same'",
        );
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            matching_historical_inputs(&db, &before, &before.saved_state_sha256)
                .unwrap()
                .is_empty()
        );
        db.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn historical_admission_never_covers_new_metadata_or_peer_delivery() {
        let home = fixture();
        add_delivery(home.path(), "historical", "unknown");
        let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
        change(home.path(), "ALTER TABLE input_deliveries ADD COLUMN ordinary_not_before INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE input_deliveries ADD COLUMN steering_target_turn TEXT;
            ALTER TABLE input_deliveries ADD COLUMN steering_session_id TEXT;
            ALTER TABLE input_deliveries ADD COLUMN source_metadata TEXT;
            ALTER TABLE input_deliveries ADD COLUMN source_binding TEXT;
            ALTER TABLE input_deliveries ADD COLUMN source_receipt TEXT;
            CREATE TABLE huddle_peer_deliveries(input_id TEXT PRIMARY KEY,message_id TEXT,binding TEXT);");
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(
            matching_historical_inputs(&db, &before, &before.saved_state_sha256)
                .unwrap()
                .len(),
            1
        );
        db.execute_batch("ROLLBACK").unwrap();
        for column in [
            "steering_target_turn",
            "steering_session_id",
            "source_metadata",
            "source_binding",
            "source_receipt",
        ] {
            db.execute(&format!("UPDATE input_deliveries SET {column}='new'"), [])
                .unwrap();
            db.execute_batch("BEGIN IMMEDIATE").unwrap();
            assert!(
                matching_historical_inputs(&db, &before, &before.saved_state_sha256)
                    .unwrap()
                    .is_empty(),
                "{column}"
            );
            db.execute_batch("ROLLBACK").unwrap();
            db.execute(&format!("UPDATE input_deliveries SET {column}=NULL"), [])
                .unwrap();
        }
        db.execute("UPDATE input_deliveries SET ordinary_not_before=1", [])
            .unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            matching_historical_inputs(&db, &before, &before.saved_state_sha256)
                .unwrap()
                .is_empty()
        );
        db.execute_batch("ROLLBACK").unwrap();
        db.execute("UPDATE input_deliveries SET ordinary_not_before=0", [])
            .unwrap();
        db.execute(
            "INSERT INTO huddle_peer_deliveries VALUES('historical','historical','new')",
            [],
        )
        .unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            matching_historical_inputs(&db, &before, &before.saved_state_sha256)
                .unwrap()
                .is_empty()
        );
        db.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn required_secret_missing_or_unbound_refuses_without_generation() {
        for filename in ["control-token", "context-token", "import-token"] {
            let home = fixture();
            let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
            let path = home.path().join("secrets").join(filename);
            fs::remove_file(&path).unwrap();
            assert!(AuditGuard::begin(home.path()).is_err(), "{filename}");
            assert!(!path.exists());
            let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
            assert_eq!(table_snapshots(&db, None).unwrap(), before.tables);
            assert_eq!(
                count(
                    &db,
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name='native_update_admission'"
                )
                .unwrap(),
                0
            );
            crate::config::private_write(
                &path,
                b"different-valid-length-token-not-in-original-database",
            )
            .unwrap();
            assert!(AuditGuard::begin(home.path()).is_err(), "{filename}");
            assert_eq!(
                fs::read(&path).unwrap(),
                b"different-valid-length-token-not-in-original-database"
            );
        }
    }

    #[test]
    fn before_startup_allows_only_exact_original_and_bound_gate() {
        let home = fixture();
        let guard = AuditGuard::begin(home.path()).unwrap();
        let before = guard.snapshot().clone();
        drop(guard);
        verify_before_startup(home.path(), &before, None).unwrap();
        let transaction = uuid::Uuid::new_v4().to_string();
        AuditGuard::begin(home.path())
            .unwrap()
            .prepare_gate(&transaction)
            .unwrap();
        let expected = ExpectedGate {
            transaction,
            phase: GatePhase::Prepared,
        };
        verify_before_startup(home.path(), &before, Some(&expected)).unwrap();
        assert!(verify_before_startup(home.path(), &before, None).is_err());
        change(
            home.path(),
            "UPDATE tokens SET expires_at=expires_at+1 WHERE id='local-control'",
        );
        assert!(verify_before_startup(home.path(), &before, Some(&expected)).is_err());
    }

    #[test]
    fn before_startup_refuses_partial_successor_schema_and_changed_original() {
        for sql in [
            "CREATE TABLE companion_calls(id TEXT,state TEXT)",
            "ALTER TABLE input_deliveries ADD COLUMN ordinary_not_before INTEGER NOT NULL DEFAULT 0",
            "UPDATE conversations SET note='changed'",
            "CREATE INDEX added_original_index ON conversations(title)",
        ] {
            let home = fixture();
            let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
            change(home.path(), sql);
            assert!(
                verify_before_startup(home.path(), &before, None).is_err(),
                "{sql}"
            );
        }
    }

    #[test]
    fn secret_regeneration_cannot_pass_even_if_database_hash_is_rewritten_to_match() {
        let home = fixture();
        let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
        let replacement = b"synthetic-regenerated-control-token-long-enough-for-old-loader";
        crate::config::private_write(&home.path().join("secrets/control-token"), replacement)
            .unwrap();
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        db.execute(
            "UPDATE tokens SET hash=?1 WHERE id='local-control'",
            [hash(replacement)],
        )
        .unwrap();
        drop(db);
        assert!(
            AuditGuard::begin(home.path())
                .unwrap()
                .verify_matches(&before)
                .is_err()
        );
        assert!(
            verify_preserved(home.path(), &before, test_window(), None)
                .unwrap_err()
                .to_string()
                .contains("Original secret files")
        );
    }

    #[test]
    fn extra_safe_secret_tree_is_preserved_and_not_exposed() {
        let home = fixture();
        let folder = home.path().join("secrets/other-provider");
        crate::config::private_dir(&folder).unwrap();
        crate::config::private_dir(&folder.join("empty")).unwrap();
        let content = b"private-extra-provider-credential-not-a-public-receipt";
        crate::config::private_write(&folder.join("credential"), content).unwrap();
        let guard = AuditGuard::begin(home.path()).unwrap();
        let before = guard.snapshot().clone();
        assert!(before.secrets.iter().any(
            |entry| entry.path == "other-provider/empty" && entry.kind == SecretKind::Directory
        ));
        assert!(
            before
                .secrets
                .iter()
                .any(|entry| entry.path == "other-provider/credential"
                    && entry.sha256.as_deref() == Some(hash(content).as_str()))
        );
        assert!(
            !serde_json::to_string(&before)
                .unwrap()
                .contains(std::str::from_utf8(content).unwrap())
        );
        guard.verify_current().unwrap();
        drop(guard);
        verify_preserved(home.path(), &before, test_window(), None).unwrap();
        fs::remove_dir(folder.join("empty")).unwrap();
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
        crate::config::private_dir(&folder.join("empty")).unwrap();
        crate::config::private_write(&folder.join("credential"), b"changed").unwrap();
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
    }

    #[test]
    fn unsafe_secret_policy_and_links_refuse_without_repair() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for mode in [0o644, 0o620] {
            let home = fixture();
            let path = home.path().join("secrets/control-token");
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(AuditGuard::begin(home.path()).is_err());
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
        let home = fixture();
        let path = home.path().join("secrets/context-token");
        let original = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        symlink("control-token", &path).unwrap();
        assert!(AuditGuard::begin(home.path()).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&path).unwrap();
        crate::config::private_write(&path, &original).unwrap();
        fs::set_permissions(
            home.path().join("secrets"),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        assert!(AuditGuard::begin(home.path()).is_err());
        assert_eq!(
            fs::metadata(home.path().join("secrets"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }

    #[test]
    fn named_import_secret_binds_existing_authority_and_revoked_absence_is_not_recreated() {
        let home = fixture();
        let folder = home.path().join("secrets/import-sources");
        crate::config::private_dir(&folder).unwrap();
        let token = b"synthetic-original-other-import-source-token-sixty-four-characters";
        crate::config::private_write(&folder.join("other-token"), token).unwrap();
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO import_sources VALUES('other','other','source-importer:other',1,1,2)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO tokens VALUES(?1,'source-importer:other','[\"import\"]',1000)",
            [hash(token)],
        )
        .unwrap();
        drop(db);
        assert!(AuditGuard::begin(home.path()).unwrap().report().ready);
        fs::remove_file(folder.join("other-token")).unwrap();
        assert!(AuditGuard::begin(home.path()).is_err());
        change(
            home.path(),
            "UPDATE import_sources SET enabled=0 WHERE source='other'; DELETE FROM tokens WHERE id='source-importer:other'",
        );
        let before = AuditGuard::begin(home.path()).unwrap().snapshot().clone();
        assert!(!folder.join("other-token").exists());
        verify_preserved(home.path(), &before, test_window(), None).unwrap();
        assert!(!folder.join("other-token").exists());
        crate::config::private_write(
            &folder.join("unexpected-new-token"),
            b"unexpected-generated-new-secret",
        )
        .unwrap();
        assert!(verify_preserved(home.path(), &before, test_window(), None).is_err());
    }

    #[test]
    fn missing_database_is_not_created_and_incomplete_database_is_not_migrated() {
        let home = fixture();
        fs::remove_file(home.path().join("state.sqlite3")).unwrap();
        assert!(AuditGuard::begin(home.path()).is_err());
        assert!(!home.path().join("state.sqlite3").exists());
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        db.execute("CREATE TABLE original(value TEXT)", []).unwrap();
        drop(db);
        assert!(AuditGuard::begin(home.path()).is_err());
        let db = Connection::open(home.path().join("state.sqlite3")).unwrap();
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'").unwrap(),
            1
        );
    }
}
