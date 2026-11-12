use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: String,
    pub label: String,
    pub owner: String,
    #[serde(default)]
    pub auth_mode: AuthMode,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub embed_origins: Vec<String>,
    pub bind: String,
    pub port: u16,
    pub public_url: String,
    pub pam_service: String,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,
    #[serde(default = "default_import")]
    pub import_source: String,
    #[serde(default = "default_import")]
    pub import_surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Agent>,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    OsAccount,
    LocalPassword,
}
impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OsAccount => "os_account",
            Self::LocalPassword => "local_password",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Https,
    LocalHttp,
}
fn default_import() -> String {
    "toad".into()
}
#[cfg(test)]
mod platform_tests {
    use super::*;

    #[test]
    fn local_configuration_is_explicit_loopback_and_can_start_without_an_agent() {
        let home = tempfile::tempdir().unwrap();
        let local = Config::local(home.path(), "Fresh desktop", 54127).unwrap();
        assert!(local.agent.is_none());
        assert_eq!(local.auth_mode, AuthMode::LocalPassword);
        assert_eq!(local.transport, Transport::LocalHttp);
        local.save(home.path()).unwrap();
        assert!(Config::load(home.path()).unwrap().agent.is_none());
        for (bind, url, port) in [
            ("0.0.0.0", "http://0.0.0.0:54127", 54127),
            ("127.0.0.1", "http://localhost:54127", 54127),
            ("127.0.0.1", "http://127.0.0.1:54128", 54127),
            ("127.0.0.1", "http://127.0.0.1:0", 0),
            ("127.0.0.1", "http://127.0.0.1:54127/path", 54127),
        ] {
            let mut invalid = local.clone();
            invalid.bind = bind.into();
            invalid.public_url = url.into();
            invalid.port = port;
            assert!(invalid.validate().is_err());
        }
        let mut invalid = local.clone();
        invalid.allowed_users.push("other".into());
        assert!(invalid.validate().is_err());
        invalid = local;
        invalid.auth_mode = AuthMode::OsAccount;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn legacy_serde_defaults_preserve_os_account_https_and_default_endpoint() {
        let home = tempfile::tempdir().unwrap();
        let mut value =
            serde_json::to_value(Config::local(home.path(), "Legacy", 54127).unwrap()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("auth_mode");
        object.remove("transport");
        object.insert(
            "public_url".into(),
            serde_json::json!("https://conversations.example"),
        );
        object.insert("agent".into(), serde_json::json!({"label":"Original agent","command":"fixture-command","args":[],"cwd":home.path(),"timeout_seconds":60}));
        let config: Config = serde_json::from_value(value.clone()).unwrap();
        config.validate().unwrap();
        assert_eq!(config.auth_mode, AuthMode::OsAccount);
        assert_eq!(config.transport, Transport::Https);
        assert_eq!(config.agent.as_ref().unwrap().label, "Original agent");
        value.as_object_mut().unwrap().remove("agent");
        assert!(
            serde_json::from_value::<Config>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }

    #[test]
    fn account_names_keep_platform_identity_and_reject_wildcards() {
        for invalid in ["", "*", "user\0", "a/b", "a\nb"] {
            assert!(!valid_user(invalid), "unexpected account: {invalid:?}");
        }
        assert!(valid_user("ordinary-user"));
        #[cfg(windows)]
        {
            for valid in [
                "COMPUTER\\Brian",
                ".\\Renée",
                "域\\使用者",
                "Jane Doe",
                "user@example.com",
            ] {
                assert!(valid_user(valid), "expected Windows account: {valid}");
            }
            for invalid in ["\\user", "domain\\", "domain\\user\\other"] {
                assert!(!valid_user(invalid));
            }
        }
        #[cfg(not(windows))]
        assert!(!valid_user("domain\\user"));
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    /// ACP folder on the agent's host; `cwd` is the local launcher folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "ClientPolicy::is_default")]
    pub client: ClientPolicy,
}
impl Config {
    /// Build a fresh desktop configuration; publication belongs to the launcher.
    pub fn local(home: &Path, label: &str, port: u16) -> Result<Self> {
        let config = Self {
            id: uuid::Uuid::new_v4().to_string(),
            label: label.into(),
            owner: "owner".into(),
            auth_mode: AuthMode::LocalPassword,
            transport: Transport::LocalHttp,
            allowed_users: vec![],
            embed_origins: vec![],
            bind: "127.0.0.1".into(),
            port,
            public_url: format!("http://127.0.0.1:{port}"),
            pam_service: "agent-conversations".into(),
            tls_cert: home.join("tls/cert.pem"),
            tls_key: home.join("tls/key.pem"),
            tls_ca: None,
            import_source: default_import(),
            import_surface: default_import(),
            agent: None,
        };
        config.validate()?;
        Ok(config)
    }
    pub fn load(home: &Path) -> Result<Self> {
        let c: Self = serde_json::from_slice(&std::fs::read(home.join("config.json"))?)?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.id)?;
        let bind = self.bind.parse::<IpAddr>()?;
        ensure!(
            (self.auth_mode == AuthMode::LocalPassword) == (self.transport == Transport::LocalHttp),
            "Local passwords require the explicit local HTTP mode"
        );
        if self.transport == Transport::LocalHttp {
            let authority = match bind {
                IpAddr::V4(ip) => format!("{ip}:{}", self.port),
                IpAddr::V6(ip) => format!("[{ip}]:{}", self.port),
            };
            ensure!(
                bind.is_loopback()
                    && self.port != 0
                    && self.public_url == format!("http://{authority}")
                    && self.allowed_users.is_empty()
                    && self.embed_origins.is_empty(),
                "Local HTTP requires one exact saved loopback origin and local account"
            );
        }
        ensure!(
            !self.import_source.is_empty()
                && self.import_source.len() <= 80
                && self
                    .import_source
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "Set a stable import source name"
        );
        ensure!(
            !self.import_surface.is_empty()
                && self.import_surface != "web"
                && self.import_surface.len() <= 40
                && self
                    .import_surface
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_ ".contains(&b)),
            "Set a non-web import surface"
        );
        ensure!(
            !self.owner.is_empty() && self.owner.len() <= 100,
            "Set a local owner username"
        );
        ensure!(
            self.allowed_users.len() <= 100 && self.allowed_users.iter().all(|u| valid_user(u)),
            "Use at most 100 explicit local usernames"
        );
        ensure!(valid_user(&self.owner), "Invalid local owner username");
        ensure!(
            !self.pam_service.is_empty()
                && self
                    .pam_service
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "Invalid PAM service"
        );
        ensure!(
            !self.label.is_empty() && self.label.len() <= 200,
            "Set a service label"
        );
        ensure!(
            self.embed_origins.len() <= 16
                && self.embed_origins.iter().all(|origin| {
                    reqwest::Url::parse(origin).is_ok_and(|u| {
                        u.scheme() == "https"
                            && u.host().is_some()
                            && u.username().is_empty()
                            && u.password().is_none()
                            && u.query().is_none()
                            && u.fragment().is_none()
                            && u.path() == "/"
                            && !origin.chars().any(char::is_whitespace)
                            && !origin.contains('*')
                    })
                }),
            "Embedding origins must be explicit HTTPS origins"
        );
        if let Some(agent) = &self.agent {
            agent.validate()?;
        } else {
            ensure!(
                self.auth_mode == AuthMode::LocalPassword,
                "Configure the legacy default agent"
            );
        }
        if self.transport == Transport::Https && !self.public_url.is_empty() {
            let u = reqwest::Url::parse(&self.public_url)?;
            ensure!(
                u.scheme() == "https"
                    && u.host().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.query().is_none()
                    && u.fragment().is_none()
                    && u.path() == "/",
                "Public URL must be an HTTPS origin without credentials, path or query"
            );
        }
        Ok(())
    }
    pub fn save(&self, home: &Path) -> Result<()> {
        self.validate()?;
        private_write(&home.join("config.json"), &serde_json::to_vec_pretty(self)?)
    }
    pub fn allows_user(&self, user: &str) -> bool {
        user == self.owner || self.allowed_users.iter().any(|u| u == user)
    }
    pub fn provider_id(&self) -> String {
        format!("web:{}", self.id)
    }
}
pub fn private_dir(path: &Path) -> Result<()> {
    amplifier_native_platform::private_dir(path)
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    amplifier_native_platform::private_write(path, bytes)
}

pub fn valid_user(user: &str) -> bool {
    #[cfg(windows)]
    {
        // Windows account names may be Unicode or contain spaces. Reject control
        // characters, wildcards and malformed DOMAIN\\user forms, not OS aliases.
        !user.is_empty()
            && user.len() <= 100
            && user.trim() == user
            && !user
                .chars()
                .any(|c| c.is_control() || "/[]:;|=,+*?\"<>".contains(c))
            && match user.split_once('\\') {
                Some((domain, name)) => {
                    !domain.is_empty() && !name.is_empty() && !name.contains('\\')
                }
                None => true,
            }
    }
    #[cfg(not(windows))]
    {
        !user.is_empty()
            && user.len() <= 100
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
    }
}
impl Agent {
    pub fn validate(&self) -> Result<()> {
        self.client.validate()?;
        ensure!(
            !self.label.trim().is_empty() && self.label.len() <= 200,
            "Set an agent label"
        );
        ensure!(
            !self.command.is_empty()
                && self.args.len() <= 128
                && self.command.len() <= 4096
                && self.args.iter().all(|a| a.len() <= 8192),
            "Set a bounded ACP command and arguments"
        );
        ensure!(
            Path::new(&self.cwd).is_absolute(),
            "Agent cwd must be absolute"
        );
        if let Some(folder) = &self.session_cwd {
            let b = folder.as_bytes();
            let absolute = folder.starts_with('/')
                || (b.len() >= 3
                    && b[0].is_ascii_alphabetic()
                    && b[1] == b':'
                    && matches!(b[2], b'/' | b'\\'))
                || folder.starts_with("\\\\");
            ensure!(
                absolute && folder.len() <= 4096 && !folder.chars().any(char::is_control),
                "Agent session_cwd must be an absolute path on the agent's host"
            );
        }
        ensure!(
            (10..=3600).contains(&self.timeout_seconds),
            "Agent timeout must be 10–3600 seconds"
        );
        ensure!(
            self.env.len() <= 100 && self.mcp_servers.len() <= 16,
            "Too many environment settings or MCP servers"
        );
        for s in &self.mcp_servers {
            ensure!(
                s.is_object() && s["name"].as_str().is_some_and(|n| !n.is_empty()),
                "MCP descriptors require a name"
            );
        }
        Ok(())
    }
}

/// Privileged ACP callbacks hosted by this experience, disabled by default.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ClientPolicy {
    pub read_text_file: bool,
    pub write_text_file: bool,
    pub terminal: bool,
    pub workspace_roots: Vec<String>,
    pub allowed_rooms: Vec<String>,
    pub allowed_users: Vec<String>,
    pub allowed_agents: Vec<String>,
    pub permission_timeout_seconds: u64,
    pub max_file_bytes: usize,
    pub terminal_output_bytes: usize,
    pub terminal_timeout_seconds: u64,
}
impl Default for ClientPolicy {
    fn default() -> Self {
        Self {
            read_text_file: false,
            write_text_file: false,
            terminal: false,
            workspace_roots: vec![],
            allowed_rooms: vec![],
            allowed_users: vec![],
            allowed_agents: vec![],
            permission_timeout_seconds: 180,
            max_file_bytes: 1024 * 1024,
            terminal_output_bytes: 256 * 1024,
            terminal_timeout_seconds: 120,
        }
    }
}
impl ClientPolicy {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn enabled(&self) -> bool {
        self.read_text_file || self.write_text_file || self.terminal
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.workspace_roots.len() <= 16
                && self.allowed_rooms.len() <= 64
                && self.allowed_users.len() <= 50
                && self.allowed_agents.len() <= 8,
            "Too many client workspace or audience entries"
        );
        if self.enabled() {
            ensure!(
                !self.workspace_roots.is_empty()
                    && !self.allowed_rooms.is_empty()
                    && !self.allowed_users.is_empty()
                    && !self.allowed_agents.is_empty(),
                "Client callbacks require explicit workspace roots, rooms, people and agent audience"
            );
        }
        ensure!(
            self.workspace_roots.iter().all(|p| p.len() <= 4096
                && Path::new(p).is_absolute()
                && Path::new(p).components().all(|c| !matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                ))),
            "Client roots must be absolute paths without traversal"
        );
        ensure!(
            self.allowed_rooms
                .iter()
                .all(|r| uuid::Uuid::parse_str(r).is_ok()),
            "Client room allowlist must contain exact conversation IDs"
        );
        ensure!(
            self.allowed_users.iter().all(|u| valid_user(u)),
            "Invalid client audience username"
        );
        ensure!(
            self.allowed_agents.iter().all(|a| !a.is_empty()
                && a.len() <= 80
                && a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))),
            "Invalid client agent audience ID"
        );
        ensure!(
            (1..=180).contains(&self.permission_timeout_seconds)
                && (1..=300).contains(&self.terminal_timeout_seconds),
            "Client permission/terminal timeouts are out of range"
        );
        ensure!(
            (1024..=2 * 1024 * 1024).contains(&self.max_file_bytes)
                && (1024..=1024 * 1024).contains(&self.terminal_output_bytes),
            "Client file/output limits are out of range"
        );
        Ok(())
    }
}
