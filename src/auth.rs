use crate::{
    config::{AuthMode, Config, Transport},
    error::{ApiError, ApiResult},
    state::{AppState, now_ms},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rand::RngCore;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(all(unix, not(target_os = "macos")))]
use std::ffi::{CStr, CString};
use std::{fs, io::Read, path::Path, sync::Arc};
use zeroize::Zeroizing;

const LOCAL_PASSWORD_FILE: &str = "local-password.json";
const FIRST_ACCOUNT_FILE: &str = "first-account.json";
const SETUP_LIFETIME_MS: i64 = 15 * 60 * 1000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalPassword {
    schema: u32,
    installation_id: String,
    owner: String,
    password_hash: String,
    created_at: i64,
    start_at_signin: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstAccountCapability {
    schema: u32,
    installation_id: String,
    token: String,
    expires_at: i64,
}

fn check_private_path(metadata: &fs::Metadata, directory: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !metadata.file_type().is_symlink()
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
        "Local account storage must be an ordinary file or directory"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                == 0,
            "Local account storage must not be a reparse point"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
            "Local account storage must be private to this OS user"
        );
    }
    Ok(())
}

// Fixed installation-owned names, bounded reads and no link following. A corrupt
// record is an error, never an invitation to replace an existing account.
fn read_private<T: DeserializeOwned>(home: &Path, name: &str) -> anyhow::Result<Option<T>> {
    let directory = home.join("secrets");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => check_private_path(&metadata, true)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => check_private_path(&metadata, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
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
    check_private_path(&metadata, false)?;
    anyhow::ensure!(
        metadata.len() <= 8192,
        "Local account record exceeds its limit"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(8193).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 8192,
        "Local account record exceeds its limit"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn local_account(home: &Path, config: &Config) -> anyhow::Result<Option<LocalPassword>> {
    let saved = read_private::<LocalPassword>(home, LOCAL_PASSWORD_FILE)?;
    if let Some(saved) = &saved {
        anyhow::ensure!(
            saved.schema == 1 && saved.installation_id == config.id && saved.owner == config.owner,
            "Local account does not belong to this installation"
        );
    }
    Ok(saved)
}

pub fn first_account_needed(home: &Path) -> anyhow::Result<bool> {
    let config = Config::load(home)?;
    Ok(config.auth_mode == AuthMode::LocalPassword && local_account(home, &config)?.is_none())
}

fn fixed_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The desktop launcher alone mints this capability. Reopening retains a live
/// capability and never replaces a password, including after a lost HTTP reply.
pub fn prepare_first_account(home: &Path) -> anyhow::Result<Option<String>> {
    let config = Config::load(home)?;
    if config.auth_mode != AuthMode::LocalPassword {
        return Ok(None);
    }
    let _owner = crate::native_update::startup_change_guard(home)?;
    let _activity = home
        .join("state.sqlite3")
        .exists()
        .then(|| crate::update_admission::begin_configuration_home(home))
        .transpose()?;
    if local_account(home, &config)?.is_some() {
        return Ok(None);
    }
    if let Some(saved) = read_private::<FirstAccountCapability>(home, FIRST_ACCOUNT_FILE)? {
        anyhow::ensure!(
            saved.schema == 1 && saved.installation_id == config.id && fixed_hex(&saved.token),
            "Invalid first-account capability"
        );
        if saved.expires_at > now_ms() && saved.expires_at <= now_ms() + SETUP_LIFETIME_MS {
            return Ok(Some(saved.token));
        }
        fs::remove_file(home.join("secrets").join(FIRST_ACCOUNT_FILE))?;
    }
    crate::config::private_dir(&home.join("secrets"))?;
    let saved = FirstAccountCapability {
        schema: 1,
        installation_id: config.id,
        token: random_token(),
        expires_at: now_ms() + SETUP_LIFETIME_MS,
    };
    amplifier_native_platform::private_create(
        &home.join("secrets").join(FIRST_ACCOUNT_FILE),
        &serde_json::to_vec(&saved)?,
    )?;
    Ok(Some(saved.token))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstAccount {
    setup_token: String,
    password: String,
    #[serde(default)]
    start_at_signin: bool,
}

fn create_local_account(home: &Path, input: FirstAccount) -> ApiResult<Value> {
    create_local_account_with_startup(home, input, |home, id| {
        amplifier_native_platform::install_startup(&crate::native_bundle::APPLICATION, home, id)
    })
}

fn create_local_account_with_startup(
    home: &Path,
    input: FirstAccount,
    install_startup: impl FnOnce(&Path, &uuid::Uuid) -> anyhow::Result<()>,
) -> ApiResult<Value> {
    let password = Zeroizing::new(input.password);
    let _owner = crate::native_update::startup_change_guard(home)?;
    let _activity = crate::update_admission::begin_configuration_home(home)?;
    let config = Config::load(home)?;
    if config.auth_mode != AuthMode::LocalPassword || local_account(home, &config)?.is_some() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This installation already has an account, or does not use local account setup".into(),
        ));
    }
    let setup = read_private::<FirstAccountCapability>(home, FIRST_ACCOUNT_FILE)?
        .ok_or_else(ApiError::forbidden)?;
    if setup.schema != 1
        || setup.installation_id != config.id
        || !fixed_hex(&input.setup_token)
        || !constant_equal(&setup.token, &input.setup_token)
        || setup.expires_at <= now_ms()
        || setup.expires_at > now_ms() + SETUP_LIFETIME_MS
    {
        return Err(ApiError::forbidden());
    }
    let password_hash =
        amplifier_native_platform::local_auth::hash_password(&password).map_err(|_| {
            ApiError::bad("Use a password of at least 12 characters and at most 1024 bytes")
        })?;
    if setup.expires_at <= now_ms() {
        return Err(ApiError::forbidden());
    }
    let saved = LocalPassword {
        schema: 1,
        installation_id: config.id.clone(),
        owner: config.owner.clone(),
        password_hash,
        created_at: now_ms(),
        start_at_signin: input.start_at_signin,
    };
    amplifier_native_platform::private_create(
        &home.join("secrets").join(LOCAL_PASSWORD_FILE),
        &serde_json::to_vec(&saved).map_err(anyhow::Error::from)?,
    )?;
    // Account existence makes the capability unusable even if its cleanup fails.
    let _ = fs::remove_file(home.join("secrets").join(FIRST_ACCOUNT_FILE));
    let startup = if input.start_at_signin {
        let result = uuid::Uuid::parse_str(&config.id)
            .map_err(anyhow::Error::from)
            .and_then(|id| install_startup(home, &id));
        match result {
            Ok(()) => json!({"requested":true,"registered":true,"error":null}),
            Err(_) => {
                json!({"requested":true,"registered":false,"error":"Account created. Start at sign-in could not be registered; reopen the app manually."})
            }
        }
    } else {
        json!({"requested":false,"registered":false,"error":null})
    };
    Ok(startup)
}

pub async fn first_account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<FirstAccount>,
) -> ApiResult<Response> {
    same_origin(&state.config, &headers)?;
    if state.config.auth_mode != AuthMode::LocalPassword || !fixed_hex(&input.setup_token) {
        return Err(ApiError::forbidden());
    }
    amplifier_native_platform::local_auth::validate_password(&input.password).map_err(|_| {
        ApiError::bad("Use a password of at least 12 characters and at most 1024 bytes")
    })?;
    let permit = state.login_gate.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Account setup is already running".into(),
        )
    })?;
    let home = state.home.clone();
    let startup = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        create_local_account(&home, input)
    })
    .await
    .map_err(|_| ApiError::bad("Account setup was interrupted; reopen the app to inspect it"))??;
    signed_in(&state, &state.config.owner, "local_password", Some(startup))
}

