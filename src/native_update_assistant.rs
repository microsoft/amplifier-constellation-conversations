//! A downloaded application's temporary update screen for an older installation.
//! It owns only its loopback listener and existing account/session connection.
//! Normal application state, migrations, service ownership and work are not opened.
use crate::{
    auth,
    config::{Config, Transport},
    error::{ApiError, ApiResult},
    native_browser_update as control, native_update, native_update_offer,
};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const LOCK: &str = "native-update-assistant.lock";
const RECEIPT: &str = "native-browser-update/assistant-control.json";
const IDENTITY: &str = "/api/v1/companion/update/assistant-identity";
const CLOSE: &str = "/api/v1/companion/update/assistant-close";
const MAX_RECEIPT: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    installation_id: String,
    offer_id: String,
    candidate: PathBuf,
    candidate_version: String,
    generation: Value,
    nonce: String,
    port: u16,
}

fn local_config(config: &Config, port: u16) -> Result<Config> {
    ensure!(
        config.transport == Transport::LocalHttp && port != 0,
        "The downloaded update screen requires an existing local installation"
    );
    let mut local = config.clone();
    local.bind = "127.0.0.1".into();
    local.port = port;
    local.public_url = format!("http://127.0.0.1:{port}");
    local.validate()?;
    Ok(local)
}

fn receipt(home: &Path) -> Result<Option<Receipt>> {
    read_receipt(home, &home.join(RECEIPT))
}
fn read_receipt(home: &Path, path: &Path) -> Result<Option<Receipt>> {
    platform::validate_private_directory(home)?;
    platform::validate_private_directory(&home.join("native-browser-update"))?;
    if !path.exists() && !path.is_symlink() {
        return Ok(None);
    }
    ensure!(
        control::ordinary(path, false)?.len() <= MAX_RECEIPT as u64,
        "Update screen receipt exceeds its bound"
    );
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.len() <= MAX_RECEIPT as u64
            && crate::native_check_diagnostics::private_file_policy(&file),
        "Invalid update screen receipt"
    );
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_RECEIPT as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let named = control::ordinary(path, false)?;
    ensure!(
        bytes.len() <= MAX_RECEIPT
            && before.len() == after.len()
            && before.modified()? == after.modified()?
            && after.len() == bytes.len() as u64
            && named.len() == after.len()
            && named.modified()? == after.modified()?,
        "Update screen receipt changed during observation"
    );
    let saved: Receipt = serde_json::from_slice(&bytes)?;
    validate_receipt(home, &saved)?;
    Ok(Some(saved))
}

fn validate_receipt(home: &Path, saved: &Receipt) -> Result<()> {
    let offer = native_update_offer::read_offer(home, &saved.offer_id)?;
    ensure!(
        saved.schema == 1
            && saved.installation_id == Config::load(home)?.id
            && saved.installation_id == offer.installation_id
            && saved.port != 0
            && saved.candidate == offer.launcher
            && saved.candidate_version == offer.target_version
            && saved.nonce.len() == 64
            && saved.nonce.bytes().all(|v| v.is_ascii_hexdigit())
            && saved.generation["pid"]
                .as_u64()
                .is_some_and(|v| v > 0 && v <= u32::MAX as u64),
        "The retained update screen identity changed"
    );
    Ok(())
}

fn save_receipt(home: &Path, saved: &Receipt) -> Result<()> {
    validate_receipt(home, saved)?;
    let bytes = serde_json::to_vec(saved)?;
    ensure!(
        bytes.len() <= MAX_RECEIPT,
        "Update screen receipt exceeds its bound"
    );
    // The protected offer directories already exist. Never repair a selected
    // output directory or adopt an unrecognised existing receipt.
    if home.join(RECEIPT).exists() || home.join(RECEIPT).is_symlink() {
        receipt(home)?.context("Missing existing update screen receipt")?;
        platform::private_write_data(&home.join(RECEIPT), &bytes)
    } else {
        platform::private_create(&home.join(RECEIPT), &bytes)
    }
}

