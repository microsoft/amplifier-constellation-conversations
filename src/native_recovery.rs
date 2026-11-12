//! Restricted recovery listener. It opens only existing authentication storage;
//! application migrations, work recovery and normal health are never started.
use crate::{
    auth,
    config::{Config, Transport},
    error::{ApiError, ApiResult},
    native_browser_update as control, native_update,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const RECEIPT: &str = "native-browser-update/recovery-control.json";
const RELEASE_PATH: &str = "/api/v1/companion/update/recovery-release";
const IDENTITY_PATH: &str = "/api/v1/companion/update/recovery-identity";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    installation_id: String,
    transaction_id: String,
    listener_nonce: String,
    state: String,
    operation_id: Option<String>,
    attempt_id: Option<String>,
    pid: u32,
    port: u16,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Release {
    pub operation_id: String,
    pub attempt_id: String,
    pub nonce: String,
}
fn receipt(home: &Path) -> Result<Option<Receipt>> {
    let path = home.join(RECEIPT);
    if !path.exists() {
        return Ok(None);
    }
    ensure!(
        control::ordinary(&path, false)?.len() <= 8192,
        "Recovery control receipt exceeds its bound"
    );
    let value: Receipt = serde_json::from_slice(&fs::read(path)?)?;
    ensure!(
        value.schema == 1
            && value.installation_id == Config::load(home)?.id
            && matches!(value.state.as_str(), "listening" | "released"),
        "Recovery control identity changed"
    );
    Ok(Some(value))
}
fn save_receipt(home: &Path, value: &Receipt) -> Result<()> {
    amplifier_native_platform::private_dir(&home.join("native-browser-update"))?;
    amplifier_native_platform::private_write_data(&home.join(RECEIPT), &serde_json::to_vec(value)?)
}

struct RecoveryState {
    home: PathBuf,
    config: Config,
    db: Mutex<Connection>,
    login_gate: Arc<tokio::sync::Semaphore>,
    shutdown: tokio::sync::watch::Sender<bool>,
    release: Mutex<Option<Release>>,
    identity: Receipt,
    origin: String,
}
impl RecoveryState {
    fn open(home: PathBuf, identity: Receipt, origin: String) -> Result<Arc<Self>> {
        let config = Config::load(&home)?;
        let path = home.join("state.sqlite3");
        control::ordinary(&path, false)
            .context("Recovery requires the existing authentication database")?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_secs(3))?;
        // Preparing these fixed reads validates the existing schema without
        // creating tables, running PRAGMAs, refreshing tokens or touching work.
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
            db: Mutex::new(db),
            login_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            shutdown,
            release: Mutex::new(None),
            identity,
            origin,
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
fn authority(origin: &str) -> Result<&str> {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .filter(|s| {
            !s.is_empty()
                && !s.contains('/')
                && !s.contains('@')
                && !s.contains('?')
                && !s.contains('#')
        })
        .context("Recovery needs a fixed configured origin")
}
async fn gate(
    State(state): State<Arc<RecoveryState>>,
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
    let exact = authority(&state.origin).ok();
    if headers.get_all(header::HOST).iter().count() != 1
        || headers.get(header::HOST).and_then(|v| v.to_str().ok()) != exact
        || headers.get_all(header::ORIGIN).iter().count() > 1
        || headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(state.origin.as_str()))
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
fn router(state: Arc<RecoveryState>) -> Router {
    Router::new()
        .merge(control::assets())
        .route(
            "/companion.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../ui/companion.css"),
                )
            }),
        )
        .route("/", get(|| async { Redirect::temporary("/updates") }))
        .route(
            "/companion",
            get(|| async { Redirect::temporary("/updates") }),
        )
        .route("/auth/session", get(session))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/local-listener-proof", post(proof))
        .route("/api/v1/companion/update", get(status))
        .route("/api/v1/companion/update/recover", post(recover))
        .route(RELEASE_PATH, post(release))
        .route(IDENTITY_PATH, get(identity))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}