fn local_password_matches(home: &Path, config: &Config, password: &str) -> bool {
    local_account(home, config)
        .ok()
        .flatten()
        .is_some_and(|saved| {
            amplifier_native_platform::local_auth::verify_password(&saved.password_hash, password)
        })
}

#[cfg(all(unix, not(target_os = "macos")))]
struct PasswordConversation {
    password: Zeroizing<String>,
    answered: bool,
}
#[cfg(all(unix, not(target_os = "macos")))]
impl pam_client::ConversationHandler for PasswordConversation {
    fn prompt_echo_on(&mut self, _: &CStr) -> Result<CString, pam_client::ErrorCode> {
        Err(pam_client::ErrorCode::CONV_ERR)
    }
    fn prompt_echo_off(&mut self, prompt: &CStr) -> Result<CString, pam_client::ErrorCode> {
        // This first UI supports a single ordinary password, not MFA or password changes.
        if self.answered
            || !prompt
                .to_string_lossy()
                .trim()
                .eq_ignore_ascii_case("password:")
        {
            return Err(pam_client::ErrorCode::CONV_ERR);
        }
        self.answered = true;
        CString::new(self.password.as_bytes()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }
    fn text_info(&mut self, _: &CStr) {}
    fn error_msg(&mut self, _: &CStr) {}
}

/// Authenticate first, then return the configured actor used in rooms/audiences.
/// Windows accepts OS account aliases only when their authenticated SID matches
/// an explicitly configured account. Additional users are not restricted to the
/// service process owner, and are not elevated to administrator by an alias.
pub fn authenticate_allowed(
    service: &str,
    username: &str,
    password: String,
    allowed: &[String],
) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = service;
        crate::platform::windows::authenticate_allowed(username, password, allowed)
    }
    #[cfg(unix)]
    {
        let password = Zeroizing::new(password);
        let actor = allowed.iter().find(|actor| actor.as_str() == username)?;
        #[cfg(target_os = "macos")]
        let success = crate::platform::macos::authenticate(service, username, password.to_string());
        #[cfg(not(target_os = "macos"))]
        let success = {
            let conversation = PasswordConversation {
                password,
                answered: false,
            };
            let Ok(mut context) = pam_client::Context::new(service, Some(username), conversation)
            else {
                return None;
            };
            context.authenticate(pam_client::Flag::NONE).is_ok()
                && context.acct_mgmt(pam_client::Flag::NONE).is_ok()
        };
        success.then(|| actor.clone())
    }
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn cookie_name(state: &AppState) -> String {
    format!("acv_{}", state.config.id.replace('-', ""))
}
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        // HTTP/2 may send each cookie in its own field. Other apps on the
        // same hostname (including different ports) can precede our cookie.
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|v| {
            let (k, v) = v.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
}
fn constant_equal(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |sum, (a, b)| sum | (a ^ b))
            == 0
}
#[derive(Clone)]
pub struct Principal {
    pub actor: String,
    pub admin: bool,
    pub csrf: Option<String>,
    pub session_hash: Option<String>,
}

