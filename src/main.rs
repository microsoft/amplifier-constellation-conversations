use agent_conversations::{
    config::{self, Agent, AuthMode, Config, Transport},
    credentials, desktop, driver, native_bundle, native_update, native_update_protocol, platform,
    server,
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "A standalone web conversation experience for an existing ACP agent"
)]
struct Cli {
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Start the desktop interface without opening a browser.
    #[arg(long, global = true)]
    no_browser: bool,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
// One command is parsed once at startup; boxing individual CLI fields adds no value.
#[allow(clippy::large_enum_variant)]
enum Action {
    /// Open the existing HTTPS interface in the system browser.
    Open,
    /// Configure sign-in startup for this installation without starting it now.
    Startup {
        #[command(subcommand)]
        action: StartupAction,
    },
    /// Verify or install a complete native package without changing live state.
    Bundle {
        #[command(subcommand)]
        action: BundleAction,
    },
    /// Import an installation-owned provider credential without exposing it in arguments.
    Credential {
        #[command(subcommand)]
        action: CredentialAction,
    },
    Init {
        #[arg(long, default_value = "Conversations")]
        label: String,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value = "")]
        public_url: String,
        #[arg(long, default_value_t = platform::default_auth_service().to_owned())]
        pam_service: String,
        #[arg(long)]
        agent_home: Option<PathBuf>,
        #[arg(long)]
        agent_command: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        agent_arg: Vec<String>,
        #[arg(long)]
        agent_cwd: Option<PathBuf>,
        #[arg(long, default_value = "Helper")]
        agent_label: String,
        #[arg(long)]
        tls_cert: Option<PathBuf>,
        #[arg(long)]
        tls_key: Option<PathBuf>,
        #[arg(long)]
        tls_ca: Option<PathBuf>,
    },
    Serve,
    #[command(hide = true)]
    ServeUpdate {
        transaction: String,
        nonce: String,
    },
    /// Retain, update or recover this complete installation without agent work.
    NativeUpdate {
        #[command(subcommand)]
        action: NativeUpdateAction,
    },
    /// Fixed installation-owned update-provider/lifecycle integration.
    NativeUpdateProtocol {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        mode: String,
        #[arg(long)]
        action: String,
    },
    Status,
    Probe,
    /// Manage independently scoped transcript-sharing connections.
    ImportSource {
        #[command(subcommand)]
        action: SourceAction,
    },
    /// Issue one agent's read-only conversation-source credential.
    ContextSource {
        #[command(subcommand)]
        action: ContextAction,
    },
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    Api {
        method: String,
        path: String,
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
}
#[derive(Subcommand)]
enum StartupAction {
    Install,
    Remove,
}
#[derive(Subcommand)]
enum BundleAction {
    Verify {
        #[arg(long)]
        path: PathBuf,
    },
    Install {
        #[arg(long)]
        path: PathBuf,
    },
}
#[derive(Subcommand)]
enum NativeUpdateAction {
    /// Restore the retained previous bundle through a new idle transaction.
    Rollback {
        transaction: String,
    },
    Bootstrap {
        #[arg(long)]
        package: PathBuf,
    },
    Plan {
        #[arg(long)]
        package: PathBuf,
    },
    Run {
        #[arg(long)]
        package: PathBuf,
    },
    Status,
    /// Recover a retained transaction; never redispatch conversation work.
    Recover {
        transaction: String,
        #[arg(long)]
        rollback: bool,
    },
}
#[derive(Subcommand)]
enum CredentialAction {
    /// Read one OpenAI API key from standard input; never print or test it.
    ImportOpenai,
}
#[derive(Subcommand)]
enum ConfigAction {
    Get,
    Set {
        #[arg(long)]
        file: PathBuf,
    },
}
#[derive(Subcommand)]
enum SourceAction {
    List,
    Create {
        source: String,
        #[arg(long)]
        surface: String,
        /// Save the returned credential privately; never print it.
        #[arg(long)]
        token_out: PathBuf,
    },
    Rotate {
        source: String,
        #[arg(long)]
        token_out: PathBuf,
    },
    Revoke {
        source: String,
    },
}
#[derive(Subcommand)]
enum ContextAction {
    Show {
        agent: String,
    },
    Issue {
        agent: String,
        /// Save the new credential privately; replaces any prior credential.
        #[arg(long)]
        token_out: PathBuf,
        #[arg(long, default_value_t = 90 * 86400)]
        expires_in_seconds: i64,
    },
    Revoke {
        agent: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let show_error = cli.command.is_none() && !cli.no_browser;
    if let Err(error) = run(cli).await {
        if show_error {
            amplifier_native_platform::show_launch_error(
                &native_bundle::APPLICATION,
                &error.to_string(),
            );
        }
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
async fn run(cli: Cli) -> Result<()> {
    let home = match cli.home {
        Some(home) => home,
        None => amplifier_native_platform::default_home(&native_bundle::APPLICATION, "default")?,
    };
    let Some(command) = cli.command else {
        return desktop::launch(home, !cli.no_browser).await;
    };
    match command {
        Action::Credential {
            action: CredentialAction::ImportOpenai,
        } => {
            use std::io::IsTerminal;
            Config::load(&home)?;
            let input = std::io::stdin();
            ensure!(
                !input.is_terminal(),
                "Supply the key through protected standard input; do not type it into an echoing terminal"
            );
            credentials::import_openai(&home, input.lock())?;
            println!("{}", json!({"stored":true,"provider":"openai"}));
        }
        Action::Open => {
            // No bearer or control token is placed in a URL. Authentication stays
            // in the browser's existing HTTPS/CSRF flow.
            let config = Config::load(&home)?;
            if config.transport == Transport::LocalHttp {
                return desktop::launch(home, !cli.no_browser).await;
            }
            let runtime: Value =
                serde_json::from_slice(&std::fs::read(home.join("runtime.json"))?)?;
            let url = if config.public_url.is_empty() {
                runtime["url"]
                    .as_str()
                    .context("Start this installation before opening it")?
            } else {
                &config.public_url
            };
            amplifier_native_platform::open_https_browser(url)?;
        }
        Action::Startup { action } => {
            let _owner = native_update::startup_change_guard(&home)?;
            let _activity = agent_conversations::update_admission::begin_configuration_home(&home)?;
            let config = Config::load(&home)?;
            let id = uuid::Uuid::parse_str(&config.id)?;
            match action {
                StartupAction::Install => amplifier_native_platform::install_startup(
                    &native_bundle::APPLICATION,
                    &home,
                    &id,
                )?,
                StartupAction::Remove => amplifier_native_platform::remove_startup(
                    &native_bundle::APPLICATION,
                    &home,
                    &id,
                )?,
            }
        }
        Action::Bundle { action } => {
            let receipt = match action {
                BundleAction::Verify { path } => native_bundle::verify(&path)?,
                BundleAction::Install { path } => native_bundle::install(&path, &home)?,
            };
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        }
        Action::Init {
            label,
            owner,
            bind,
            port,
            public_url,
            pam_service,
            agent_home,
            agent_command,
            agent_arg,
            agent_cwd,
            agent_label,
            tls_cert,
            tls_key,
            tls_ca,
        } => {
            ensure!(
                !home.join("config.json").exists(),
                "This installation already exists; no files were changed"
            );
            config::private_dir(&home)?;
            let home = dunce::canonicalize(&home)?;
            config::private_dir(&home.join("workspace"))?;
            let (command, args, cwd) = if let Some(agent_home) = agent_home {
                ensure!(
                    agent_command.is_none() && agent_arg.is_empty(),
                    "Use either --agent-home or --agent-command"
                );
                let p = dunce::canonicalize(agent_home)?;
                (
                    p.join("bin")
                        .join(platform::executable("agent-instance"))
                        .to_string_lossy()
                        .into_owned(),
                    vec![
                        "acp".into(),
                        "--home".into(),
                        p.to_string_lossy().into_owned(),
                    ],
                    agent_cwd.unwrap_or(p.join("workspace")),
                )
            } else {
                (
                    agent_command.context("Supply --agent-home or --agent-command")?,
                    agent_arg,
                    agent_cwd.unwrap_or(home.join("workspace")),
                )
            };
            let (cert, key, ca) = match (tls_cert, tls_key) {
                (Some(cert), Some(key)) => (
                    std::fs::canonicalize(cert)?,
                    std::fs::canonicalize(key)?,
                    tls_ca.map(std::fs::canonicalize).transpose()?,
                ),
                (None, None) => generate_tls(&home)?,
                _ => anyhow::bail!("Supply both --tls-cert and --tls-key"),
            };
            let c = Config {
                id: uuid::Uuid::new_v4().to_string(),
                auth_mode: AuthMode::OsAccount,
                transport: Transport::Https,
                label,
                owner: match owner {
                    Some(owner) => owner,
                    None => platform::current_user()?,
                },
                allowed_users: Vec::new(),
                embed_origins: Vec::new(),
                bind,
                port,
                public_url,
                pam_service,
                tls_cert: cert,
                tls_key: key,
                tls_ca: ca,
                import_source: "toad".into(),
                import_surface: "toad".into(),
                agent: Some(Agent {
                    label: agent_label,
                    command,
                    args,
                    cwd: cwd.to_string_lossy().into_owned(),
                    session_cwd: None,
                    env: BTreeMap::new(),
                    mcp_servers: Vec::new(),
                    timeout_seconds: 900,
                    client: Default::default(),
                }),
            };
            c.save(&home)?;
            let _ = AppState::open(home.clone())?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"home":home,"provider_id":c.provider_id(),"control_token_file":home.join("secrets/control-token"),"context_token_file":home.join("secrets/context-token"),"import_token_file":home.join("secrets/import-token"),"authentication":platform::auth_method(),"next":if cfg!(target_os="linux") { "Configure the local PAM service, trust the installation certificate, then run serve. No agent persona or runtime was installed." } else { "Trust the installation certificate, then run serve. Sign in using an explicitly allowed local account's password. No agent persona or runtime was installed." }})
                )?
            );
        }
        Action::Serve => server::serve(home).await?,
        Action::ServeUpdate { transaction, nonce } => {
            server::serve_update(home, transaction, nonce).await?
        }
        Action::NativeUpdate { action } => {
            let value = match action {
                NativeUpdateAction::Bootstrap { package } => {
                    native_update::bootstrap(&home, &package)?
                }
                NativeUpdateAction::Plan { package } => native_update::plan(&home, &package)?,
                NativeUpdateAction::Run { package } => native_update::run(&home, &package).await?,
                NativeUpdateAction::Status => native_update::status(&home)?,
                NativeUpdateAction::Rollback { transaction } => {
                    native_update::rollback(&home, &transaction).await?
                }
                NativeUpdateAction::Recover {
                    transaction,
                    rollback,
                } => native_update::recover(&home, &transaction, rollback).await?,
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Action::NativeUpdateProtocol {
            config,
            mode,
            action,
        } => {
            let clock = native_update_protocol::invocation_clock(&mode, &action)?;
            let value = if mode == "enrollment" {
                match action.as_str() {
                    "enroll" => native_update_protocol::claim_owner(&config)?,
                    "registration" => native_update_protocol::registration(&config)?,
                    _ => anyhow::bail!("Unknown enrollment action"),
                }
            } else {
                native_update_protocol::run(&config, &mode, &action).await?
            };
            // The watchdog remains active even if the caller stops reading.
            use std::io::Write;
            let output = serde_json::to_string_pretty(&value)? + "\n";
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(output.as_bytes())?;
            stdout.flush()?;
            clock.complete()?;
        }
        Action::Status => {
            let c = Config::load(&home)?;
            let runtime = std::fs::read(home.join("runtime.json"))
                .ok()
                .and_then(|v| serde_json::from_slice::<Value>(&v).ok());
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"label":c.label,"agent":c.agent.as_ref().map(|a|&a.label),"provider_id":c.provider_id(),"public_url":c.public_url,"last_runtime":runtime,"note":"Runtime information is the last recorded listener, not a health check."})
                )?
            );
        }
        Action::Probe => println!(
            "{}",
            serde_json::to_string_pretty(
                &driver::probe(
                    Config::load(&home)?
                        .agent
                        .as_ref()
                        .context("Select a real agent before testing its connection")?
                )
                .await?
            )?
        ),
        Action::ImportSource { action } => {
            let (method, path, body, token_out) = match action {
                SourceAction::List => ("GET", "/api/v1/import-sources".to_owned(), None, None),
                SourceAction::Create {
                    source,
                    surface,
                    token_out,
                } => (
                    "POST",
                    "/api/v1/import-sources".to_owned(),
                    Some(json!({"source":source,"surface":surface})),
                    Some(token_out),
                ),
                SourceAction::Rotate { source, token_out } => {
                    ensure!(safe_source(&source), "Invalid source name");
                    (
                        "POST",
                        format!("/api/v1/import-sources/{source}/rotate"),
                        None,
                        Some(token_out),
                    )
                }
                SourceAction::Revoke { source } => {
                    ensure!(safe_source(&source), "Invalid source name");
                    (
                        "DELETE",
                        format!("/api/v1/import-sources/{source}"),
                        None,
                        None,
                    )
                }
            };
            // Check the destination before issuing a new credential.
            if let Some(ref path) = token_out {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."));
                ensure!(
                    parent.is_dir(),
                    "Create the private credential destination folder first"
                );
                ensure!(!path.is_dir(), "The credential destination must be a file");
            }
            let mut result = api_request(&home, method, &path, body).await?;
            if let Some(path) = token_out {
                let token = result
                    .as_object_mut()
                    .context("Invalid source response")?
                    .remove("token")
                    .context("Missing source credential")?;
                config::private_write(
                    &path,
                    token
                        .as_str()
                        .context("Invalid source credential")?
                        .as_bytes(),
                )?;
                result["credential_saved_to"] = json!(path);
            }
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Action::ContextSource { action } => {
            let (agent, method, body, token_out) = match action {
                ContextAction::Show { agent } => (agent, "GET", None, None),
                ContextAction::Issue {
                    agent,
                    token_out,
                    expires_in_seconds,
                } => (
                    agent,
                    "POST",
                    Some(json!({"expires_in_seconds":expires_in_seconds})),
                    Some(token_out),
                ),
                ContextAction::Revoke { agent } => (agent, "DELETE", None, None),
            };
            ensure!(
                !agent.is_empty()
                    && agent.len() <= 80
                    && agent
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
                "Invalid agent ID"
            );
            if let Some(ref path) = token_out {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."));
                ensure!(
                    parent.is_dir(),
                    "Create the private credential destination folder first"
                );
                ensure!(!path.is_dir(), "The credential destination must be a file");
            }
            let mut result = api_request(
                &home,
                method,
                &format!("/api/v1/agents/{agent}/context-credential"),
                body,
            )
            .await?;
            if let Some(path) = token_out {
                let secret = result
                    .as_object_mut()
                    .context("Invalid source response")?
                    .remove("token")
                    .context("Missing source credential")?;
                config::private_write(
                    &path,
                    secret
                        .as_str()
                        .context("Invalid source credential")?
                        .as_bytes(),
                )?;
                result["credential_saved_to"] = json!(path);
            }
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Action::Config { action } => match action {
            ConfigAction::Get => {
                println!("{}", serde_json::to_string_pretty(&Config::load(&home)?)?)
            }
            ConfigAction::Set { file } => {
                let _activity =
                    agent_conversations::update_admission::begin_configuration_home(&home)?;
                let c: Config = serde_json::from_slice(&std::fs::read(file)?)?;
                let old = Config::load(&home)?;
                ensure!(
                    c.id == old.id && c.owner == old.owner,
                    "Instance identity and owner cannot be changed"
                );
                c.save(&home)?;
                println!("Saved. Restart this web service to apply the configuration.");
            }
        },
        Action::Api {
            method,
            path,
            body_file,
        } => {
            ensure!(
                path.starts_with("/api/") || path == "/context/v1/query",
                "Use a local API path"
            );
            ensure!(
                !path.contains("..") && !path.starts_with("//"),
                "Invalid API path"
            );
            let body = body_file
                .map(|file| -> Result<Value> { Ok(serde_json::from_slice(&std::fs::read(file)?)?) })
                .transpose()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&api_request(&home, &method, &path, body).await?)?
            );
        }
    }
    Ok(())
}
fn safe_source(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 80
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
async fn api_request(
    home: &std::path::Path,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let c = Config::load(home)?;
    if c.transport == Transport::LocalHttp {
        return desktop::local_api(home, method, path, body).await;
    }
    let r: Value = serde_json::from_slice(&std::fs::read(home.join("runtime.json"))?)?;
    let port = r["port"].as_u64().context("Start the service first")?;
    let ca = reqwest::Certificate::from_pem(&std::fs::read(c.tls_ca.unwrap_or(c.tls_cert))?)?;
    let client = reqwest::Client::builder()
        .add_root_certificate(ca)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let token = std::fs::read_to_string(home.join("secrets/control-token"))?;
    let mut req = client
        .request(method.parse()?, format!("https://127.0.0.1:{port}{path}"))
        .bearer_auth(token.trim());
    if let Some(body) = body {
        req = req.json(&body);
    }
    let response = req.send().await?;
    let status = response.status();
    let value: Value = response.json().await?;
    ensure!(
        status.is_success(),
        "API returned {status}: {}",
        value["error"].as_str().unwrap_or("Request failed")
    );
    Ok(value)
}
fn generate_tls(home: &std::path::Path) -> Result<(PathBuf, PathBuf, Option<PathBuf>)> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    use time::{Duration, OffsetDateTime};
    config::private_dir(&home.join("tls"))?;
    let now = OffsetDateTime::now_utc();
    let mut ca = CertificateParams::default();
    ca.distinguished_name
        .push(DnType::CommonName, "Agent Conversations local trust");
    ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca.not_before = now - Duration::days(1);
    ca.not_after = now + Duration::days(3650);
    let ca_key = KeyPair::generate()?;
    let root = ca.self_signed(&ca_key)?;
    config::private_write(&home.join("tls/root-ca.crt"), root.pem().as_bytes())?;
    config::private_write(
        &home.join("tls/root-ca.key"),
        ca_key.serialize_pem().as_bytes(),
    )?;
    let issuer = Issuer::new(ca, ca_key);
    let mut names = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    if let Ok(entries) = if_addrs::get_if_addrs() {
        names.extend(entries.into_iter().map(|a| a.ip().to_string()));
    }
    names.sort();
    names.dedup();
    let mut leaf = CertificateParams::new(names)?;
    leaf.distinguished_name
        .push(DnType::CommonName, "Agent Conversations");
    leaf.not_before = now - Duration::days(1);
    leaf.not_after = now + Duration::days(365);
    leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // Identify the issuing local CA for strict RFC 5280 path validation.
    leaf.use_authority_key_identifier_extension = true;
    let key = KeyPair::generate()?;
    let cert = leaf.signed_by(&key, &issuer)?;
    config::private_write(&home.join("tls/cert.pem"), cert.pem().as_bytes())?;
    config::private_write(&home.join("tls/key.pem"), key.serialize_pem().as_bytes())?;
    Ok((
        home.join("tls/cert.pem"),
        home.join("tls/key.pem"),
        Some(home.join("tls/root-ca.crt")),
    ))
}