struct Assistant {
    home: PathBuf,
    config: Config,
    installation_origin: String,
    db: Mutex<Connection>,
    login_gate: Arc<tokio::sync::Semaphore>,
    identity: Receipt,
    closing: Mutex<bool>,
    shutdown: tokio::sync::watch::Sender<bool>,
}
impl Assistant {
    fn open(home: PathBuf, identity: Receipt) -> Result<Arc<Self>> {
        validate_receipt(&home, &identity)?;
        let saved = Config::load(&home)?;
        ensure!(
            !auth::first_account_needed(&home)?,
            "Finish first-account setup in the installed application before updating"
        );
        let config = local_config(&saved, identity.port)?;
        let path = home.join("state.sqlite3");
        control::ordinary(&path, false)?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_secs(3))?;
        // The same fixed existing-auth schema as the recovery listener. No
        // AppState, PRAGMA, schema creation, token renewal or work recovery.
        for query in [
            "SELECT hash,owner,csrf,expires_at FROM browser_sessions LIMIT 0",
            "SELECT key,failures,locked_until FROM login_attempts LIMIT 0",
            "SELECT id,hash,scopes,expires_at FROM tokens LIMIT 0",
            "SELECT seq,at,kind,conversation_id,data FROM audit LIMIT 0",
        ] {
            db.prepare(query)?;
        }
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Ok(Arc::new(Self {
            home,
            config,
            installation_origin: saved.public_url,
            db: Mutex::new(db),
            login_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            identity,
            closing: Mutex::new(false),
            shutdown,
        }))
    }
    fn account(&self) -> auth::AccountContext<'_> {
        auth::AccountContext {
            home: &self.home,
            config: &self.config,
            db: &self.db,
            login_gate: &self.login_gate,
            append_only_audit: true,
        }
    }
}

async fn gate(
    State(state): State<Arc<Assistant>>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    if !request.headers().contains_key(header::HOST)
        && let Some(authority) = request.uri().authority().map(|a| a.as_str().to_owned())
        && let Ok(value) = authority.parse()
    {
        request.headers_mut().insert(header::HOST, value);
    }
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().count() != 1
        || headers.get_all(header::ORIGIN).iter().count() > 1
        || headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(state.config.public_url.as_str()))
        || auth::validate_host(&state.config, headers).is_err()
    {
        return ApiError::forbidden().into_response();
    }
    let mut response = next.run(request).await;
    crate::server::secure_response(&mut response, &[], false);
    response.headers_mut().insert(
        header::HeaderName::from_static("permissions-policy"),
        "microphone=(), camera=(), display-capture=()"
            .parse()
            .unwrap(),
    );
    response
}

fn router(state: Arc<Assistant>) -> Router {
    Router::new()
        .merge(control::assets())
        .route("/", get(|| async { Redirect::temporary("/updates") }))
        .route("/companion", get(back_to_installation))
        .route(
            "/companion.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/companion.css"),
                )
            }),
        )
        .route("/auth/session", get(session))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/local-listener-proof", post(proof))
        .route(IDENTITY, get(identity))
        .route(CLOSE, get(close_status).post(close_screen))
        .route("/api/v1/companion/update", get(status))
        .route("/api/v1/companion/update/request", post(request_update))
        .route("/api/v1/companion/update/cancel", post(cancel))
        .route("/api/v1/companion/update/continue", post(continue_update))
        .route("/api/v1/companion/update/recover", post(recover))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}
