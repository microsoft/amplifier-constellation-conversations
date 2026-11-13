//! Installation-owned desktop entrypoint. Opening the app never selects an
//! agent, starts a provider call, or creates a second update owner.
use crate::{
    auth,
    config::{AuthMode, Config, Transport},
    native_bundle, native_update,
    state::AppState,
};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const PROOF_DOMAIN: &str = "constellation.companion.local-listener.v1";

fn file_digest(path: &Path) -> Result<String> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "Expected an ordinary application file"
    );
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Both the ordinary archive and the Mac application carry the same verified
/// immutable bundle. The Mac executable is a byte-identical native launcher.
fn downloaded_bundle(executable: &Path) -> Result<PathBuf> {
    let parent = executable.parent().context("Missing application folder")?;
    let bundle = if cfg!(target_os = "macos") && parent.file_name().is_some_and(|v| v == "MacOS") {
        parent
            .parent()
            .context("Missing application resources")?
            .join("Resources/bundle")
    } else {
        parent.to_owned()
    };
    let verified = native_bundle::verify(&bundle)
        .context("Open the complete Companion download with its bundled runtime")?;
    let service = Path::new(
        verified["entrypoints"]["service"]
            .as_str()
            .context("Missing bundled application")?,
    );
    ensure!(
        file_digest(service)? == file_digest(executable)?,
        "Application launcher differs from its verified bundle"
    );
    Ok(bundle)
}

/// The browser selects a complete extracted package folder. On macOS the
/// ordinary app itself is that folder; its fixed native launcher must still
/// equal the fully verified Resources bundle service.
pub(crate) fn checked_package_folder(folder: &Path) -> Result<PathBuf> {
    ensure!(
        folder.is_absolute() && folder.is_dir(),
        "Choose an absolute extracted package folder"
    );
    let executable = if cfg!(target_os = "macos")
        && folder
            .extension()
            .is_some_and(|extension| extension == "app")
    {
        folder.join("Contents/MacOS/agent-conversations")
    } else {
        folder.join(crate::platform::executable("agent-conversations"))
    };
    downloaded_bundle(&executable)
}