async fn session(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    auth::session_account(state.account(), headers)
}
async fn login(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
    Json(input): Json<auth::Login>,
) -> ApiResult<Response> {
    auth::login_account(state.account(), headers, input).await
}
async fn logout(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    auth::logout_account(state.account(), headers)
}
async fn status(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
    Query(query): Query<control::StatusQuery>,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", false)?;
    Ok(Json(control::status(
        &state.home,
        "recovery",
        query.request_id.as_deref(),
        control::launcher(),
    )?))
}
async fn recover(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
    Json(input): Json<control::RecoverInput>,
) -> ApiResult<Json<Value>> {
    auth::check_account(state.account(), &headers, "admin", true)?;
    let home = state.home.clone();
    tokio::task::spawn_blocking(move || {
        control::recover(&home, "recovery", input, control::launcher())
    })
    .await
    .map_err(|_| {
        ApiError(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Recovery request could not be observed".into(),
        )
    })?
    .map(Json)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    nonce: String,
}
async fn proof(
    State(state): State<Arc<RecoveryState>>,
    Json(input): Json<Proof>,
) -> ApiResult<Json<Value>> {
    crate::desktop::listener_proof(&state.home, &state.config, &input.nonce)
        .map(Json)
        .map_err(|_| ApiError::forbidden())
}
fn machine(state: &RecoveryState, headers: &HeaderMap) -> ApiResult<()> {
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(ApiError::forbidden());
    }
    auth::check_account(state.account(), headers, "admin", true)?;
    Ok(())
}
async fn identity(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    machine(&state, &headers)?;
    Ok(Json(
        json!({"recovery":true,"id":state.identity.installation_id,"transaction_id":state.identity.transaction_id,"listener_nonce":state.identity.listener_nonce,"pid":state.identity.pid}),
    ))
}
async fn release(
    State(state): State<Arc<RecoveryState>>,
    headers: HeaderMap,
    Json(input): Json<Release>,
) -> ApiResult<Json<Value>> {
    machine(&state, &headers)?;
    control::authorize_release(&state.home, &state.identity.transaction_id, &input)?;
    let mut release = state.release.lock().unwrap();
    if let Some(previous) = release.as_ref() {
        if previous.operation_id != input.operation_id
            || previous.attempt_id != input.attempt_id
            || !auth::constant_equal(&previous.nonce, &input.nonce)
        {
            return Err(ApiError::forbidden());
        }
    } else {
        *release = Some(input);
    }
    state.shutdown.send_replace(true);
    Ok(Json(json!({"releasing":true})))
}