async fn back_to_installation(State(state): State<Arc<Assistant>>) -> Redirect {
    Redirect::temporary(&format!("{}/companion", state.installation_origin))
}
async fn session(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    auth::session_account(state.account(), headers)
}
async fn login(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Json(input): Json<auth::Login>,
) -> ApiResult<Response> {
    auth::login_account(state.account(), headers, input).await
}
async fn logout(State(state): State<Arc<Assistant>>, headers: HeaderMap) -> ApiResult<Response> {
    auth::logout_account(state.account(), headers)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    nonce: String,
}
async fn proof(
    State(state): State<Arc<Assistant>>,
    Json(input): Json<Proof>,
) -> ApiResult<Json<Value>> {
    crate::desktop::listener_proof(&state.home, &state.config, &input.nonce)
        .map(Json)
        .map_err(|_| ApiError::forbidden())
}
async fn identity(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(ApiError::forbidden());
    }
    auth::check_account(state.account(), &headers, "admin", false)?;
    Ok(Json(
        serde_json::to_value(&state.identity).map_err(anyhow::Error::from)?,
    ))
}
async fn close_status(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", false)?;
    blocking(move || {
        Ok(json!({"identity":state.identity,
        "can_close":control::assistant_can_close(&state.home)?}))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Close {
    listener_nonce: String,
}
fn close_intent(home: &Path, saved: &Receipt) -> Result<bool> {
    validate_receipt(home, saved)?;
    let path = home
        .join("native-browser-update")
        .join(format!("assistant-close-{}.json", saved.nonce));
    if let Some(existing) = read_receipt(home, &path)? {
        ensure!(existing == *saved, "Update screen close intent changed");
        return Ok(false);
    }
    let bytes = serde_json::to_vec(saved)?;
    ensure!(
        bytes.len() <= MAX_RECEIPT,
        "Update screen close intent exceeds its bound"
    );
    // No replace: only the creator may dispatch this exact listener close.
    platform::private_create(&path, &bytes)?;
    Ok(true)
}
async fn close_screen(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Json(input): Json<Close>,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", true)?;
    if !auth::constant_equal(&state.identity.nonce, &input.listener_nonce) {
        return Err(ApiError::forbidden());
    }
    blocking(move || {
        let mut closing = state.closing.lock().unwrap();
        if !*closing {
            if !control::assistant_can_close(&state.home)? {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Keep this update screen open until its retained update is settled".into(),
                ));
            }
            close_intent(&state.home, &state.identity)?;
            // Publication can fail. Until it succeeds, retain that failure and
            // do not claim this listener is closing. No fallible gap follows.
            *closing = true;
            state.shutdown.send_replace(true);
        }
        Ok(json!({"closing":true,"listener_nonce":state.identity.nonce}))
    })
    .await
}
fn mutate(state: &Assistant, action: impl FnOnce() -> ApiResult<Value>) -> ApiResult<Value> {
    let closing = state.closing.lock().unwrap();
    if *closing {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "This update screen is closing; reopen the application to observe the retained update"
                .into(),
        ));
    }
    action()
}
async fn blocking(
    action: impl FnOnce() -> ApiResult<Value> + Send + 'static,
) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The update reply could not be observed; reopen this update to check its status"
                    .into(),
            )
        })?
        .map(Json)
}
async fn status(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Query(mut query): Query<control::StatusQuery>,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", false)?;
    if query
        .offer_id
        .as_deref()
        .is_some_and(|id| id != state.identity.offer_id)
    {
        return Err(ApiError::forbidden());
    }
    query.offer_id = Some(state.identity.offer_id.clone());
    blocking(move || {
        let mut value = control::update_status(&state.home, "normal", &query)?;
        value["assistant"] = json!(true);
        value["actions"]["advanced_package"] = json!(false);
        value["companion_url"] = json!(format!("{}/companion", state.installation_origin));
        Ok(value)
    })
    .await
}
async fn request_update(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Json(input): Json<control::UpdateInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth::check_account(state.account(), &headers, "admin", true)?;
    if input.offer_id.as_deref() != Some(&state.identity.offer_id) || input.package_path.is_some() {
        return Err(ApiError::forbidden());
    }
    let result =
        blocking(move || mutate(&state, || control::request_update(&state.home, input))).await?;
    Ok((StatusCode::ACCEPTED, result))
}
async fn cancel(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Json(input): Json<control::UpdateControlInput>,
) -> ApiResult<Json<Value>> {
    change(state, headers, input, "cancel").await
}
async fn continue_update(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Json(input): Json<control::UpdateControlInput>,
) -> ApiResult<Json<Value>> {
    change(state, headers, input, "continue").await
}
async fn change(
    state: Arc<Assistant>,
    headers: HeaderMap,
    input: control::UpdateControlInput,
    action: &'static str,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", true)?;
    blocking(move || {
        mutate(&state, || {
            control::control_update(&state.home, &input, action)
        })
    })
    .await
}
async fn recover(
    State(state): State<Arc<Assistant>>,
    headers: HeaderMap,
    Json(input): Json<control::RecoverInput>,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", true)?;
    blocking(move || {
        mutate(&state, || {
            control::recover(&state.home, "normal", input, control::launcher())
        })
    })
    .await
}

fn open_page(saved: &Receipt, browser: bool) -> Result<()> {
    if browser {
        platform::open_browser(&format!(
            "http://127.0.0.1:{}/updates?offer={}",
            saved.port, saved.offer_id
        ))?;
    }
    Ok(())
}

fn lock_held(home: &Path) -> Result<bool> {
    match native_update::lock(home, LOCK) {
        Ok(lock) => {
            drop(lock);
            Ok(false)
        }
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            }) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