/// Publish only a new private identity. Package installation follows publication
/// so immutable receipts always contain final home paths. If bootstrap is
/// interrupted, the saved identity remains and the existing bootstrap can resume.
pub fn create(home: &Path, port: u16) -> Result<()> {
    ensure!(
        !home.exists() && !home.is_symlink(),
        "This folder already exists; its files were preserved"
    );
    let parent = home.parent().context("Missing installation parent")?;
    fs::create_dir_all(parent)?;
    let stage = parent.join(format!(".companion-setup-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        platform::private_dir(&stage)?;
        let config = Config::local(home, "Companion", port)?;
        config.save(&stage)?;
        drop(AppState::open(stage.clone())?);
        auth::prepare_first_account(&stage)?;
        platform::publish_directory_if_absent(&stage, home)
    })();
    if result.is_err() && stage.exists() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

pub async fn launch(home: PathBuf, open_browser: bool) -> Result<()> {
    if !home.join("config.json").exists() {
        // Validate the supplied application before publishing any new identity.
        downloaded_bundle(&std::env::current_exe()?)?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        create(&home, port)?;
        drop(listener);
    }
    let home = dunce::canonicalize(home)?;
    let config = Config::load(&home)?;
    // A retained switch may intentionally have binary B with receipt A. Its
    // restricted recovery helper is verified from the immutable journal package,
    // before normal selected-installation validation or any AppState open.
    if native_update::recovery_snapshot(&home)?.is_some_and(|j| j.phase != "restored") {
        let package = native_update::recovery_package(&home)?;
        let helper = native_update::verify_helper_package(&package)?;
        if dunce::canonicalize(std::env::current_exe()?)? != dunce::canonicalize(&helper)? {
            let mut command = tokio::process::Command::new(helper);
            command.arg("--home").arg(&home);
            if !open_browser {
                command.arg("--no-browser");
            }
            ensure!(
                command.status().await?.success(),
                "The retained recovery application could not open"
            );
            return Ok(());
        }
        return crate::native_recovery::serve(home, open_browser).await;
    }
    let service = native_update::lock(&home, "service.lock");
    if service.is_err() {
        // A lock error is never permission to start another service. Reopening
        // requires the actual authenticated process, not a stale runtime file.
        let status = native_update::local_api(&home, "GET", "status", None).await
            .context("The running Companion could not be verified; no unverified local listener received a credential")?;
        verify_running_identity(&home, &config, &status)?;
        return ready(&home, &config, open_browser);
    }
    drop(service);
    if !home.join("native-installation.json").exists() {
        let bundle = downloaded_bundle(&std::env::current_exe()?)?;
        native_update::bootstrap(&home, &bundle)?;
    }
    // Validate the immutable selected bundle and installed executable first.
    // Reopening a downloaded older app must not replace the selected version.
    native_update::verify_installed(&home)?;
    let status = native_update::status(&home)?;
    ensure!(
        status["transaction_id"].is_null() || status["phase"] == "restored",
        "Finish the retained native update before reopening Companion"
    );
    let selected = platform::installed_binary(&native_bundle::APPLICATION, &home);
    if dunce::canonicalize(std::env::current_exe()?)? != dunce::canonicalize(&selected)? {
        let mut command = tokio::process::Command::new(&selected);
        command.arg("--home").arg(&home);
        if !open_browser {
            command.arg("--no-browser");
        }
        let status = command
            .status()
            .await
            .context("Could not open the installed Companion version")?;
        ensure!(status.success(), "The installed Companion could not start");
        return Ok(());
    }
    if config.auth_mode == AuthMode::LocalPassword {
        auth::prepare_first_account(&home)?;
    }
    crate::server::serve_with_browser(home, open_browser).await
}

fn verify_running_identity(home: &Path, config: &Config, status: &Value) -> Result<()> {
    let runtime: Value = serde_json::from_slice(&fs::read(home.join("runtime.json"))?)?;
    let process_nonce = runtime["process_nonce"]
        .as_str()
        .filter(|v| v.len() == 64)
        .context("Missing current listener identity")?;
    let pid = runtime["pid"]
        .as_u64()
        .filter(|v| (1..=u32::MAX as u64).contains(v))
        .context("Missing current listener process")?;
    ensure!(
        status["id"] == config.id
            && status["pid"] == pid
            && status["process_nonce"] == process_nonce,
        "The saved listener is not the current Companion process"
    );
    ensure!(
        config.transport != Transport::LocalHttp || home.join("native-installation.json").is_file(),
        "The local Companion has no verified installed bundle"
    );
    if home.join("native-installation.json").exists() {
        let installed = native_update::verify_installed(home)?;
        ensure!(
            installed["id"] == config.id
                && status["binary_sha256"] == installed["package"]["binary_sha256"],
            "The running Companion differs from the selected installation"
        );
    }
    Ok(())
}

fn ready_url(origin: &str, transport: Transport, setup_token: Option<&str>) -> Result<String> {
    if transport == Transport::Https {
        platform::validate_https_origin(origin)?;
        ensure!(
            setup_token.is_none(),
            "HTTPS does not use first-account setup"
        );
        return Ok(origin.to_owned());
    }
    let mut url = format!("{}/companion", origin.trim_end_matches('/'));
    if let Some(token) = setup_token {
        url.push_str("#setup=");
        url.push_str(token);
    }
    Ok(url)
}

pub fn ready(home: &Path, config: &Config, open_browser: bool) -> Result<()> {
    let runtime: Value = serde_json::from_slice(&fs::read(home.join("runtime.json"))?)?;
    let origin = if config.public_url.is_empty() {
        runtime["url"]
            .as_str()
            .context("Missing current listener address")?
    } else {
        &config.public_url
    };
    let setup_token = if open_browser && config.auth_mode == AuthMode::LocalPassword {
        auth::prepare_first_account(home)?
    } else {
        None
    };
    let url = ready_url(origin, config.transport, setup_token.as_deref())?;
    if open_browser {
        if config.transport == Transport::LocalHttp {
            platform::open_browser(&url)?;
        } else {
            platform::open_https_browser(&url)?;
        }
    } else {
        // Headless qualification reads the private capability itself. Never put
        // it into public readiness output, logs, command arguments or receipts.
        println!("{}", json!({"ready":true,"id":config.id,"url":url}));
    }
    Ok(())
}

/// The caller's exact Host/Origin gate runs before this unauthenticated route.
/// Proof establishes only possession; it grants no caller authority.
pub fn listener_proof(home: &Path, config: &Config, nonce: &str) -> Result<Value> {
    ensure!(
        config.transport == Transport::LocalHttp,
        "Local proof is unavailable for this installation"
    );
    let token = fs::read_to_string(home.join("secrets/control-token"))?;
    let proof = platform::local_auth::listener_proof(
        PROOF_DOMAIN,
        &config.id,
        &config.public_url,
        nonce,
        token.trim(),
    )?;
    Ok(json!({"id":config.id,"origin":config.public_url,"nonce":nonce,"proof":proof}))
}

/// Proof and bearer dispatch share one TCP connection, without a pool, redirect
/// or reconnect that could substitute another loopback process after proof.
pub async fn proved_local_api(
    config: &Config,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    use axum::{body::Bytes, http::header};
    use http_body_util::{BodyExt, Full, Limited};
    use hyper_util::rt::TokioIo;
    ensure!(
        config.transport == Transport::LocalHttp,
        "Expected local HTTP transport"
    );
    config.validate()?;
    ensure!(
        (path.starts_with("/api/") || path == "/context/v1/query")
            && !path.contains("..")
            && !path.contains('#'),
        "Use a fixed local API path"
    );
    struct Connection(tokio::task::JoinHandle<()>);
    impl Drop for Connection {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let stream = tokio::net::TcpStream::connect((config.bind.as_str(), config.port)).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream)).await?;
    let _connection = Connection(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let host = config
        .public_url
        .strip_prefix("http://")
        .context("Invalid saved local origin")?;
    let nonce = auth::random_token();
    let proof_request = hyper::Request::builder()
        .method("POST")
        .uri("/auth/local-listener-proof")
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(serde_json::to_vec(
            &json!({"nonce":nonce}),
        )?)))?;
    let response = sender.send_request(proof_request).await?;
    ensure!(
        response.status().is_success(),
        "Local installation proof was refused"
    );
    let bytes = Limited::new(response.into_body(), 4096)
        .collect()
        .await
        .map_err(|_| anyhow::anyhow!("Invalid local installation proof response"))?
        .to_bytes();
    let proof: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        proof["id"] == config.id && proof["origin"] == config.public_url && proof["nonce"] == nonce,
        "A different installation answered at the saved address"
    );
    platform::local_auth::verify_listener_proof(
        PROOF_DOMAIN,
        &config.id,
        &config.public_url,
        &nonce,
        token,
        proof["proof"]
            .as_str()
            .context("Missing local installation proof")?,
    )?;
    // Consuming the response body does not promise that Hyper's dispatcher
    // has completed its handoff. Wait on this exact proved connection; a
    // closed connection fails here without reconnecting or sending a bearer.
    sender.ready().await?;
    let mut request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, host)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let body = if let Some(body) = body {
        request = request.header(header::CONTENT_TYPE, "application/json");
        serde_json::to_vec(&body)?
    } else {
        vec![]
    };
    let response = sender
        .send_request(request.body(Full::new(Bytes::from(body)))?)
        .await?;
    let status = response.status();
    let bytes = Limited::new(response.into_body(), 16 * 1024 * 1024)
        .collect()
        .await
        .map_err(|_| anyhow::anyhow!("Invalid or excessive local API response"))?
        .to_bytes();
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        status.is_success(),
        "Local API request was not accepted ({status})"
    );
    Ok(value)
}