pub fn check(
    state: &AppState,
    headers: &HeaderMap,
    scope: &str,
    mutation: bool,
) -> ApiResult<Principal> {
    validate_host(&state.config, headers)?;
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let token = value
            .to_str()
            .ok()
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(ApiError::unauthorized)?;
        let row: Option<(String, String)> = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id,scopes FROM tokens WHERE hash=?1 AND expires_at>?2",
                params![digest(token), now_ms()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, scopes) = row.ok_or_else(ApiError::unauthorized)?;
        let scopes: Vec<String> = serde_json::from_str(&scopes).unwrap_or_default();
        if !scopes.iter().any(|s| s == "admin" || s == scope) {
            return Err(ApiError::forbidden());
        }
        return Ok(Principal {
            actor: if scopes.iter().any(|s| s == "admin") {
                state.config.owner.clone()
            } else {
                format!("token:{id}")
            },
            admin: scopes.iter().any(|s| s == "admin"),
            csrf: None,
            session_hash: None,
        });
    }
    let token = cookie_value(headers, &cookie_name(state)).ok_or_else(ApiError::unauthorized)?;
    let hash = digest(&token);
    let row: Option<(String, String)> = state
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT owner,csrf FROM browser_sessions WHERE hash=?1 AND expires_at>?2",
            params![hash, now_ms()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (owner, csrf) = row.ok_or_else(ApiError::unauthorized)?;
    if !state.config.allows_user(&owner) {
        return Err(ApiError::unauthorized());
    }
    let admin = owner == state.config.owner;
    if scope == "admin" && !admin {
        return Err(ApiError::forbidden());
    }
    // Browser sessions do not inherit machine-only provider/import scopes.
    if !["admin", "observe", "chat"].contains(&scope) {
        return Err(ApiError::forbidden());
    }
    if mutation {
        same_origin(&state.config, headers)?;
        if !headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| constant_equal(v, &csrf))
        {
            return Err(ApiError::forbidden());
        }
    }
    Ok(Principal {
        actor: owner,
        admin,
        csrf: Some(csrf),
        session_hash: Some(hash),
    })
}
/// Local HTTP is accepted only at the saved literal-loopback authority. This
/// also applies before bearer authentication and to otherwise public routes.
pub fn validate_host(config: &Config, headers: &HeaderMap) -> ApiResult<()> {
    if config.transport == Transport::LocalHttp {
        let expected = config
            .public_url
            .strip_prefix("http://")
            .ok_or_else(ApiError::forbidden)?;
        if headers.get_all(header::HOST).iter().count() != 1
            || headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(expected)
            || headers.get_all(header::ORIGIN).iter().count() > 1
            || headers
                .get(header::ORIGIN)
                .is_some_and(|v| v.to_str().ok() != Some(config.public_url.as_str()))
        {
            return Err(ApiError::forbidden());
        }
    }
    Ok(())
}
pub fn same_origin(config: &Config, headers: &HeaderMap) -> ApiResult<()> {
    validate_host(config, headers)?;
    if config.transport == Transport::LocalHttp {
        // Browser account/session mutations require an explicit exact origin.
        return if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
            == Some(config.public_url.as_str())
        {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        };
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(ApiError::forbidden)?;
        if origin.to_str().ok() != Some(format!("https://{host}").as_str()) {
            return Err(ApiError::forbidden());
        }
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    #[serde(default)]
    username: String,
    password: String,
}
pub async fn login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(login): Json<Login>,
) -> ApiResult<Response> {
    same_origin(&state.config, &headers)?;
    if login.username.len() > 100 || login.password.len() > 1024 || login.password.is_empty() {
        return Err(ApiError::unauthorized());
    }
    let permit = state.login_gate.clone().try_acquire_owned().map_err(|_| {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Please wait before trying to sign in again".into(),
        )
    })?;
    let config = state.config.clone();
    {
        let db = state.db.lock().unwrap();
        let locked: Option<i64> = db
            .query_row(
                "SELECT locked_until FROM login_attempts WHERE key='owner'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if locked.is_some_and(|until| until > now_ms()) {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Please wait before trying to sign in again".into(),
            ));
        }
        db.execute(
            "DELETE FROM browser_sessions WHERE expires_at<=?1",
            [now_ms()],
        )?;
    }
    let method = if config.auth_mode == AuthMode::LocalPassword {
        "local_password"
    } else {
        crate::platform::auth_method()
    };
    let home = state.home.clone();
    let mut allowed = vec![config.owner.clone()];
    allowed.extend(config.allowed_users.iter().cloned());
    let worker = tokio::task::spawn_blocking(move || {
        // The permit remains held even if an OS login outlives the HTTP timeout.
        let _permit = permit;
        if config.auth_mode == AuthMode::LocalPassword {
            let password = Zeroizing::new(login.password);
            ((login.username.is_empty() || login.username == config.owner)
                && local_password_matches(&home, &config, &password))
            .then(|| config.owner.clone())
        } else {
            authenticate_allowed(
                &config.pam_service,
                &login.username,
                login.password,
                &allowed,
            )
        }
    });
    let actor = tokio::time::timeout(std::time::Duration::from_secs(20), worker)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let Some(owner) = actor else {
        state.db.lock().unwrap().execute("INSERT INTO login_attempts(key,failures,locked_until) VALUES('owner',1,0) ON CONFLICT(key) DO UPDATE SET failures=CASE WHEN locked_until>0 AND locked_until<?1 THEN 1 ELSE failures+1 END, locked_until=CASE WHEN failures>=4 THEN ?1+60000 ELSE 0 END",[now_ms()])?;
        state.audit("auth.failed", None, json!({"method":method}))?;
        return Err(ApiError::unauthorized());
    };
    signed_in(&state, &owner, method, None)
}