/// During an unresolved update the old application can intentionally be absent.
/// Reopen only an already-owned assistant; this path never binds or starts one.
pub(crate) async fn reopen_active(home: &Path, browser: bool) -> Result<bool> {
    let lock = home.join(LOCK);
    let held = if lock.exists() || lock.is_symlink() {
        lock_held(home)?
    } else {
        false
    };
    let path = home.join(RECEIPT);
    if !path.exists() && !path.is_symlink() {
        ensure!(
            !held,
            "The update screen is starting without a published identity; observe it without starting another listener"
        );
        return Ok(false);
    }
    let saved = receipt(home)?.context("The update screen identity disappeared")?;
    if !platform::process::matches(&saved.generation)? {
        ensure!(
            !held,
            "A different process holds the update screen lock; its identity is uncertain"
        );
        return Ok(false);
    }
    ensure!(
        held,
        "The recorded update screen is alive without its ownership lock"
    );
    let offer = native_update_offer::read_offer(home, &saved.offer_id)?;
    reopen(home, &offer, browser).await?;
    Ok(true)
}
async fn reopen(home: &Path, offer: &native_update_offer::Offer, browser: bool) -> Result<()> {
    let saved = receipt(home)?
        .context("The update screen is starting; reopen the downloaded application shortly")?;
    ensure!(
        saved.offer_id == offer.id
            && saved.candidate == offer.launcher
            && platform::process::matches(&saved.generation)?
            && lock_held(home)?,
        "The retained update screen cannot be verified; no other screen was started"
    );
    let response = proved(home, &saved, "GET", IDENTITY, None).await?;
    ensure!(
        response == serde_json::to_value(&saved)?
            && receipt(home)?.as_ref() == Some(&saved)
            && platform::process::matches(&saved.generation)?
            && lock_held(home)?,
        "The retained update screen changed while reopening"
    );
    open_page(&saved, browser)
}

async fn proved(
    home: &Path,
    saved: &Receipt,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let config = local_config(&Config::load(home)?, saved.port)?;
    let token = fs::read_to_string(home.join("secrets/control-token"))?;
    tokio::time::timeout(
        Duration::from_secs(5),
        crate::desktop::proved_local_api(&config, token.trim(), method, path, body),
    )
    .await
    .context("The retained update screen did not answer; observe it without repeating the action")?
}

async fn dispatch_close_once<F, Fut>(
    home: &Path,
    saved: &Receipt,
    send: F,
) -> Result<Option<Result<Value>>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    if close_intent(home, saved)? {
        Ok(Some(send().await))
    } else {
        Ok(None)
    }
}