pub async fn local_api(
    home: &Path,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let config = Config::load(home)?;
    let token = fs::read_to_string(home.join("secrets/control-token"))?;
    tokio::time::timeout(
        Duration::from_secs(15),
        proved_local_api(&config, token.trim(), method, path, body),
    )
    .await
    .context("Local Companion did not respond in time")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn ready_url_preserves_https_origin_and_local_setup_fragment() {
        for origin in [
            "https://companion.example:9443",
            "https://companion.example:9443/",
        ] {
            let url = ready_url(origin, Transport::Https, None).unwrap();
            assert_eq!(url, origin);
            platform::validate_https_origin(&url).unwrap();
        }
        assert!(
            ready_url(
                "https://companion.example/companion",
                Transport::Https,
                None
            )
            .is_err()
        );
        assert!(ready_url("https://companion.example", Transport::Https, Some("token")).is_err());
        let origin = "http://127.0.0.1:31471";
        let plain = ready_url(origin, Transport::LocalHttp, None).unwrap();
        assert_eq!(plain, "http://127.0.0.1:31471/companion");
        platform::validate_local_url(&plain).unwrap();
        let token = "a".repeat(64);
        let setup = ready_url(origin, Transport::LocalHttp, Some(&token)).unwrap();
        assert_eq!(
            setup,
            format!("http://127.0.0.1:31471/companion#setup={token}")
        );
        platform::validate_local_url(&setup).unwrap();
    }

    #[test]
    fn atomic_new_home_has_no_agent_and_never_replaces_an_existing_identity() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("Companion café 日本語");
        create(&home, 31471).unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(config.transport, Transport::LocalHttp);
        assert!(config.agent.is_none());
        assert!(!home.join("tls").exists());
        assert!(!home.join("companion.json").exists());
        assert!(!home.join("runtime.json").exists());
        let state = AppState::open(home.clone()).unwrap();
        assert_eq!(
            state
                .db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM agent_endpoints", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(state);
        let before = fs::read(home.join("config.json")).unwrap();
        let capability = auth::prepare_first_account(&home).unwrap();
        assert!(create(&home, 31472).is_err());
        assert_eq!(fs::read(home.join("config.json")).unwrap(), before);
        assert_eq!(auth::prepare_first_account(&home).unwrap(), capability);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        let occupied = root.path().join("preexisting");
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("retained"), b"owner data").unwrap();
        assert!(create(&occupied, 31472).is_err());
        assert_eq!(fs::read(occupied.join("retained")).unwrap(), b"owner data");
        assert!(!occupied.join("config.json").exists());
    }

    #[test]
    fn listener_proof_binds_component_identity_origin_and_challenge() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        create(&home, 31473).unwrap();
        let config = Config::load(&home).unwrap();
        let token = fs::read_to_string(home.join("secrets/control-token")).unwrap();
        let nonce = auth::random_token();
        let response = listener_proof(&home, &config, &nonce).unwrap();
        assert_eq!(response["id"], config.id);
        assert_eq!(response["origin"], config.public_url);
        assert_eq!(response["nonce"], nonce);
        let proof = response["proof"].as_str().unwrap();
        platform::local_auth::verify_listener_proof(
            PROOF_DOMAIN,
            &config.id,
            &config.public_url,
            &nonce,
            token.trim(),
            proof,
        )
        .unwrap();
        for (domain, id, origin, nonce) in [
            (
                "another-component",
                config.id.as_str(),
                config.public_url.as_str(),
                nonce.as_str(),
            ),
            (
                PROOF_DOMAIN,
                "another-installation",
                config.public_url.as_str(),
                nonce.as_str(),
            ),
            (
                PROOF_DOMAIN,
                config.id.as_str(),
                "http://127.0.0.1:31474",
                nonce.as_str(),
            ),
            (
                PROOF_DOMAIN,
                config.id.as_str(),
                config.public_url.as_str(),
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
        ] {
            assert!(
                platform::local_auth::verify_listener_proof(
                    domain,
                    id,
                    origin,
                    nonce,
                    token.trim(),
                    proof
                )
                .is_err()
            );
        }
        assert!(!response.to_string().contains(token.trim()));
        assert!(listener_proof(&home, &config, "not-a-challenge").is_err());
    }

    async fn request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
        let mut bytes = Vec::new();
        let boundary = loop {
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
            bytes.push(byte[0]);
            assert!(bytes.len() < 16384);
            if bytes.ends_with(b"\r\n\r\n") {
                break bytes.len();
            }
        };
        let header = String::from_utf8(bytes[..boundary].to_vec()).unwrap();
        let length = header
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, length)| length.trim().parse::<usize>().unwrap())
            .unwrap_or(0);
        assert!(length < 4096);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        (
            header,
            if body.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&body).unwrap()
            },
        )
    }

    async fn response(stream: &mut tokio::net::TcpStream, value: Value) {
        let body = serde_json::to_string(&value).unwrap();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fake_listener_receives_no_bearer_and_valid_proof_reuses_exact_connection() {
        // Exercise the proof-to-bearer handoff while Hyper's dispatcher runs
        // independently; a single-thread runtime can hide readiness races.
        for valid in std::iter::once(false).chain(std::iter::repeat_n(true, 64)) {
            tokio::time::timeout(Duration::from_secs(5), async {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let root = tempfile::tempdir().unwrap();
                let config = Config::local(root.path(), "Connection test", listener.local_addr().unwrap().port()).unwrap();
                let server_config = config.clone();
                let token = "fixture-control-token-never-send-before-proof";
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (header, body) = request(&mut stream).await;
                    assert!(header.starts_with("POST /auth/local-listener-proof "));
                    assert!(!header.to_ascii_lowercase().contains("authorization:"));
                    assert!(!header.contains(token) && !body.to_string().contains(token));
                    let nonce = body["nonce"].as_str().unwrap();
                    let proof = if valid {
                        platform::local_auth::listener_proof(PROOF_DOMAIN, &server_config.id, &server_config.public_url, nonce, token).unwrap()
                    } else { "0".repeat(64) };
                    response(&mut stream, json!({"id":server_config.id,"origin":server_config.public_url,"nonce":nonce,"proof":proof})).await;
                    if valid {
                        // Accept no second connection: bearer must arrive on
                        // this exact already-proved stream.
                        let (header, _) = request(&mut stream).await;
                        assert!(header.starts_with("GET /api/v1/fixture "));
                        assert!(header.contains(&format!("Bearer {token}")));
                        response(&mut stream, json!({"ok":true})).await;
                    } else {
                        let mut tail = Vec::new();
                        stream.read_to_end(&mut tail).await.unwrap();
                        assert!(tail.is_empty(), "No authenticated request follows a false proof");
                    }
                });
                let result = proved_local_api(&config, token, "GET", "/api/v1/fixture", None).await;
                assert_eq!(result.is_ok(), valid, "{result:?}");
                if valid { assert_eq!(result.unwrap(), json!({"ok":true})); }
                server.await.unwrap();
            }).await.unwrap();
        }
    }

    #[tokio::test]
    async fn closed_proved_connection_receives_no_bearer_or_reconnect() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let root = tempfile::tempdir().unwrap();
            let config = Config::local(root.path(), "Closed connection", listener.local_addr().unwrap().port()).unwrap();
            let server_config = config.clone();
            let token = "fixture-control-token-never-reconnect";
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = request(&mut stream).await;
                assert!(!headers.to_ascii_lowercase().contains("authorization:"));
                let nonce = body["nonce"].as_str().unwrap();
                let proof = platform::local_auth::listener_proof(PROOF_DOMAIN, &server_config.id, &server_config.public_url, nonce, token).unwrap();
                let body = json!({"id":server_config.id,"origin":server_config.public_url,"nonce":nonce,"proof":proof}).to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
                let mut tail = Vec::new();
                stream.read_to_end(&mut tail).await.unwrap();
                assert!(tail.is_empty(), "A closing proved connection must not receive a bearer");
                // A replacement connection would have no proof reply and make
                // the enclosing bounded test fail instead of silently passing.
                listener
            });
            assert!(proved_local_api(&config, token, "GET", "/api/v1/fixture", None).await.is_err());
            let listener = server.await.unwrap();
            assert!(tokio::time::timeout(Duration::from_millis(25), listener.accept()).await.is_err());
        }).await.unwrap();
    }
}
