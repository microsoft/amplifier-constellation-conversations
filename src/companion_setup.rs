//! Protected setup for saved contacts. Selection never replaces an endpoint or
//! clears its grants/session; no media, provider request or model turn is made.
use super::*;
use crate::{config, credentials, update_admission};
use fs2::FileExt;
use std::{fs, path::Path as FsPath};
use zeroize::Zeroizing;

fn setup_lock(home: &FsPath) -> Result<fs::File> {
    let path = home.join("companion-setup.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(file.metadata()?.is_file(), "Invalid setup lock");
    file.try_lock_exclusive()
        .context("Another setup operation is in progress")?;
    Ok(file)
}

pub async fn setup_status(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", false)?;
    let profiles = config(&s).ok().map(|c| c.profiles).unwrap_or_default();
    let db = s.db.lock().unwrap();
    let mut query = db.prepare("SELECT id,config FROM agent_endpoints ORDER BY id")?;
    let agents = query.query_map([], |row| {
        let id: String = row.get(0)?;
        let config: Value = serde_json::from_str(&row.get::<_,String>(1)?).unwrap_or_default();
        Ok(json!({"id":id,"label":config["label"],"voice_configured":profiles.contains_key(&id)}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Json(
        json!({"agents":agents,"credential":credentials::openai_status(&s.home)?,"voices":VOICES,"voice_model":VOICE_MODEL,"provider_verified":false,"native_bundle_installed":s.home.join("native-installation.json").is_file()}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupWrite {
    agent_id: String,
    voice: String,
    #[serde(default)]
    openai_key: Option<String>,
}

fn installed_voice_paths(home: &FsPath) -> Result<(PathBuf, PathBuf)> {
    let bytes = fs::read(home.join("native-installation.json"))?;
    ensure!(
        bytes.len() <= 128 * 1024,
        "Invalid installed bundle receipt"
    );
    let receipt: Value = serde_json::from_slice(&bytes)?;
    let root = receipt["package"]["bundle"]["bundle"]
        .as_str()
        .context("Missing installed bundle")?;
    let canonical = dunce::canonicalize(root)?;
    let packages = dunce::canonicalize(home.join("packages"))?;
    ensure!(
        canonical.parent() == Some(packages.as_path()),
        "Bundle is not installation-owned"
    );
    let verified = crate::native_bundle::verify(&canonical)?;
    ensure!(
        verified == receipt["package"]["bundle"],
        "Installed bundle receipt differs"
    );
    Ok((
        PathBuf::from(
            verified["entrypoints"]["node"]
                .as_str()
                .context("Missing private runtime")?,
        ),
        PathBuf::from(
            verified["entrypoints"]["bridge"]
                .as_str()
                .context("Missing voice bridge")?,
        ),
    ))
}

pub async fn save_setup(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(mut body): Json<SetupWrite>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    let key = body.openai_key.take().map(Zeroizing::new);
    if !VOICES.contains(&body.voice.as_str()) {
        return Err(ApiError::bad("Choose an available voice."));
    }
    s.agent(&body.agent_id)?
        .ok_or_else(|| ApiError::bad("Choose a saved agent connection."))?;
    let _activity = update_admission::begin_configuration(&s)?;
    let _lock = setup_lock(&s.home)?;
    let path = s.home.join("companion.json");
    let mut value: Value = match fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > 64 * 1024 {
                return Err(ApiError::bad("Companion configuration too large"));
            }
            // Refuse malformed configuration rather than silently overwriting it.
            config(&s)?;
            serde_json::from_slice(&bytes).map_err(anyhow::Error::from)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (node, bridge) = installed_voice_paths(&s.home)?;
            json!({"enabled":false,"node":node,"bridge":bridge,"profiles":{}})
        }
        Err(e) => return Err(e.into()),
    };
    let profiles = value["profiles"]
        .as_object_mut()
        .context("Invalid saved profiles")?;
    let added = !profiles.contains_key(&body.agent_id);
    if added {
        if profiles.len() >= 100 {
            return Err(ApiError::bad("At most 100 contacts can be configured"));
        }
        profiles.insert(
            body.agent_id.clone(),
            json!({"voice":body.voice,"context_registered":false}),
        );
    }
    // Existing identity, tools, source grants, limits and voice settings survive
    // selecting that contact again. Editing its voice uses the established UI.
    value["enabled"] = json!(true);
    let serialized = serde_json::to_vec_pretty(&value).map_err(anyhow::Error::from)?;
    if serialized.len() > 64 * 1024 {
        return Err(ApiError::bad("Companion configuration too large"));
    }
    if let Some(key) = key.as_ref() {
        credentials::import_openai(&s.home, key.as_bytes())?;
    }
    config::private_write(&path, &serialized)?;
    s.audit(
        "companion.contact_configured",
        None,
        json!({"agent_id":body.agent_id,"added":added}),
    )?;
    Ok(Json(
        json!({"saved":true,"agent_id":body.agent_id,"added":added,"credential":credentials::openai_status(&s.home)?,"provider_verified":false,"note":"Contact saved. No call or model prompt was started."}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalAgentWrite {
    id: String,
    label: String,
    home: PathBuf,
}

pub async fn connect_local_agent(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(body): Json<LocalAgentWrite>,
) -> ApiResult<Json<Value>> {
    auth::check(&s, &h, "admin", true)?;
    if body.id.is_empty()
        || body.id.len() > 80
        || !body
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        || body.id == "default"
    {
        return Err(ApiError::bad(
            "Choose a new connection name using letters, numbers or hyphens.",
        ));
    }
    if !body.home.is_absolute() {
        return Err(ApiError::bad(
            "Choose the full path to an installed agent folder.",
        ));
    }
    let home = dunce::canonicalize(&body.home)
        .map_err(|_| ApiError::bad("That agent folder could not be opened."))?;
    let binary = home
        .join("bin")
        .join(crate::platform::executable("agent-instance"));
    let metadata = fs::symlink_metadata(&binary).map_err(|_| {
        ApiError::bad("That folder does not contain an installed Instance launcher.")
    })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !home.join("config.json").is_file()
        || !home.join("workspace").is_dir()
    {
        return Err(ApiError::bad(
            "Choose an existing, complete Instance installation.",
        ));
    }
    let agent = config::Agent {
        label: body.label,
        command: binary.to_string_lossy().into_owned(),
        args: vec![
            "acp".into(),
            "--home".into(),
            home.to_string_lossy().into_owned(),
        ],
        cwd: home.join("workspace").to_string_lossy().into_owned(),
        session_cwd: None,
        env: BTreeMap::new(),
        mcp_servers: vec![],
        timeout_seconds: 300,
        client: Default::default(),
    };
    agent
        .validate()
        .map_err(|_| ApiError::bad("Enter a name for this agent."))?;
    let serialized = serde_json::to_string(&agent).map_err(anyhow::Error::from)?;
    if let Some(existing) = s.agent(&body.id)? {
        if serde_json::to_value(existing).map_err(anyhow::Error::from)?
            == serde_json::to_value(&agent).map_err(anyhow::Error::from)?
        {
            return Ok(Json(
                json!({"saved":true,"id":body.id,"already_saved":true,"connection_checked":false}),
            ));
        }
        return Err(ApiError::bad(
            "That connection name is already saved. Select it or choose another name.",
        ));
    }
    let _activity = update_admission::begin_probe(&s)?;
    let probe = crate::driver::probe(&agent).await.map_err(|_| {
        ApiError::bad("The agent did not complete its connection check. Nothing was added.")
    })?;
    let mut db = s.db.lock().unwrap();
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    update_admission::ensure_open(&tx)?;
    let count: i64 = tx.query_row("SELECT count(*) FROM agent_endpoints", [], |r| r.get(0))?;
    if count >= 100 {
        return Err(ApiError::bad("At most 100 connections can be configured."));
    }
    // No upsert: a concurrent existing endpoint cannot lose its grants or sessions.
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO agent_endpoints(id,config,updated_at) VALUES(?1,?2,?3)",
        params![body.id, serialized, now_ms()],
    )?;
    if inserted != 1 {
        return Err(ApiError::bad(
            "That connection name was saved by another request. Refresh the list.",
        ));
    }
    tx.commit()?;
    drop(db);
    s.changed();
    Ok(Json(
        json!({"saved":true,"id":body.id,"connection_checked":true,"check":probe}),
    ))
}
