//! Opt-in, audience-bound standard ACP callbacks with single-operation approval.
use crate::config::{Agent, ClientPolicy};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::Notify,
};

pub struct ClientCallbacks {
    policy: ClientPolicy,
    roots: Vec<Root>,
    cwd: PathBuf,
    issuer: String,
    session: Mutex<String>,
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    slots: Arc<tokio::sync::Semaphore>,
    stopped: std::sync::atomic::AtomicBool,
}
struct Terminal {
    _slot: tokio::sync::OwnedSemaphorePermit,
    state: Arc<Mutex<TerminalState>>,
    notify: Arc<Notify>,
    tree: Arc<Mutex<Option<process::ProcessTree>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.tree.lock().unwrap().take();
        self.task.abort();
    }
}
#[derive(Default)]
struct TerminalState {
    output: Vec<u8>,
    truncated: bool,
    limit: usize,
    exit: Option<Value>,
}
impl TerminalState {
    fn append(&mut self, bytes: &[u8], limit: usize) {
        self.limit = limit;
        self.output.extend_from_slice(bytes);
        if self.output.len() > limit {
            self.truncated = true;
            let mut start = self.output.len() - limit;
            while start < self.output.len() && self.output[start] & 0xc0 == 0x80 {
                start += 1;
            }
            self.output.drain(..start);
        }
    }
}
pub struct Audience<'a> {
    pub room: &'a str,
    pub actor: &'a str,
    pub users: &'a [String],
    pub agents: &'a [String],
}
struct Root {
    path: PathBuf,
    directory: std::fs::File,
}
/// Created only after an existing human permission request is resolved. It is
/// bound to one concrete operation in one turn and consumed by value.
pub struct Receipt {
    issuer: String,
    operation: String,
    expires: std::time::Instant,
}
fn fingerprint(method: &str, params: &Value) -> String {
    format!("{:x}", Sha256::digest(json!([method, params]).to_string()))
}
impl ClientCallbacks {
    pub async fn new(agent: &Agent, home: &Path, audience: Audience<'_>) -> Result<Self> {
        let configured = &agent.client;
        configured.validate()?;
        let permitted = configured.enabled()
            && configured.allowed_rooms.iter().any(|r| r == audience.room)
            && configured.allowed_users.iter().any(|u| u == audience.actor)
            && audience.users.iter().any(|u| u == audience.actor)
            && !audience.users.is_empty()
            && !audience.agents.is_empty()
            && audience
                .users
                .iter()
                .all(|u| configured.allowed_users.contains(u))
            && audience
                .agents
                .iter()
                .all(|a| configured.allowed_agents.contains(a));
        let policy = if permitted {
            configured.clone()
        } else {
            ClientPolicy::default()
        };
        let mut this = Self {
            policy,
            roots: vec![],
            cwd: PathBuf::from(&agent.cwd),
            issuer: uuid::Uuid::new_v4().to_string(),
            session: Mutex::new(String::new()),
            terminals: Mutex::new(HashMap::new()),
            slots: Arc::new(tokio::sync::Semaphore::new(8)),
            stopped: std::sync::atomic::AtomicBool::new(false),
        };
        if !permitted {
            return Ok(this);
        }
        ensure!(
            cfg!(unix),
            "Filesystem callbacks require Unix; no unrestricted fallback is used"
        );
        let service = std::fs::canonicalize(home)?;
        let user_home = std::env::var_os("HOME").and_then(|p| std::fs::canonicalize(p).ok());
        for path in &this.policy.workspace_roots {
            let path = PathBuf::from(path);
            ensure!(
                path.components().count() > 2,
                "Choose a dedicated workspace folder"
            );
            for reserved in [
                "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/proc", "/sys", "/dev", "/run",
            ] {
                ensure!(
                    !path.starts_with(reserved),
                    "System directories cannot be client workspaces"
                );
            }
            ensure!(
                !service.starts_with(&path) && !path.starts_with(&service),
                "Workspace roots must not include the service installation or its credentials"
            );
            ensure!(
                user_home.as_ref().is_none_or(|h| !h.starts_with(&path)),
                "Choose a dedicated workspace, not the user's home directory"
            );
            let directory = open_directory(&path)?;
            this.roots.push(Root { path, directory });
        }
        this.roots
            .sort_by_key(|r| std::cmp::Reverse(r.path.components().count()));
        this.check_directory(&this.cwd)?;
        if this.policy.terminal {
            ensure!(
                cfg!(target_os = "linux") && Path::new("/usr/bin/bwrap").is_file(),
                "Client terminals require Linux and /usr/bin/bwrap; no unrestricted fallback is used"
            );
            let mut cmd = this.isolated_command()?;
            cmd.args([
                "--chdir",
                this.cwd.to_str().context("Invalid working directory")?,
                "--",
                "/usr/bin/true",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
            let mut child = cmd
                .spawn()
                .context("Could not check terminal confinement")?;
            let tree = process::ProcessTree::attach(&child)?;
            let result = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            drop(tree);
            ensure!(
                result.is_ok_and(|s| s.is_ok_and(|s| s.success())),
                "Isolated terminal preflight failed; no unrestricted fallback is used"
            );
        }
        Ok(this)
    }
    pub fn bind_session(&self, session: &str) -> Result<()> {
        let mut bound = self.session.lock().unwrap();
        ensure!(
            (bound.is_empty() || *bound == session) && !session.is_empty() && session.len() <= 1024,
            "Invalid callback session"
        );
        *bound = session.into();
        Ok(())
    }
    pub fn advertised(&self) -> Value {
        if !self.policy.enabled() {
            return json!({});
        }
        json!({"fs":{"readTextFile":self.policy.read_text_file,"writeTextFile":self.policy.write_text_file},"terminal":self.policy.terminal})
    }
    pub fn supports(&self, method: &str) -> bool {
        match method {
            "fs/read_text_file" => self.policy.read_text_file,
            "fs/write_text_file" => self.policy.write_text_file,
            "terminal/create"
            | "terminal/output"
            | "terminal/wait_for_exit"
            | "terminal/kill"
            | "terminal/release" => self.policy.terminal,
            _ => false,
        }
    }
    pub fn permission_timeout(&self) -> Duration {
        Duration::from_secs(self.policy.permission_timeout_seconds)
    }
    pub fn permission(&self, method: &str, p: &Value) -> Result<Option<Value>> {
        ensure!(
            !self.stopped.load(std::sync::atomic::Ordering::Acquire),
            "This client turn has ended"
        );
        ensure!(
            p["sessionId"] == *self.session.lock().unwrap()
                && p["sessionId"].as_str().is_some_and(|s| !s.is_empty()),
            "Client operation belongs to a different session"
        );
        ensure!(
            self.supports(method),
            "Client capability not offered for this audience"
        );
        ensure!(
            p.to_string().len() <= self.policy.max_file_bytes.saturating_mul(6) + 65536,
            "Client request too large"
        );
        let title = match method {
            "fs/read_text_file" | "fs/write_text_file" => {
                let path = p["path"].as_str().context("Missing file path")?;
                self.local(path)?;
                if method == "fs/write_text_file" {
                    ensure!(
                        p["content"]
                            .as_str()
                            .is_some_and(|s| s.len() <= self.policy.max_file_bytes),
                        "File content exceeds configured limit"
                    );
                }
                format!(
                    "{} {}",
                    if method == "fs/read_text_file" {
                        "Read file"
                    } else {
                        "Write file"
                    },
                    path
                )
            }
            "terminal/create" => {
                let command = p["command"].as_str().context("Missing command")?;
                ensure!(
                    !command.is_empty() && command.len() <= 4096,
                    "Invalid command"
                );
                format!("Run isolated command: {command}")
            }
            _ => return Ok(None),
        };
        let mut input = p.clone();
        input
            .as_object_mut()
            .context("Invalid client request")?
            .remove("sessionId");
        if method == "fs/write_text_file" {
            let text = p["content"].as_str().context("Missing content")?;
            if text.len() > 8192 {
                input.as_object_mut().unwrap().remove("content");
                input["content_preview"] = json!(text.chars().take(2048).collect::<String>());
                input["content_bytes"] = json!(text.len());
                input["content_sha256"] = json!(format!("{:x}", Sha256::digest(text.as_bytes())));
                input["preview_note"] = json!(
                    "Preview only. Approval covers the entire file content identified by this hash."
                );
            }
        }
        ensure!(
            input.to_string().len() < 55000,
            "Permission details exceed the review limit"
        );
        Ok(Some(
            json!({"sessionId":p["sessionId"],"toolCall":{"toolCallId":uuid::Uuid::new_v4().to_string(),"title":title.chars().take(200).collect::<String>(),"kind":if method=="fs/read_text_file"{"read"}else if method=="fs/write_text_file"{"edit"}else{"execute"},"rawInput":{"method":method,"parameters":input}},"options":[{"optionId":"client_allow_once","name":"Allow once","kind":"allow_once"},{"optionId":"client_reject_once","name":"Deny once","kind":"reject_once"}]}),
        ))
    }
    pub(crate) fn receipt(&self, method: &str, p: &Value, outcome: &Value) -> Result<Receipt> {
        ensure!(
            self.permission(method, p)?.is_some(),
            "This operation does not require a new receipt"
        );
        ensure!(
            outcome["outcome"]["outcome"] == "selected"
                && outcome["outcome"]["optionId"] == "client_allow_once",
            "Client operation was not approved"
        );
        Ok(Receipt {
            issuer: self.issuer.clone(),
            operation: fingerprint(method, p),
            expires: std::time::Instant::now() + Duration::from_secs(30),
        })
    }
    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        let mut terminals = self.terminals.lock().unwrap();
        for terminal in terminals.values() {
            terminal.tree.lock().unwrap().take();
        }
        terminals.clear();
    }
    pub async fn call(&self, method: &str, p: &Value, receipt: Option<Receipt>) -> Result<Value> {
        ensure!(
            p["sessionId"] == *self.session.lock().unwrap(),
            "Client operation belongs to a different session"
        );
        if self.permission(method, p)?.is_some() {
            let r = receipt.context("A single-use human permission receipt is required")?;
            ensure!(
                r.issuer == self.issuer
                    && r.operation == fingerprint(method, p)
                    && r.expires > std::time::Instant::now(),
                "Permission receipt does not match this operation"
            );
        }
        match method {
            "fs/read_text_file" if self.policy.read_text_file => self.read(p),
            "fs/write_text_file" if self.policy.write_text_file => self.write(p),
            "terminal/create" if self.policy.terminal => self.create(p).await,
            "terminal/output" | "terminal/wait_for_exit" | "terminal/kill" | "terminal/release"
                if self.policy.terminal =>
            {
                self.terminal(method, p).await
            }
            _ => bail!("Client capability not offered"),
        }
    }
    fn local(&self, path: &str) -> Result<(PathBuf, PathBuf)> {
        let path = PathBuf::from(path);
        ensure!(
            path.is_absolute()
                && path
                    .components()
                    .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
            "Use an absolute path without traversal"
        );
        let root = self
            .roots
            .iter()
            .find(|r| path.starts_with(&r.path))
            .context("Path is outside the configured client roots")?;
        let relative = path.strip_prefix(&root.path)?.to_owned();
        ensure!(!relative.as_os_str().is_empty(), "A file path is required");
        Ok((root.path.clone(), relative))
    }
    #[cfg(unix)]
    fn open(&self, path: &str, write: bool) -> Result<std::fs::File> {
        use std::{
            ffi::CString,
            os::{
                fd::{AsRawFd, FromRawFd},
                unix::ffi::OsStrExt,
            },
        };
        let (root, relative) = self.local(path)?;
        let mut directory = self
            .roots
            .iter()
            .find(|r| r.path == root)
            .context("Unknown root")?
            .directory
            .try_clone()?;
        let components: Vec<_> = relative.components().collect();
        for (i, component) in components.iter().enumerate() {
            let Component::Normal(name) = component else {
                bail!("Invalid path component")
            };
            let name = CString::new(name.as_bytes())?;
            let last = i + 1 == components.len();
            let flags = if last {
                libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK
                    | if write {
                        libc::O_WRONLY | libc::O_CREAT
                    } else {
                        libc::O_RDONLY
                    }
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
            };
            // SAFETY: the owned directory descriptor and NUL-terminated single
            // path component remain valid. No symlink is ever followed.
            let mut fd =
                unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
            if fd < 0
                && write
                && !last
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
            {
                unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
                fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
            }
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            if last {
                use std::os::unix::fs::MetadataExt;
                let metadata = file.metadata()?;
                ensure!(
                    metadata.is_file() && metadata.nlink() == 1,
                    "Client tools only access regular files without hard links"
                );
                return Ok(file);
            }
            directory = file;
        }
        bail!("File path required")
    }
    #[cfg(not(unix))]
    fn open(&self, _: &str, _: bool) -> Result<std::fs::File> {
        bail!("Local filesystem callbacks are not available on this platform")
    }
    fn read(&self, p: &Value) -> Result<Value> {
        use std::io::Read;
        let file = self.open(p["path"].as_str().context("Missing path")?, false)?;
        ensure!(
            file.metadata()?.len() <= self.policy.max_file_bytes as u64,
            "File exceeds configured byte limit"
        );
        let mut text = String::new();
        file.take(self.policy.max_file_bytes as u64 + 1)
            .read_to_string(&mut text)?;
        ensure!(
            text.len() <= self.policy.max_file_bytes,
            "File exceeds configured byte limit"
        );
        let line = optional_positive(p, "line")?.unwrap_or(1);
        let limit = optional_positive(p, "limit")?.unwrap_or(usize::MAX);
        let content = if p.get("line").is_some_and(|v| !v.is_null())
            || p.get("limit").is_some_and(|v| !v.is_null())
        {
            text.split_inclusive('\n')
                .skip(line - 1)
                .take(limit)
                .collect::<String>()
        } else {
            text
        };
        Ok(json!({"content":content}))
    }
    fn write(&self, p: &Value) -> Result<Value> {
        use std::io::Write;
        let content = p["content"].as_str().context("Missing text content")?;
        ensure!(
            content.len() <= self.policy.max_file_bytes,
            "Content exceeds configured byte limit"
        );
        let mut file = self.open(p["path"].as_str().context("Missing path")?, true)?;
        file.set_len(0)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        Ok(json!({}))
    }
    #[cfg(target_os = "linux")]
    fn isolated_command(&self) -> Result<Command> {
        use std::os::fd::AsRawFd;
        let mut cmd = Command::new("/usr/bin/bwrap");
        process::prepare(&mut cmd);
        cmd.env_clear().args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--ro-bind",
            "/usr",
            "/usr",
        ]);
        for name in ["bin", "sbin", "lib", "lib64"] {
            let path = PathBuf::from(format!("/{name}"));
            if path.exists() {
                if path.is_symlink() {
                    cmd.args([
                        "--symlink",
                        &std::fs::read_link(&path)?.to_string_lossy(),
                        &format!("/{name}"),
                    ]);
                } else {
                    cmd.arg("--ro-bind").arg(&path).arg(&path);
                }
            }
        }
        cmd.args([
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "LANG",
            "C.UTF-8",
        ]);
        // Only operator-selected workspace folders are writable. Host /home,
        // /etc, provider credentials, sockets and network are not mounted.
        for root in self.roots.iter().rev() {
            cmd.arg(if self.policy.write_text_file {
                "--bind-fd"
            } else {
                "--ro-bind-fd"
            })
            .arg(root.directory.as_raw_fd().to_string())
            .arg(&root.path);
        }
        let fds: Vec<_> = self.roots.iter().map(|r| r.directory.as_raw_fd()).collect();
        // SAFETY: after fork, only fcntl is called on owned descriptors. The
        // parent's close-on-exec flags remain unchanged; bwrap consumes these.
        unsafe {
            cmd.pre_exec(move || {
                for fd in &fds {
                    if libc::fcntl(*fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        Ok(cmd)
    }
    #[cfg(not(target_os = "linux"))]
    fn isolated_command(&self) -> Result<Command> {
        bail!("Isolated terminals require Linux")
    }
    fn check_directory(&self, path: &Path) -> Result<()> {
        ensure!(
            path.is_absolute()
                && path
                    .components()
                    .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
            "Use an absolute working folder without traversal"
        );
        let root = self
            .roots
            .iter()
            .find(|r| path.starts_with(&r.path))
            .context("Working folder is outside configured roots")?;
        open_relative_directory(&root.directory, path.strip_prefix(&root.path)?)?;
        Ok(())
    }
    async fn create(&self, p: &Value) -> Result<Value> {
        ensure!(
            cfg!(target_os = "linux"),
            "Isolated client terminals currently require Linux"
        );
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .context("Too many active client terminals")?;
        let command = p["command"].as_str().context("Missing command")?;
        ensure!(
            !command.is_empty() && command.len() <= 4096 && !command.contains('\0'),
            "Invalid terminal command"
        );
        let args: Vec<String> = serde_json::from_value(
            p.get("args")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!([])),
        )?;
        ensure!(
            args.len() <= 128 && args.iter().all(|v| v.len() <= 16384 && !v.contains('\0')),
            "Terminal arguments too large"
        );
        let cwd = p["cwd"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(self.cwd.clone());
        self.check_directory(&cwd)?;
        let limit = optional_positive(p, "outputByteLimit")?
            .unwrap_or(self.policy.terminal_output_bytes)
            .min(self.policy.terminal_output_bytes);
        let mut cmd = self.isolated_command()?;
        if let Some(env) = p.get("env").filter(|v| !v.is_null()) {
            let env = env
                .as_array()
                .context("Terminal environment must be an array")?;
            ensure!(env.len() <= 64, "Too many terminal environment entries");
            for item in env {
                let name = item["name"].as_str().context("Missing environment name")?;
                let value = item["value"]
                    .as_str()
                    .context("Missing environment value")?;
                ensure!(
                    !name.is_empty()
                        && name.len() <= 128
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        && value.len() <= 16384
                        && !value.contains('\0'),
                    "Invalid terminal environment"
                );
                cmd.args(["--setenv", name, value]);
            }
        }
        cmd.arg("--chdir")
            .arg(cwd)
            .arg("--")
            .arg(command)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .context("Could not start isolated client terminal")?;
        let tree = Arc::new(Mutex::new(Some(process::ProcessTree::attach(&child)?)));
        let state = Arc::new(Mutex::new(TerminalState::default()));
        let notify = Arc::new(Notify::new());
        let stdout = child.stdout.take().context("Missing terminal output")?;
        let stderr = child.stderr.take().context("Missing terminal errors")?;
        let out = state.clone();
        let changed = notify.clone();
        let cleanup = tree.clone();
        let timeout = self.policy.terminal_timeout_seconds;
        let task = tokio::spawn(async move {
            let a = capture(stdout, out.clone(), limit);
            let b = capture(stderr, out.clone(), limit);
            let wait = async {
                let status =
                    match tokio::time::timeout(Duration::from_secs(timeout), child.wait()).await {
                        Ok(status) => status,
                        Err(_) => {
                            cleanup.lock().unwrap().take();
                            child.wait().await
                        }
                    };
                // Reap every descendant even when the primary command exits.
                cleanup.lock().unwrap().take();
                status
            };
            let (status, _, _) = tokio::join!(wait, a, b);
            let value = match status {
                Ok(status) => {
                    #[cfg(unix)]
                    let signal = {
                        use std::os::unix::process::ExitStatusExt;
                        status.signal().map(|s| s.to_string())
                    };
                    #[cfg(not(unix))]
                    let signal: Option<String> = None;
                    json!({"exitCode":status.code(),"signal":signal})
                }
                Err(_) => json!({"exitCode":null,"signal":"error"}),
            };
            out.lock().unwrap().exit = Some(value);
            changed.notify_waiters();
        });
        let id = uuid::Uuid::new_v4().to_string();
        let terminal = Arc::new(Terminal {
            _slot: slot,
            state,
            notify,
            tree,
            task,
        });
        let mut terminals = self.terminals.lock().unwrap();
        ensure!(
            !self.stopped.load(std::sync::atomic::Ordering::Acquire),
            "This client turn has ended"
        );
        terminals.insert(id.clone(), terminal);
        Ok(json!({"terminalId":id}))
    }
    async fn terminal(&self, method: &str, p: &Value) -> Result<Value> {
        let id = p["terminalId"].as_str().context("Missing terminal id")?;
        let terminal = self
            .terminals
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .context("Unknown terminal for this session")?;
        match method {
            "terminal/output" => {
                let state = terminal.state.lock().unwrap();
                let output = String::from_utf8_lossy(&state.output);
                let mut start = output.len().saturating_sub(state.limit);
                while !output.is_char_boundary(start) {
                    start += 1;
                }
                let mut result =
                    json!({"output":&output[start..],"truncated":state.truncated || start>0});
                if let Some(exit) = &state.exit {
                    result["exitStatus"] = exit.clone();
                }
                Ok(result)
            }
            "terminal/kill" => {
                terminal.tree.lock().unwrap().take();
                Ok(json!({}))
            }
            "terminal/release" => {
                terminal.tree.lock().unwrap().take();
                self.terminals.lock().unwrap().remove(id);
                Ok(json!({}))
            }
            "terminal/wait_for_exit" => loop {
                let notified = terminal.notify.notified();
                if let Some(exit) = terminal.state.lock().unwrap().exit.clone() {
                    break Ok(exit);
                }
                notified.await;
            },
            _ => bail!("Unknown terminal operation"),
        }
    }
}
fn optional_positive(p: &Value, key: &str) -> Result<Option<usize>> {
    let Some(v) = p.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let n = v
        .as_u64()
        .filter(|n| *n > 0 && *n <= usize::MAX as u64)
        .context("Expected a positive integer")?;
    Ok(Some(n as usize))
}
async fn capture<R: AsyncRead + Unpin>(
    mut stream: R,
    state: Arc<Mutex<TerminalState>>,
    limit: usize,
) {
    let mut buffer = [0u8; 4096];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => state.lock().unwrap().append(&buffer[..n], limit),
        }
    }
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<std::fs::File> {
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
        "Use an absolute workspace without traversal or symlinks"
    );
    open_relative_directory(&std::fs::File::open("/")?, path.strip_prefix("/")?)
}
#[cfg(unix)]
fn open_relative_directory(root: &std::fs::File, relative: &Path) -> Result<std::fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };
    let mut directory = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("Invalid directory component")
        };
        let name = CString::new(name.as_bytes())?;
        // SAFETY: valid owned directory and a single NUL-terminated component.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        ensure!(
            fd >= 0,
            "Working folder cannot contain symlinks or inaccessible directories"
        );
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}
#[cfg(not(unix))]
fn open_directory(_: &Path) -> Result<std::fs::File> {
    bail!("Filesystem callbacks require Unix")
}
#[cfg(not(unix))]
fn open_relative_directory(_: &std::fs::File, _: &Path) -> Result<std::fs::File> {
    bail!("Filesystem callbacks require Unix")
}
mod process {
    use anyhow::{Context, Result};
    use tokio::process::{Child, Command};
    pub fn prepare(c: &mut Command) {
        #[cfg(unix)]
        c.process_group(0);
    }
    pub struct ProcessTree {
        pid: u32,
    }
    impl ProcessTree {
        pub fn attach(child: &Child) -> Result<Self> {
            Ok(Self {
                pid: child.id().context("Terminal exited during launch")?,
            })
        }
    }
    impl Drop for ProcessTree {
        fn drop(&mut self) {
            #[cfg(unix)]
            if self.pid > 0 {
                unsafe {
                    libc::kill(-(self.pid as i32), libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::ClientPolicy;
    use std::collections::BTreeMap;
    const ROOM: &str = "12345678-1234-4234-8234-123456789abc";
    fn fixture() -> (tempfile::TempDir, Agent, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(home.path()).unwrap();
        let work = base.join("workspace");
        let service = base.join("service");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&service).unwrap();
        let agent = Agent {
            label: "Example".into(),
            command: "unused".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: work.to_string_lossy().into_owned(),
            session_cwd: None,
            mcp_servers: vec![],
            timeout_seconds: 30,
            client: ClientPolicy {
                read_text_file: true,
                write_text_file: true,
                workspace_roots: vec![work.to_string_lossy().into_owned()],
                allowed_rooms: vec![ROOM.into()],
                allowed_users: vec!["alice".into()],
                allowed_agents: vec!["default".into()],
                ..Default::default()
            },
        };
        (home, agent, service)
    }
    async fn tools(agent: &Agent, home: &Path) -> ClientCallbacks {
        let c = ClientCallbacks::new(
            agent,
            home,
            Audience {
                room: ROOM,
                actor: "alice",
                users: &["alice".into()],
                agents: &["default".into()],
            },
        )
        .await
        .unwrap();
        c.bind_session("session").unwrap();
        c
    }
    fn allow(c: &ClientCallbacks, method: &str, p: &Value) -> Receipt {
        c.receipt(
            method,
            p,
            &json!({"outcome":{"outcome":"selected","optionId":"client_allow_once"}}),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn default_room_actor_and_whole_audience_control_advertising() {
        let (_temp, mut agent, service) = fixture();
        assert!(ClientPolicy::default().validate().is_ok());
        agent.client.allowed_rooms.clear();
        assert!(agent.validate().is_err());
        agent.client.allowed_rooms.push(ROOM.into());
        for (room, actor, users, agents) in [
            (
                "other",
                "alice",
                vec!["alice".into()],
                vec!["default".into()],
            ),
            (ROOM, "bob", vec!["bob".into()], vec!["default".into()]),
            (
                ROOM,
                "alice",
                vec!["alice".into(), "bob".into()],
                vec!["default".into()],
            ),
            (
                ROOM,
                "alice",
                vec!["alice".into()],
                vec!["default".into(), "untrusted".into()],
            ),
        ] {
            let c = ClientCallbacks::new(
                &agent,
                &service,
                Audience {
                    room,
                    actor,
                    users: &users,
                    agents: &agents,
                },
            )
            .await
            .unwrap();
            assert_eq!(c.advertised(), json!({}));
        }
        let c = tools(&agent, &service).await;
        assert_eq!(
            c.advertised(),
            json!({"fs":{"readTextFile":true,"writeTextFile":true},"terminal":false})
        );
        let mut old = serde_json::to_value(&agent).unwrap();
        old.as_object_mut().unwrap().remove("client");
        let legacy = serde_json::from_value::<Agent>(old.clone()).unwrap();
        assert!(!legacy.client.enabled());
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            old,
            "Disabled defaults must preserve the endpoint fingerprint of old installations"
        );
    }
    #[tokio::test]
    async fn every_file_operation_requires_an_exact_turn_bound_single_use_receipt() {
        let (_temp, agent, service) = fixture();
        let c = tools(&agent, &service).await;
        let path = Path::new(&agent.cwd).join("a.txt");
        let p = json!({"sessionId":"session","path":path,"content":"one\ntwo\n"});
        assert!(c.call("fs/write_text_file", &p, None).await.is_err());
        assert!(!path.exists());
        assert!(
            c.receipt("fs/write_text_file", &p, &crate::permissions::cancelled())
                .is_err()
        );
        let mut changed = p.clone();
        changed["content"] = json!("other");
        assert!(
            c.call(
                "fs/write_text_file",
                &changed,
                Some(allow(&c, "fs/write_text_file", &p))
            )
            .await
            .is_err()
        );
        let other = tools(&agent, &service).await;
        assert!(
            other
                .call(
                    "fs/write_text_file",
                    &p,
                    Some(allow(&c, "fs/write_text_file", &p))
                )
                .await
                .is_err()
        );
        c.call(
            "fs/write_text_file",
            &p,
            Some(allow(&c, "fs/write_text_file", &p)),
        )
        .await
        .unwrap();
        assert!(c.call("fs/write_text_file", &p, None).await.is_err());
        let read = json!({"sessionId":"session","path":path,"line":2,"limit":1});
        assert_eq!(
            c.call(
                "fs/read_text_file",
                &read,
                Some(allow(&c, "fs/read_text_file", &read))
            )
            .await
            .unwrap()["content"],
            "two\n"
        );
        let mut wrong = read.clone();
        wrong["sessionId"] = json!("another");
        assert!(c.call("fs/read_text_file", &wrong, None).await.is_err());
        let mut expired = allow(&c, "fs/read_text_file", &read);
        expired.expires = std::time::Instant::now() - Duration::from_secs(1);
        assert!(
            c.call("fs/read_text_file", &read, Some(expired))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn symlinks_hardlinks_traversal_devices_and_private_service_roots_are_denied() {
        use std::os::unix::fs::symlink;
        let (_temp, mut agent, service) = fixture();
        let c = tools(&agent, &service).await;
        let outside = service.join("secret");
        std::fs::write(&outside, "private").unwrap();
        let work = Path::new(&agent.cwd);
        symlink(&outside, work.join("link")).unwrap();
        symlink(&service, work.join("dir-link")).unwrap();
        std::fs::hard_link(&outside, work.join("hard")).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let fifo = work.join("fifo");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let fp = json!({"sessionId":"session","path":fifo});
        assert!(
            c.call(
                "fs/read_text_file",
                &fp,
                Some(allow(&c, "fs/read_text_file", &fp))
            )
            .await
            .is_err()
        );
        for path in [
            work.join("link"),
            work.join("hard"),
            work.join("dir-link/secret"),
        ] {
            let p = json!({"sessionId":"session","path":path});
            assert!(
                c.call(
                    "fs/read_text_file",
                    &p,
                    Some(allow(&c, "fs/read_text_file", &p))
                )
                .await
                .is_err()
            );
            let p = json!({"sessionId":"session","path":path,"content":"overwrite"});
            assert!(
                c.call(
                    "fs/write_text_file",
                    &p,
                    Some(allow(&c, "fs/write_text_file", &p))
                )
                .await
                .is_err()
            );
        }
        for path in [
            outside.clone(),
            work.join("../service/secret"),
            PathBuf::from("relative"),
        ] {
            assert!(
                c.permission(
                    "fs/read_text_file",
                    &json!({"sessionId":"session","path":path})
                )
                .is_err()
            );
        }
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "private");
        agent.client.workspace_roots = vec![service.to_string_lossy().into_owned()];
        assert!(
            ClientCallbacks::new(
                &agent,
                &service,
                Audience {
                    room: ROOM,
                    actor: "alice",
                    users: &["alice".into()],
                    agents: &["default".into()]
                }
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn pinned_roots_survive_path_replacement_without_following_a_new_symlink() {
        use std::os::unix::fs::symlink;
        let (_temp, agent, service) = fixture();
        let c = tools(&agent, &service).await;
        let work = Path::new(&agent.cwd);
        std::fs::write(work.join("a"), "original").unwrap();
        std::fs::write(service.join("a"), "private").unwrap();
        std::fs::rename(work, work.with_extension("moved")).unwrap();
        symlink(&service, work).unwrap();
        let p = json!({"sessionId":"session","path":work.join("a")});
        assert_eq!(
            c.call(
                "fs/read_text_file",
                &p,
                Some(allow(&c, "fs/read_text_file", &p))
            )
            .await
            .unwrap()["content"],
            "original"
        );
    }
    #[tokio::test]
    async fn file_sizes_and_permission_previews_remain_bounded() {
        let (_temp, mut agent, service) = fixture();
        agent.client.max_file_bytes = 10000;
        let c = tools(&agent, &service).await;
        let path = Path::new(&agent.cwd).join("large");
        let p = json!({"sessionId":"session","path":path,"content":"é".repeat(4500)});
        let permission = c.permission("fs/write_text_file", &p).unwrap().unwrap();
        assert_eq!(
            permission["toolCall"]["rawInput"]["parameters"]["content_bytes"],
            9000
        );
        assert!(permission["toolCall"]["rawInput"]["parameters"]["content"].is_null());
        c.call(
            "fs/write_text_file",
            &p,
            Some(allow(&c, "fs/write_text_file", &p)),
        )
        .await
        .unwrap();
        std::fs::write(&path, "x".repeat(10001)).unwrap();
        let read = json!({"sessionId":"session","path":path});
        assert!(
            c.call(
                "fs/read_text_file",
                &read,
                Some(allow(&c, "fs/read_text_file", &read))
            )
            .await
            .is_err()
        );
        let mut too_large = p;
        too_large["content"] = json!("x".repeat(10001));
        assert!(c.permission("fs/write_text_file", &too_large).is_err());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "Requires Linux bubblewrap with user namespaces permitted; run explicitly on deployment hosts"]
    async fn terminal_is_confined_networkless_bounded_and_killed_on_timeout_or_stop() {
        let (_temp, mut agent, service) = fixture();
        agent.client.terminal = true;
        agent.client.terminal_timeout_seconds = 1;
        agent.client.terminal_output_bytes = 1024;
        let c = tools(&agent, &service).await;
        let private = service.join("secret");
        std::fs::write(&private, "private").unwrap();
        let script = format!(
            "import os,socket; assert not os.path.exists({:?}); assert not os.path.exists('/etc/passwd'); s=socket.socket(); assert s.connect_ex(('1.1.1.1',80))!=0; print('z'*4000)",
            private.to_string_lossy()
        );
        let p = json!({"sessionId":"session","command":"/usr/bin/python3","args":["-c",script]});
        assert!(c.call("terminal/create", &p, None).await.is_err());
        let id = c
            .call(
                "terminal/create",
                &p,
                Some(allow(&c, "terminal/create", &p)),
            )
            .await
            .unwrap()["terminalId"]
            .clone();
        let handle = json!({"sessionId":"session","terminalId":id});
        assert_eq!(
            c.call("terminal/wait_for_exit", &handle, None)
                .await
                .unwrap()["exitCode"],
            0
        );
        let out = c.call("terminal/output", &handle, None).await.unwrap();
        assert_eq!(out["truncated"], true);
        assert!(out["output"].as_str().unwrap().len() <= 1024);
        c.call("terminal/release", &handle, None).await.unwrap();
        assert!(c.call("terminal/output", &handle, None).await.is_err());
        let escaped = Path::new(&agent.cwd).join("escaped-child");
        let child_code = format!(
            "import time,pathlib; time.sleep(2); pathlib.Path({:?}).write_text('escaped')",
            escaped.to_string_lossy()
        );
        let parent_code = format!(
            "import subprocess,time; subprocess.Popen(['/usr/bin/python3','-c',{:?}]); time.sleep(60)",
            child_code
        );
        let p =
            json!({"sessionId":"session","command":"/usr/bin/python3","args":["-c",parent_code]});
        let id = c
            .call(
                "terminal/create",
                &p,
                Some(allow(&c, "terminal/create", &p)),
            )
            .await
            .unwrap()["terminalId"]
            .clone();
        let handle = json!({"sessionId":"session","terminalId":id});
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            c.call("terminal/wait_for_exit", &handle, None),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(out["exitCode"].is_null());
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(
            !escaped.exists(),
            "A terminal descendant survived its time limit"
        );
        let id = c
            .call(
                "terminal/create",
                &p,
                Some(allow(&c, "terminal/create", &p)),
            )
            .await
            .unwrap()["terminalId"]
            .clone();
        let mut bad = p.clone();
        bad["cwd"] = json!(service);
        assert!(
            c.call(
                "terminal/create",
                &bad,
                Some(allow(&c, "terminal/create", &bad))
            )
            .await
            .is_err()
        );
        c.stop();
        assert!(
            c.call(
                "terminal/output",
                &json!({"sessionId":"session","terminalId":id}),
                None
            )
            .await
            .is_err()
        );
    }
}