fn secure_cookie(state: &AppState) -> &'static str {
    if state.config.transport == Transport::LocalHttp {
        ""
    } else {
        "; Secure"
    }
}

fn signed_in(
    state: &AppState,
    owner: &str,
    method: &str,
    startup: Option<Value>,
) -> ApiResult<Response> {
    let token = random_token();
    let csrf = random_token();
    {
        let db = state.db.lock().unwrap();
        db.execute("DELETE FROM login_attempts WHERE key='owner'", [])?;
        db.execute(
            "INSERT INTO browser_sessions(hash,owner,csrf,expires_at) VALUES(?1,?2,?3,?4)",
            params![digest(&token), owner, csrf, now_ms() + 8 * 3600 * 1000],
        )?;
    }
    state.audit(
        "auth.signed_in",
        None,
        json!({"actor":owner,"method":method}),
    )?;
    let cookie = format!(
        "{}={}{}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800",
        cookie_name(state),
        token,
        secure_cookie(state)
    );
    let mut body = json!({"authenticated":true,"owner":owner,"admin":owner == state.config.owner,"csrf":csrf,
        "auth_mode":state.config.auth_mode.as_str(),"first_account_needed":false});
    if let Some(startup) = startup {
        body["account_created"] = json!(true);
        body["startup"] = startup;
    }
    Ok(([(header::SET_COOKIE, cookie)], Json(body)).into_response())
}
pub async fn session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    match check(&state, &headers, "observe", false) {
        Ok(p) => Ok(Json(
            json!({"authenticated":true,"owner":p.actor,"admin":p.admin,"csrf":p.csrf,
                "auth_mode":state.config.auth_mode.as_str(),"first_account_needed":false}),
        )),
        Err(e) if e.0 == StatusCode::UNAUTHORIZED => Ok(Json(
            json!({"authenticated":false,"owner":state.config.owner,"csrf":null,
                "auth_mode":state.config.auth_mode.as_str(),"first_account_needed":first_account_needed(&state.home)?}),
        )),
        Err(e) => Err(e),
    }
}
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Response> {
    let p = check(&state, &headers, "observe", true)?;
    if let Some(hash) = p.session_hash {
        state
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM browser_sessions WHERE hash=?1", [hash])?;
    }
    state.audit("auth.signed_out", None, json!({"actor":p.actor}))?;
    Ok((
        [(
            header::SET_COOKIE,
            format!(
                "{}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
                cookie_name(&state),
                secure_cookie(&state)
            ),
        )],
        Json(json!({"ok":true})),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(unix, not(target_os = "macos")))]
    use pam_client::ConversationHandler;

    fn local_fixture() -> (tempfile::TempDir, Arc<AppState>, String) {
        let dir = tempfile::tempdir().unwrap();
        Config::local(dir.path(), "Local fixture", 54127)
            .unwrap()
            .save(dir.path())
            .unwrap();
        let token = prepare_first_account(dir.path()).unwrap().unwrap();
        let state = AppState::open(dir.path().to_path_buf()).unwrap();
        (dir, state, token)
    }

    fn local_headers(state: &AppState) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            state
                .config
                .public_url
                .trim_start_matches("http://")
                .parse()
                .unwrap(),
        );
        headers.insert(header::ORIGIN, state.config.public_url.parse().unwrap());
        headers
    }

    fn account_input(token: &str) -> FirstAccount {
        FirstAccount {
            setup_token: token.into(),
            password: "fixture password only".into(),
            start_at_signin: false,
        }
    }

    #[test]
    fn local_host_and_origin_must_match_saved_literal_authority() {
        let config = Config::local(Path::new("/unused"), "Fixture", 54127).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:54127".parse().unwrap());
        assert!(validate_host(&config, &headers).is_ok());
        assert!(same_origin(&config, &headers).is_err());
        headers.insert(header::ORIGIN, "http://127.0.0.1:54127".parse().unwrap());
        assert!(same_origin(&config, &headers).is_ok());
        for bad in [
            "localhost:54127",
            "127.0.0.1:54128",
            "evil.example:54127",
            "[::1]:54127",
        ] {
            headers.insert(header::HOST, bad.parse().unwrap());
            assert!(validate_host(&config, &headers).is_err());
        }
        headers.insert(header::HOST, "127.0.0.1:54127".parse().unwrap());
        for bad in [
            "null",
            "https://127.0.0.1:54127",
            "http://localhost:54127",
            "http://127.0.0.1:54127/",
        ] {
            headers.insert(header::ORIGIN, bad.parse().unwrap());
            assert!(same_origin(&config, &headers).is_err());
        }
        headers.insert(header::ORIGIN, "http://127.0.0.1:54127".parse().unwrap());
        headers.append(header::ORIGIN, "http://127.0.0.1:54127".parse().unwrap());
        assert!(same_origin(&config, &headers).is_err());
        headers.remove(header::ORIGIN);
        headers.append(header::HOST, "127.0.0.1:54127".parse().unwrap());
        assert!(validate_host(&config, &headers).is_err());
    }

    #[test]
    fn capability_expiry_and_create_only_account_survive_startup_failure() {
        let (dir, state, token) = local_fixture();
        assert_eq!(
            prepare_first_account(dir.path()).unwrap().as_deref(),
            Some(token.as_str())
        );
        let path = dir.path().join("secrets").join(FIRST_ACCOUNT_FILE);
        let mut expired = read_private::<FirstAccountCapability>(dir.path(), FIRST_ACCOUNT_FILE)
            .unwrap()
            .unwrap();
        expired.expires_at = now_ms() - 1;
        crate::config::private_write(&path, &serde_json::to_vec(&expired).unwrap()).unwrap();
        assert_eq!(
            create_local_account(dir.path(), account_input(&token))
                .unwrap_err()
                .0,
            StatusCode::FORBIDDEN
        );
        assert!(first_account_needed(dir.path()).unwrap());
        let fresh = prepare_first_account(dir.path()).unwrap().unwrap();
        assert_ne!(fresh, token);
        assert!(create_local_account(dir.path(), account_input(&token)).is_err());
        let mut input = account_input(&fresh);
        input.start_at_signin = true;
        let outcome = create_local_account_with_startup(dir.path(), input, |_, _| {
            anyhow::bail!("fixture startup refusal")
        })
        .unwrap();
        assert_eq!(outcome["registered"], false);
        assert_eq!(outcome["requested"], true);
        assert!(
            outcome["error"]
                .as_str()
                .unwrap()
                .contains("Account created")
        );
        assert!(!first_account_needed(dir.path()).unwrap());
        assert!(prepare_first_account(dir.path()).unwrap().is_none());
        let path = dir.path().join("secrets").join(LOCAL_PASSWORD_FILE);
        let before = fs::read(&path).unwrap();
        assert!(create_local_account(dir.path(), account_input(&fresh)).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
        assert!(local_password_matches(
            dir.path(),
            &state.config,
            "fixture password only"
        ));
        assert!(!local_password_matches(
            dir.path(),
            &state.config,
            "different password"
        ));
        assert!(
            !String::from_utf8(before)
                .unwrap()
                .contains("fixture password only")
        );
    }

    #[test]
    fn malformed_existing_account_is_never_replaced() {
        let (dir, _, _) = local_fixture();
        let path = dir.path().join("secrets").join(LOCAL_PASSWORD_FILE);
        amplifier_native_platform::private_create(&path, b"not an account").unwrap();
        assert!(prepare_first_account(dir.path()).is_err());
        assert!(first_account_needed(dir.path()).is_err());
        assert_eq!(fs::read(path).unwrap(), b"not an account");
    }

    #[cfg(unix)]
    #[test]
    fn local_account_rejects_symlink_and_nonprivate_capability() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (dir, _, _) = local_fixture();
        let path = dir.path().join("secrets").join(FIRST_ACCOUNT_FILE);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(prepare_first_account(dir.path()).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&path, dir.path().join("secrets").join(LOCAL_PASSWORD_FILE)).unwrap();
        assert!(first_account_needed(dir.path()).is_err());
    }

    #[tokio::test]
    async fn first_account_and_local_login_share_session_and_csrf_storage() {
        let (dir, state, token) = local_fixture();
        let headers = local_headers(&state);
        let response = first_account(
            State(state.clone()),
            headers.clone(),
            Json(account_input(&token)),
        )
        .await
        .unwrap();
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(!cookie.contains("Secure"));
        assert!(cookie.contains("HttpOnly; SameSite=Strict"));
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["account_created"], true);
        let mut session_headers = headers.clone();
        session_headers.insert(
            header::COOKIE,
            cookie.split(';').next().unwrap().parse().unwrap(),
        );
        assert!(
            check(&state, &session_headers, "admin", false)
                .unwrap()
                .admin
        );
        assert!(check(&state, &session_headers, "admin", true).is_err());
        session_headers.insert(
            "x-csrf-token",
            body["csrf"].as_str().unwrap().parse().unwrap(),
        );
        assert!(check(&state, &session_headers, "admin", true).is_ok());
        session_headers.insert(header::HOST, "evil.example:54127".parse().unwrap());
        assert!(check(&state, &session_headers, "admin", true).is_err());
        let response = login(
            State(state.clone()),
            headers.clone(),
            Json(Login {
                username: String::new(),
                password: "fixture password only".into(),
            }),
        )
        .await
        .unwrap();
        assert!(response.headers().contains_key(header::SET_COOKIE));
        assert!(
            login(
                State(state.clone()),
                headers,
                Json(Login {
                    username: "owner".into(),
                    password: "wrong password".into()
                })
            )
            .await
            .is_err()
        );
        let audit: String = state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT group_concat(data) FROM audit", [], |r| r.get(0))
            .unwrap();
        assert!(!audit.contains("fixture password only"));
        assert!(!audit.contains(&token));
        assert!(prepare_first_account(dir.path()).unwrap().is_none());
    }

    #[test]
    fn update_admission_blocks_first_account_without_writing_credentials() {
        let (dir, state, token) = local_fixture();
        state.db.lock().unwrap().execute("INSERT INTO native_update_admission(transaction_id,phase,acquired_at) VALUES(?1,'prepared',0)", [uuid::Uuid::new_v4().to_string()]).unwrap();
        assert!(create_local_account(dir.path(), account_input(&token)).is_err());
        assert!(
            !dir.path()
                .join("secrets")
                .join(LOCAL_PASSWORD_FILE)
                .exists()
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn password_is_not_reused_for_another_authentication_challenge() {
        let mut c = PasswordConversation {
            password: Zeroizing::new("test".into()),
            answered: false,
        };
        assert!(c.prompt_echo_off(c"Verification code:").is_err());
        assert!(c.prompt_echo_off(c"Password:").is_ok());
        assert!(c.prompt_echo_off(c"Password:").is_err());
        assert!(c.prompt_echo_on(c"Username:").is_err());
    }
    #[test]
    fn unlisted_accounts_fail_without_creating_a_session() {
        assert_eq!(
            authenticate_allowed(
                "agent-conversations-no-such-policy",
                "unlisted",
                "fixture".into(),
                &[]
            ),
            None
        );
    }
}