fn saved_origin(home: &Path, config: &Config) -> Result<(String, u16)> {
    let port = if config.port != 0 {
        config.port
    } else {
        let runtime: Value = serde_json::from_slice(&fs::read(home.join("runtime.json"))?)?;
        u16::try_from(
            runtime["port"]
                .as_u64()
                .context("Missing retained listener port")?,
        )?
    };
    ensure!(port != 0, "Recovery requires the retained listener port");
    let origin = if config.public_url.is_empty() {
        format!("https://127.0.0.1:{port}")
    } else {
        config.public_url.trim_end_matches('/').to_owned()
    };
    authority(&origin)?;
    Ok((origin, port))
}
fn page_target(origin: &str, transport: Transport) -> Result<String> {
    authority(origin)?;
    if transport == Transport::Https {
        amplifier_native_platform::validate_https_origin(origin)?;
        Ok(origin.to_owned())
    } else {
        Ok(format!("{origin}/updates"))
    }
}
fn open_page(origin: &str, config: &Config, browser: bool) -> Result<()> {
    if browser {
        let url = page_target(origin, config.transport)?;
        if config.transport == Transport::LocalHttp {
            amplifier_native_platform::open_browser(&url)?;
        } else {
            amplifier_native_platform::open_https_browser(&url)?;
        }
    }
    Ok(())
}
/// No ordinary runtime receipt or health endpoint is written by this listener.
pub async fn serve(home: PathBuf, browser: bool) -> Result<()> {
    let startup = native_update::lock(&home, control::WORKER_LOCK)?;
    let service = match native_update::lock(&home, "service.lock") {
        Ok(service) => service,
        Err(error) => {
            drop(startup);
            return reopen(&home, browser).await.context(error);
        }
    };
    let journal =
        native_update::recovery_snapshot(&home)?.context("No retained native transaction")?;
    ensure!(
        journal.phase != "restored",
        "The retained transaction is already settled"
    );
    let package = native_update::recovery_package(&home)?;
    ensure!(
        dunce::canonicalize(std::env::current_exe()?)?
            == dunce::canonicalize(native_update::verify_helper_package(&package)?)?,
        "Recovery must run from its verified immutable package"
    );
    let config = Config::load(&home)?;
    let (origin, port) = saved_origin(&home, &config)?;
    let identity = Receipt {
        schema: 1,
        installation_id: config.id.clone(),
        transaction_id: journal.transaction,
        listener_nonce: auth::random_token(),
        state: "listening".into(),
        operation_id: None,
        attempt_id: None,
        pid: std::process::id(),
        port,
    };
    let state = RecoveryState::open(home.clone(), identity.clone(), origin.clone())?;
    let listener = std::net::TcpListener::bind((config.bind.as_str(), port))?;
    listener.set_nonblocking(true)?;
    let tls = if config.transport == Transport::LocalHttp {
        None
    } else {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Some(
            axum_server::tls_rustls::RustlsConfig::from_pem_file(&config.tls_cert, &config.tls_key)
                .await?,
        )
    };
    save_receipt(&home, &identity)?;
    drop(startup);
    open_page(&origin, &config, browser)?;
    serve_bound(state, listener, tls, service, axum_server::Handle::new()).await
}
async fn serve_bound(
    state: Arc<RecoveryState>,
    listener: std::net::TcpListener,
    tls: Option<axum_server::tls_rustls::RustlsConfig>,
    service: native_update::InstallationLock,
    handle: axum_server::Handle<std::net::SocketAddr>,
) -> Result<()> {
    let shutdown = handle.clone();
    let mut signal = state.shutdown.subscribe();
    let waiter = tokio::spawn(async move {
        if signal.changed().await.is_ok() {
            shutdown.graceful_shutdown(Some(Duration::from_secs(3)));
        }
    });
    let result = if let Some(tls) = tls {
        axum_server::from_tcp_rustls(listener, tls.clone())?
            .acceptor(
                axum_server::tls_rustls::RustlsAcceptor::new(tls)
                    .handshake_timeout(Duration::from_secs(5)),
            )
            .handle(handle.clone())
            .serve(router(state.clone()).into_make_service())
            .await
    } else {
        axum_server::from_tcp(listener)?
            .handle(handle.clone())
            .serve(router(state.clone()).into_make_service())
            .await
    };
    waiter.abort();
    // axum-server's graceful timeout only signals its connection tasks; serve()
    // can return while an accepted TLS handshake still owns a socket. Verify
    // the tracked count reaches zero before publishing any release receipt.
    handle.shutdown();
    let drained = tokio::time::timeout(Duration::from_secs(5), async {
        while handle.connection_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if drained.is_err() {
        // Fail closed until this recovery process exits. Dropping the descriptor
        // here would let normal startup pass while an unproved socket remains.
        std::mem::forget(service);
        anyhow::bail!("Recovery connections did not close; handoff remains unknown");
    }
    result?;
    if let Some(release) = state.release.lock().unwrap().as_ref() {
        let mut released = state.identity.clone();
        released.state = "released".into();
        released.operation_id = Some(release.operation_id.clone());
        released.attempt_id = Some(release.attempt_id.clone());
        save_receipt(&state.home, &released)?;
    }
    drop(service);
    Ok(())
}
async fn reopen(home: &Path, browser: bool) -> Result<()> {
    if let Ok(status) = native_update::local_api(home, "GET", "status", None).await
        && native_update::verify_retained_listener(home, &status).is_ok()
    {
        let config = Config::load(home)?;
        return open_page(&saved_origin(home, &config)?.0, &config, browser);
    }
    let saved = receipt(home)?.context("No verified recovery listener")?;
    ensure!(
        saved.state == "listening",
        "Recovery listener has not retained its listening identity"
    );
    let value = native_update::request_path(home, "GET", IDENTITY_PATH, None).await?;
    ensure!(
        value["recovery"] == true
            && value["id"] == saved.installation_id
            && value["transaction_id"] == saved.transaction_id
            && value["listener_nonce"] == saved.listener_nonce
            && value["pid"] == saved.pid,
        "Recovery listener identity changed"
    );
    let config = Config::load(home)?;
    open_page(&saved_origin(home, &config)?.0, &config, browser)
}

pub(crate) async fn yield_listener(home: &Path, transaction: &str, release: Release) -> Result<()> {
    let Some(saved) = receipt(home)?
        .filter(|value| value.transaction_id == transaction && value.state == "listening")
    else {
        return Ok(());
    };
    // Holding the worker lock prevents a new recovery server from starting.
    // A stale listening receipt with a free service lock is simply crash evidence.
    if let Ok(guard) = native_update::lock(home, "service.lock") {
        drop(guard);
        return Ok(());
    }
    let reply = native_update::request_path(
        home,
        "POST",
        RELEASE_PATH,
        Some(serde_json::to_value(&release)?),
    )
    .await?;
    ensure!(
        reply["releasing"] == true,
        "Recovery listener did not accept the exact handoff"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = receipt(home)?
            && value.state == "released"
            && value.listener_nonce == saved.listener_nonce
            && value.operation_id.as_deref() == Some(&release.operation_id)
            && value.attempt_id.as_deref() == Some(&release.attempt_id)
            && let Ok(guard) = native_update::lock(home, "service.lock")
        {
            drop(guard);
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Recovery listener did not release its owned port and lock"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    fn identity(config: &Config) -> Receipt {
        Receipt {
            schema: 1,
            installation_id: config.id.clone(),
            transaction_id: uuid::Uuid::new_v4().to_string(),
            listener_nonce: auth::random_token(),
            state: "listening".into(),
            operation_id: None,
            attempt_id: None,
            pid: std::process::id(),
            port: config.port,
        }
    }
    fn headers(config: &Config) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            config
                .public_url
                .trim_start_matches("http://")
                .parse()
                .unwrap(),
        );
        headers.insert(header::ORIGIN, config.public_url.parse().unwrap());
        headers
    }
    async fn fixture() -> (tempfile::TempDir, Arc<RecoveryState>) {
        let dir = tempfile::tempdir().unwrap();
        Config::local(dir.path(), "Recovery fixture", 54127)
            .unwrap()
            .save(dir.path())
            .unwrap();
        let app = crate::state::AppState::open(dir.path().to_owned()).unwrap();
        let token = auth::prepare_first_account(dir.path()).unwrap().unwrap();
        auth::first_account(State(app.clone()),headers(&app.config),Json(serde_json::from_value(json!({"setup_token":token,"password":"recovery fixture password","start_at_signin":false})).unwrap())).await.unwrap();
        app.db.lock().unwrap().execute_batch("CREATE TABLE recovery_sentinel(kind TEXT,value TEXT); INSERT INTO recovery_sentinel VALUES('work','working'),('reply','streaming'),('receipt','unknown'),('grant','consumed'),('budget','remaining'); WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<5005) INSERT INTO audit(at,kind,conversation_id,data) SELECT 1,'retained',NULL,'{}' FROM n;").unwrap();
        let config = app.config.clone();
        drop(app);
        let state = RecoveryState::open(
            dir.path().to_owned(),
            identity(&config),
            config.public_url.clone(),
        )
        .unwrap();
        (dir, state)
    }
    fn protected(db: &Connection) -> Vec<String> {
        let mut result = Vec::new();
        let mut statement = db
            .prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<(String, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (name, schema) in tables {
            result.push(schema);
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
            let count = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                result.push(format!(
                    "{name}:{:?}",
                    (0..count)
                        .map(|i| row.get::<_, rusqlite::types::Value>(i).unwrap())
                        .collect::<Vec<_>>()
                ));
            }
        }
        result
    }
    #[test]
    fn missing_database_is_never_created_and_https_opens_validated_root() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::local(dir.path(), "Fixture", 54127).unwrap();
        config.save(dir.path()).unwrap();
        assert!(
            RecoveryState::open(
                dir.path().to_owned(),
                identity(&config),
                config.public_url.clone()
            )
            .is_err()
        );
        assert!(!dir.path().join("state.sqlite3").exists());
        assert_eq!(
            page_target("https://127.0.0.1:54127", Transport::Https).unwrap(),
            "https://127.0.0.1:54127"
        );
        assert!(page_target("https://127.0.0.1:54127/updates", Transport::Https).is_err());
        assert_eq!(
            page_target("http://127.0.0.1:54127", Transport::LocalHttp).unwrap(),
            "http://127.0.0.1:54127/updates"
        );
    }
    #[tokio::test]
    async fn recovery_auth_changes_only_sessions_attempts_and_append_only_audit() {
        let (dir, state) = fixture().await;
        let before = protected(&state.db.lock().unwrap());
        let config = fs::read(dir.path().join("config.json")).unwrap();
        let token = fs::read(dir.path().join("secrets/control-token")).unwrap();
        let audit: i64 = state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
            .unwrap();
        let response = login(
            State(state.clone()),
            headers(&state.config),
            Json(serde_json::from_value(json!({"password":"recovery fixture password"})).unwrap()),
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
        assert_eq!(
            session(State(state.clone()), signed.clone())
                .await
                .unwrap()
                .0["authenticated"],
            true
        );
        logout(State(state.clone()), signed.clone()).await.unwrap();
        assert_eq!(
            session(State(state.clone()), signed).await.unwrap().0["authenticated"],
            false
        );
        assert!(
            login(
                State(state.clone()),
                headers(&state.config),
                Json(serde_json::from_value(json!({"password":"wrong password"})).unwrap())
            )
            .await
            .is_err()
        );
        assert_eq!(protected(&state.db.lock().unwrap()), before);
        assert_eq!(fs::read(dir.path().join("config.json")).unwrap(), config);
        assert_eq!(
            fs::read(dir.path().join("secrets/control-token")).unwrap(),
            token
        );
        assert_eq!(
            state
                .db
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            audit + 3
        );
        assert!(!dir.path().join("runtime.json").exists());
    }
    async fn request(
        state: Arc<RecoveryState>,
        method: &str,
        path: &str,
        headers: HeaderMap,
        body: &str,
    ) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::from(body.to_owned()))
            .unwrap();
        *request.headers_mut() = headers;
        router(state).oneshot(request).await.unwrap()
    }
    #[tokio::test]
    async fn restricted_routes_headers_and_host_origin_csrf_authority() {
        let (_dir, state) = fixture().await;
        let h = headers(&state.config);
        let response = request(state.clone(), "GET", "/updates", h.clone(), "").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(
            response.headers()["permissions-policy"]
                .to_str()
                .unwrap()
                .contains("microphone=()")
        );
        assert!(
            response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'self'")
        );
        for path in [
            "/health",
            "/api/v1/native-update/status",
            "/api/v1/companion",
            "/api/v1/companion/setup",
            "/api/v1/companion/update/check",
            "/api/v1/companion/update/apply",
            "/auth/first-account",
        ] {
            assert_eq!(
                request(state.clone(), "POST", path, h.clone(), "{}")
                    .await
                    .status(),
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        let mut hostile = h.clone();
        hostile.insert(header::HOST, "evil.example".parse().unwrap());
        assert_eq!(
            request(state.clone(), "GET", "/updates", hostile, "")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut hostile = h.clone();
        hostile.insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert_eq!(
            request(state.clone(), "GET", "/updates", hostile, "")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut duplicate = h.clone();
        duplicate.append(header::HOST, "127.0.0.1:54127".parse().unwrap());
        assert_eq!(
            request(state.clone(), "GET", "/updates", duplicate, "")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut h2 = h.clone();
        h2.remove(header::HOST);
        assert_eq!(
            request(
                state.clone(),
                "GET",
                "http://127.0.0.1:54127/updates",
                h2,
                ""
            )
            .await
            .status(),
            StatusCode::OK
        );
        let response = login(
            State(state.clone()),
            h.clone(),
            Json(serde_json::from_value(json!({"password":"recovery fixture password"})).unwrap()),
        )
        .await
        .unwrap();
        let mut signed = h;
        signed.insert(
            header::COOKIE,
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .parse()
                .unwrap(),
        );
        signed.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let payload=json!({"request_id":uuid::Uuid::new_v4().to_string(),"operation_id":uuid::Uuid::new_v4().to_string(),"action":"resume"}).to_string();
        assert_eq!(
            request(
                state.clone(),
                "POST",
                "/api/v1/companion/update/recover",
                signed.clone(),
                &payload
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(request(state.clone(),"POST",RELEASE_PATH,signed,&json!({"operation_id":uuid::Uuid::new_v4().to_string(),"attempt_id":uuid::Uuid::new_v4().to_string(),"nonce":"private"}).to_string()).await.status(),StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn recovery_closes_connections_and_listener_before_releasing_service_lock() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (dir, mut state) = fixture().await;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        Arc::get_mut(&mut state).unwrap().identity.port = address.port();
        save_receipt(dir.path(), &state.identity).unwrap();
        let service = native_update::lock(dir.path(), "service.lock").unwrap();
        let run = tokio::spawn(serve_bound(
            state.clone(),
            listener,
            None,
            service,
            axum_server::Handle::new(),
        ));
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection
            .write_all(
                b"GET /updates HTTP/1.1\r\nHost: 127.0.0.1:54127\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        let mut bytes = [0; 32768];
        let count = tokio::time::timeout(Duration::from_secs(2), connection.read(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&bytes[..count])
                .unwrap()
                .contains("200 OK")
        );
        assert!(native_update::lock(dir.path(), "service.lock").is_err());
        assert!(std::net::TcpListener::bind(address).is_err());
        // A partial next request keeps a connection alive across the handoff.
        connection
            .write_all(b"GET /updates HTTP/1.1\r\n")
            .await
            .unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let attempt = uuid::Uuid::new_v4().to_string();
        *state.release.lock().unwrap() = Some(Release {
            operation_id: operation.clone(),
            attempt_id: attempt.clone(),
            nonce: auth::random_token(),
        });
        state.shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(6), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let saved = receipt(dir.path()).unwrap().unwrap();
        assert_eq!(saved.state, "released");
        assert_eq!(saved.operation_id.as_deref(), Some(operation.as_str()));
        assert_eq!(saved.attempt_id.as_deref(), Some(attempt.as_str()));
        let mut remaining = Vec::new();
        // EOF or reset proves the old connection cannot execute a late request.
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            connection.read_to_end(&mut remaining),
        )
        .await
        .unwrap();
        assert!(
            result.is_ok() || result.unwrap_err().kind() == std::io::ErrorKind::ConnectionReset
        );
        let _service = native_update::lock(dir.path(), "service.lock").unwrap();
        let _listener = std::net::TcpListener::bind(address).unwrap();
        // A worker crash after handoff leaves authentication intact, allowing a
        // new restricted state to open without any application initialization.
        let reopened = RecoveryState::open(
            dir.path().to_owned(),
            identity(&state.config),
            state.origin.clone(),
        )
        .unwrap();
        assert_eq!(
            protected(&reopened.db.lock().unwrap()),
            protected(&state.db.lock().unwrap())
        );
        assert!(!dir.path().join("runtime.json").exists());
    }
    #[tokio::test]
    async fn incomplete_tls_handshake_blocks_release_after_server_graceful_return() {
        use tokio::io::AsyncWriteExt;
        let (dir, state) = fixture().await;
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            cert.cert.pem().into_bytes(),
            cert.signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        save_receipt(dir.path(), &state.identity).unwrap();
        let service = native_update::lock(dir.path(), "service.lock").unwrap();
        let handle = axum_server::Handle::new();
        let run = tokio::spawn(serve_bound(
            state.clone(),
            listener,
            Some(tls),
            service,
            handle.clone(),
        ));
        let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
        connection.write_all(&[0x16, 0x03]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while handle.connection_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        *state.release.lock().unwrap() = Some(Release {
            operation_id: uuid::Uuid::new_v4().to_string(),
            attempt_id: uuid::Uuid::new_v4().to_string(),
            nonce: auth::random_token(),
        });
        // This forces axum-server's own wait_connections_end to return while
        // the accepted TLS handshake remains pending outside its shutdown select.
        handle.graceful_shutdown(Some(Duration::ZERO));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(handle.connection_count(), 1);
        assert!(!run.is_finished());
        assert_eq!(receipt(dir.path()).unwrap().unwrap().state, "listening");
        assert!(native_update::lock(dir.path(), "service.lock").is_err());
        drop(connection);
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(handle.connection_count(), 0);
        assert_eq!(receipt(dir.path()).unwrap().unwrap().state, "released");
        assert!(native_update::lock(dir.path(), "service.lock").is_ok());
    }
}