/// A different downloaded release may retire only a proved, settled assistant.
/// This closes that screen, never the installed service or any retained update.
async fn handoff(home: &Path, offer: &native_update_offer::Offer, browser: bool) -> Result<bool> {
    let saved = receipt(home)?
        .context("The previous update screen is starting; reopen the download shortly")?;
    if saved.offer_id == offer.id {
        reopen(home, offer, browser).await?;
        return Ok(false);
    }
    ensure!(
        platform::process::matches(&saved.generation)? && lock_held(home)?,
        "The previous update screen ownership is uncertain"
    );
    let status = proved(home, &saved, "GET", CLOSE, None).await?;
    ensure!(
        status["identity"] == serde_json::to_value(&saved)?
            && receipt(home)?.as_ref() == Some(&saved)
            && platform::process::matches(&saved.generation)?
            && lock_held(home)?,
        "The previous update screen changed before handoff"
    );
    if status["can_close"] != true {
        // Opening another download must not cancel a pending user intent.
        open_page(&saved, browser)?;
        return Ok(false);
    }
    // Durable intent comes before the single request. A lost response (or a
    // later open after interruption) permits observations only, never POST.
    let _reply = dispatch_close_once(home, &saved, || {
        proved(
            home,
            &saved,
            "POST",
            CLOSE,
            Some(json!({"listener_nonce":saved.nonce})),
        )
    })
    .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !platform::process::matches(&saved.generation)? && !lock_held(home)? {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context(
        "The previous update screen's close remains uncertain; no replacement was started",
    )??;
    Ok(true)
}

/// The caller has already proved the selected ordinary application and listener.
/// The new application's own process remains independent of the old service.
pub(crate) async fn serve(home: PathBuf, offer_id: &str, browser: bool) -> Result<()> {
    let offer = native_update_offer::read_offer(&home, offer_id)?;
    ensure!(
        offer.launcher == dunce::canonicalize(std::env::current_exe()?)?
            && offer.target_version == env!("CARGO_PKG_VERSION"),
        "Open the application that supplied this update offer"
    );
    let guard = match native_update::lock(&home, LOCK) {
        Ok(guard) => guard,
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            }) =>
        {
            if !handoff(&home, &offer, browser).await? {
                return Ok(());
            }
            // No retry: after exact prior generation exit and lock release,
            // acquire once. A competing owner is not permission to start.
            native_update::lock(&home, LOCK)?
        }
        Err(error) => return Err(error),
    };
    if let Some(previous) = receipt(&home)? {
        ensure!(
            !platform::process::matches(&previous.generation)?,
            "The previous update screen is still alive without its lock; its state is uncertain"
        );
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let identity = Receipt {
        schema: 1,
        installation_id: offer.installation_id,
        offer_id: offer.id,
        candidate: offer.launcher,
        candidate_version: offer.target_version,
        generation: platform::process::identity(std::process::id())?
            .context("Missing update screen process identity")?,
        nonce: auth::random_token(),
        port: listener.local_addr()?.port(),
    };
    let state = Assistant::open(home.clone(), identity.clone())?;
    save_receipt(&home, &identity)?;
    open_page(&identity, browser)?;
    let handle = axum_server::Handle::new();
    let signal = handle.clone();
    let mut shutdown = state.shutdown.subscribe();
    let interrupt = tokio::spawn(async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = shutdown.changed() => {},
        }
        signal.graceful_shutdown(Some(Duration::from_secs(3)));
    });
    let result = axum_server::from_tcp(listener)?
        .handle(handle.clone())
        .serve(router(state).into_make_service())
        .await;
    interrupt.abort();
    handle.shutdown();
    if tokio::time::timeout(Duration::from_secs(5), async {
        while handle.connection_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_err()
    {
        // The lock remains held until process exit; no duplicate listener may
        // be started when retained connections cannot be proved closed.
        std::mem::forget(guard);
        anyhow::bail!("Update screen connections remain uncertain until this process exits");
    }
    drop(guard);
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    fn headers(config: &Config) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            config
                .public_url
                .strip_prefix("http://")
                .unwrap()
                .parse()
                .unwrap(),
        );
        headers.insert(header::ORIGIN, config.public_url.parse().unwrap());
        headers
    }

    async fn fixture() -> (tempfile::TempDir, Receipt) {
        let dir = tempfile::tempdir().unwrap();
        Config::local(dir.path(), "Assistant fixture", 54127)
            .unwrap()
            .save(dir.path())
            .unwrap();
        let app = crate::state::AppState::open(dir.path().to_owned()).unwrap();
        let setup = auth::prepare_first_account(dir.path()).unwrap().unwrap();
        auth::first_account(State(app.clone()),headers(&app.config),Json(serde_json::from_value(json!({
            "setup_token":setup,"password":"assistant fixture password","start_at_signin":false
        })).unwrap())).await.unwrap();
        app.db.lock().unwrap().execute_batch("CREATE TABLE assistant_sentinel(kind TEXT,value TEXT); INSERT INTO assistant_sentinel VALUES('work','working'),('receipt','unknown'),('grant','consumed'),('budget','remaining');").unwrap();
        drop(app);
        let folder = dir.path().join("download");
        let selected = crate::desktop::selected_package_paths(&folder);
        fs::create_dir_all(selected.launcher.parent().unwrap()).unwrap();
        fs::write(&selected.launcher, b"unverified fixture executable").unwrap();
        let offer =
            native_update_offer::create_offer(dir.path(), folder, selected.launcher, "99.0.0")
                .unwrap();
        let saved = Receipt {
            schema: 1,
            installation_id: offer.installation_id,
            offer_id: offer.id,
            candidate: offer.launcher,
            candidate_version: offer.target_version,
            generation: platform::process::identity(std::process::id())
                .unwrap()
                .unwrap(),
            nonce: auth::random_token(),
            port: 54128,
        };
        (dir, saved)
    }

    fn protected(db: &Connection) -> Vec<String> {
        let mut output = Vec::new();
        let mut tables = db
            .prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let names: Vec<(String, String)> = tables
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (name, schema) in names {
            output.push(schema);
            if [
                "browser_sessions",
                "login_attempts",
                "audit",
                "sqlite_sequence",
            ]
            .contains(&name.as_str())
            {
                continue;
            }
            let mut statement = db
                .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
                .unwrap();
            let width = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                output.push(format!(
                    "{name}:{:?}",
                    (0..width)
                        .map(|i| row.get_ref(i).unwrap())
                        .collect::<Vec<_>>()
                ));
            }
        }
        output
    }

    #[tokio::test]
    async fn existing_account_opens_without_migration_or_service_ownership() {
        let (dir, saved) = fixture().await;
        let config = fs::read(dir.path().join("config.json")).unwrap();
        let token = fs::read(dir.path().join("secrets/control-token")).unwrap();
        let original = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        let before = protected(&original);
        let _service = native_update::lock(dir.path(), "service.lock").unwrap();
        let state = Assistant::open(dir.path().to_owned(), saved).unwrap();
        assert_eq!(state.config.public_url, "http://127.0.0.1:54128");
        assert_eq!(state.installation_origin, "http://127.0.0.1:54127");
        let response = login(
            State(state.clone()),
            headers(&state.config),
            Json(
                serde_json::from_value(json!({
                    "password":"assistant fixture password"
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 16384).await.unwrap()).unwrap();
        let mut signed = headers(&state.config);
        signed.insert(header::COOKIE, cookie.parse().unwrap());
        signed.insert(
            "x-csrf-token",
            body["csrf"].as_str().unwrap().parse().unwrap(),
        );
        assert!(auth::check_account(state.account(), &signed, "admin", true).is_ok());
        let mut wrong = signed.clone();
        wrong.insert(header::ORIGIN, state.installation_origin.parse().unwrap());
        assert!(auth::check_account(state.account(), &wrong, "admin", true).is_err());
        signed.remove("x-csrf-token");
        assert!(auth::check_account(state.account(), &signed, "admin", true).is_err());
        assert_eq!(protected(&original), before);
        assert_eq!(fs::read(dir.path().join("config.json")).unwrap(), config);
        assert_eq!(
            fs::read(dir.path().join("secrets/control-token")).unwrap(),
            token
        );
        assert!(native_update::lock(dir.path(), "service.lock").is_err());
        assert!(!dir.path().join("runtime.json").exists());
        assert!(!dir.path().join("native-installation.json").exists());
    }

    #[tokio::test]
    async fn wrong_origin_and_ordinary_application_routes_are_refused() {
        let (dir, saved) = fixture().await;
        let state = Assistant::open(dir.path().to_owned(), saved).unwrap();
        for origin in [
            "http://127.0.0.1:54127",
            "http://localhost:54128",
            "https://example.invalid",
        ] {
            let response = router(state.clone())
                .oneshot(
                    Request::builder()
                        .uri("/updates")
                        .header(header::HOST, "127.0.0.1:54128")
                        .header(header::ORIGIN, origin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        for path in [
            "/health",
            "/api/v1/agents",
            "/auth/first-account",
            "/api/v1/companion/update/check",
            "/api/v1/companion/update/apply",
        ] {
            let response = router(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::HOST, "127.0.0.1:54128")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/companion")
                    .header(header::HOST, "127.0.0.1:54128")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.headers()[header::LOCATION],
            "http://127.0.0.1:54127/companion"
        );
    }

    #[tokio::test]
    async fn incomplete_existing_auth_schema_is_not_created_or_repaired() {
        let (dir, saved) = fixture().await;
        let db = Connection::open(dir.path().join("state.sqlite3")).unwrap();
        db.execute_batch("DROP TABLE browser_sessions").unwrap();
        let before = protected(&db);
        assert!(Assistant::open(dir.path().to_owned(), saved).is_err());
        assert_eq!(protected(&db), before);
        assert!(db.prepare("SELECT * FROM browser_sessions").is_err());
    }

    #[tokio::test]
    async fn receipt_is_installation_offer_bound_and_bounded() {
        let (dir, saved) = fixture().await;
        save_receipt(dir.path(), &saved).unwrap();
        assert_eq!(receipt(dir.path()).unwrap(), Some(saved.clone()));
        let mut changed = saved.clone();
        changed.offer_id = uuid::Uuid::new_v4().to_string();
        assert!(save_receipt(dir.path(), &changed).is_err());
        changed = saved.clone();
        changed.candidate = dir.path().join("different");
        assert!(save_receipt(dir.path(), &changed).is_err());
        changed = saved.clone();
        changed.installation_id = uuid::Uuid::new_v4().to_string();
        assert!(save_receipt(dir.path(), &changed).is_err());
        assert_eq!(receipt(dir.path()).unwrap(), Some(saved));
        platform::private_write_data(&dir.path().join(RECEIPT), &vec![b' '; MAX_RECEIPT + 1])
            .unwrap();
        assert!(receipt(dir.path()).is_err());
    }

    #[tokio::test]
    async fn close_is_exact_settled_once_and_fences_later_mutations() {
        let (dir, saved) = fixture().await;
        let state = Assistant::open(dir.path().to_owned(), saved.clone()).unwrap();
        let _service = native_update::lock(dir.path(), "service.lock").unwrap();
        let token = fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
        let mut signed = headers(&state.config);
        signed.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", token.trim()).parse().unwrap(),
        );
        let wrong = close_screen(
            State(state.clone()),
            signed.clone(),
            Json(Close {
                listener_nonce: auth::random_token(),
            }),
        )
        .await;
        assert_eq!(wrong.unwrap_err().0, StatusCode::FORBIDDEN);
        let owner = native_update::lock(dir.path(), "native-update.lock").unwrap();
        let busy = close_screen(
            State(state.clone()),
            signed.clone(),
            Json(Close {
                listener_nonce: saved.nonce.clone(),
            }),
        )
        .await;
        assert_eq!(busy.unwrap_err().0, StatusCode::CONFLICT);
        assert!(!*state.closing.lock().unwrap());
        assert!(
            !dir.path()
                .join("native-browser-update")
                .join(format!("assistant-close-{}.json", saved.nonce))
                .exists()
        );
        drop(owner);
        let result = close_screen(
            State(state.clone()),
            signed.clone(),
            Json(Close {
                listener_nonce: saved.nonce.clone(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.0["closing"], true);
        assert!(*state.shutdown.borrow());
        assert!(!close_intent(dir.path(), &saved).unwrap());
        assert!(
            mutate(&state, || panic!(
                "A closing assistant must not admit another request"
            ))
            .is_err()
        );
        let _ = close_screen(
            State(state),
            signed,
            Json(Close {
                listener_nonce: saved.nonce,
            }),
        )
        .await
        .unwrap();
        assert!(native_update::lock(dir.path(), "service.lock").is_err());
        assert!(
            !dir.path()
                .join("native-browser-update/control.json")
                .exists()
        );
        assert!(
            !dir.path()
                .join("native-browser-update/update-flow.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn failed_close_intent_never_claims_or_acknowledges_shutdown() {
        let (dir, saved) = fixture().await;
        let state = Assistant::open(dir.path().to_owned(), saved.clone()).unwrap();
        let token = fs::read_to_string(dir.path().join("secrets/control-token")).unwrap();
        let mut signed = headers(&state.config);
        signed.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", token.trim()).parse().unwrap(),
        );
        let intent = dir
            .path()
            .join("native-browser-update")
            .join(format!("assistant-close-{}.json", saved.nonce));
        platform::private_create(&intent, b"unrecognised retained intent").unwrap();
        for _ in 0..2 {
            let response = close_screen(
                State(state.clone()),
                signed.clone(),
                Json(Close {
                    listener_nonce: saved.nonce.clone(),
                }),
            )
            .await;
            assert!(response.is_err());
            assert!(!*state.closing.lock().unwrap());
            assert!(!*state.shutdown.borrow());
            assert_eq!(fs::read(&intent).unwrap(), b"unrecognised retained intent");
        }
    }

    #[tokio::test]
    async fn reopened_screen_proves_same_listener_generation_and_retains_both_locks() {
        let (dir, mut saved) = fixture().await;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        saved.port = listener.local_addr().unwrap().port();
        let _service = native_update::lock(dir.path(), "service.lock").unwrap();
        let _assistant = native_update::lock(dir.path(), LOCK).unwrap();
        let state = Assistant::open(dir.path().to_owned(), saved.clone()).unwrap();
        save_receipt(dir.path(), &saved).unwrap();
        let offer = native_update_offer::read_offer(dir.path(), &saved.offer_id).unwrap();
        let handle = axum_server::Handle::new();
        let server = tokio::spawn(
            axum_server::from_tcp(listener)
                .unwrap()
                .handle(handle.clone())
                .serve(router(state).into_make_service()),
        );
        let good = reopen(dir.path(), &offer, false).await;
        let active = reopen_active(dir.path(), false).await;
        let mut changed = saved.clone();
        changed.nonce = auth::random_token();
        save_receipt(dir.path(), &changed).unwrap();
        let changed_reply = reopen(dir.path(), &offer, false).await;
        let changed_active = reopen_active(dir.path(), false).await;
        let still_owned = lock_held(dir.path()).unwrap()
            && native_update::lock(dir.path(), "service.lock").is_err();
        handle.shutdown();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        good.unwrap();
        assert!(active.unwrap());
        assert!(changed_reply.is_err());
        assert!(changed_active.is_err());
        assert!(still_owned);
        assert_eq!(receipt(dir.path()).unwrap(), Some(changed));
    }

    #[tokio::test]
    async fn absent_or_gone_assistant_falls_back_but_ownership_uncertainty_refuses() {
        let (dir, mut saved) = fixture().await;
        assert!(!reopen_active(dir.path(), false).await.unwrap());
        assert!(!dir.path().join(LOCK).exists());
        let guard = native_update::lock(dir.path(), LOCK).unwrap();
        assert!(reopen_active(dir.path(), false).await.is_err());
        drop(guard);
        save_receipt(dir.path(), &saved).unwrap();
        // A live recorded process without its lock is not confirmed absence.
        assert!(reopen_active(dir.path(), false).await.is_err());
        #[cfg(target_os = "linux")]
        let birth = "start_ticks";
        #[cfg(target_os = "macos")]
        let birth = "start_seconds";
        #[cfg(windows)]
        let birth = "creation_time_100ns";
        let prior = saved.generation[birth]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        saved.generation[birth] = json!((prior + 1).to_string());
        save_receipt(dir.path(), &saved).unwrap();
        assert!(!reopen_active(dir.path(), false).await.unwrap());
        let _guard = native_update::lock(dir.path(), LOCK).unwrap();
        assert!(reopen_active(dir.path(), false).await.is_err());
    }

    #[tokio::test]
    async fn lost_close_reply_retains_intent_and_never_dispatches_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (dir, saved) = fixture().await;
        let calls = AtomicUsize::new(0);
        let first = dispatch_close_once(dir.path(), &saved, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("injected lost close reply")
        })
        .await;
        assert_eq!(
            first.unwrap().unwrap().unwrap_err().to_string(),
            "injected lost close reply"
        );
        let second = dispatch_close_once(dir.path(), &saved, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"unexpected_second_dispatch":true}))
        })
        .await
        .unwrap();
        assert!(second.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unsafe_existing_receipt_is_preserved_without_repair() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, saved) = fixture().await;
        save_receipt(dir.path(), &saved).unwrap();
        let path = dir.path().join(RECEIPT);
        let before = fs::read(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(receipt(dir.path()).is_err());
        assert!(save_receipt(dir.path(), &saved).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn unsafe_windows_receipt_dacl_is_preserved_without_repair() {
        use std::ptr;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                },
                DACL_SECURITY_INFORMATION, GetFileSecurityW, OWNER_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
            },
        };
        fn policy(path: &Path) -> Vec<u8> {
            let path = platform::windows_native_path(path).unwrap();
            let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            let mut needed = 0;
            unsafe {
                GetFileSecurityW(path.as_ptr(), flags, ptr::null_mut(), 0, &mut needed);
            }
            assert!(needed > 0 && needed <= 65536);
            let mut storage = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
            assert_ne!(
                unsafe {
                    GetFileSecurityW(
                        path.as_ptr(),
                        flags,
                        storage.as_mut_ptr().cast(),
                        needed,
                        &mut needed,
                    )
                },
                0
            );
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), needed as usize) }
                .to_vec()
        }
        let (dir, saved) = fixture().await;
        save_receipt(dir.path(), &saved).unwrap();
        let path = dir.path().join(RECEIPT);
        let bytes = fs::read(&path).unwrap();
        let directory = policy(path.parent().unwrap());
        assert!(crate::native_check_diagnostics::private_file_policy(
            &fs::File::open(&path).unwrap()
        ));
        // Fixture-only: broaden this single disposable file, never its parent.
        let sid = platform::current_user_sid().unwrap();
        let descriptor: Vec<u16> = format!("D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;GR;;;BU)")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut raw = ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    descriptor.as_ptr(),
                    SDDL_REVISION_1,
                    &mut raw,
                    ptr::null_mut(),
                )
            },
            0
        );
        let name = platform::windows_native_path(&path).unwrap();
        let changed = unsafe {
            SetFileSecurityW(
                name.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                raw,
            )
        };
        unsafe {
            LocalFree(raw);
        }
        assert_ne!(changed, 0);
        let broad = policy(&path);
        assert!(receipt(dir.path()).is_err());
        assert!(save_receipt(dir.path(), &saved).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(policy(&path), broad);
        assert_eq!(policy(path.parent().unwrap()), directory);
    }
}
