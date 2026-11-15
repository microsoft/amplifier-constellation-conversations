//! Read-only, Linux-only evidence for adopting an existing custom installation.
//!
//! Capture happens before the installation owner stops the original service.
//! This module never dispatches Start/Stop, registers a unit, repairs permissions,
//! or publishes an origin. The adoption coordinator owns protected journaling,
//! service/update locks, effect admission, cutover and restoration decisions.
use amplifier_native_platform::installation::Service;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

pub const RECORD: &str = "native-legacy-origin.json";

#[cfg(not(target_os = "linux"))]
pub fn capture(_home: &Path, _transaction: &str) -> Result<Value> {
    anyhow::bail!("Legacy custom-service adoption currently requires Linux")
}
#[cfg(not(target_os = "linux"))]
pub fn validate(_home: &Path, _origin: &Value) -> Result<Service> {
    anyhow::bail!("Legacy custom-service adoption currently requires Linux")
}
#[cfg(not(target_os = "linux"))]
pub fn verify_stopped(_home: &Path, _origin: &Value) -> Result<()> {
    anyhow::bail!("Legacy custom-service adoption currently requires Linux")
}
#[cfg(not(target_os = "linux"))]
pub fn verify_absent(_home: &Path, _origin: &Value) -> Result<()> {
    anyhow::bail!("Legacy custom-service adoption currently requires Linux")
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn native_profile(_origin: &Value, _current: &Value) -> Result<()> {
    anyhow::bail!("Legacy custom-service adoption currently requires Linux")
}
#[cfg(not(target_os = "linux"))]
pub fn committed_definition(home: &Path, _id: &str, _fixed_path: &Path) -> Result<Option<String>> {
    if home.join(RECORD).try_exists()? {
        anyhow::bail!("Legacy origin belongs to another platform")
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
pub(crate) use linux::native_profile;
#[cfg(target_os = "linux")]
pub use linux::{capture, committed_definition, validate, verify_absent, verify_stopped};

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::{config::Config, native_bundle::APPLICATION};
    use amplifier_native_platform::{installation, process};
    use anyhow::{Context, ensure};
    use serde::{Deserialize, Serialize};
    #[cfg(test)]
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs::{self, File, OpenOptions},
        io::{ErrorKind, Read},
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
        os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        },
        path::{Component, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const LIMIT: usize = 1024 * 1024;
    const IMAGE_LIMIT: u64 = 512 * 1024 * 1024;
    const MEMBER_LIMIT: usize = 4096;
    const DIRECTORY_LIMIT: usize = 1024;
    const MANAGER_LIMIT: usize = 64 * 1024;

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Origin {
        schema: u32,
        transaction_id: String,
        installation_id: String,
        home: PathBuf,
        account: String,
        executable: PathBuf,
        binary_sha256: String,
        config_sha256: String,
        service: Service,
        definition: String,
        definition_sha256: String,
        native_snapshot: Value,
        control_group: String,
        members: Vec<Value>,
        listener: SocketAddr,
        runtime_sha256: String,
        original_running: bool,
    }

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn valid_hash(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    fn uid() -> u32 {
        unsafe { libc::geteuid() }
    }
    fn account() -> String {
        format!("uid:{}", uid())
    }

    // Opened-handle, current-owner inspection; never repair a supplied file.
    fn file(path: &Path, private: bool, limit: u64) -> Result<File> {
        let before = fs::symlink_metadata(path)?;
        ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "Expected an ordinary origin file"
        );
        ensure!(
            before.uid() == uid() && before.mode() & if private { 0o077 } else { 0o022 } == 0,
            "Origin file ownership or policy is unsafe"
        );
        ensure!(before.len() <= limit, "Origin file exceeds its bound");
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = opened.metadata()?;
        ensure!(
            metadata.dev() == before.dev()
                && metadata.ino() == before.ino()
                && metadata.uid() == before.uid()
                && metadata.mode() == before.mode()
                && metadata.len() == before.len(),
            "Origin file changed while opening"
        );
        Ok(opened)
    }
    fn read_owned(path: &Path, private: bool) -> Result<Vec<u8>> {
        let mut opened = file(path, private, LIMIT as u64)?;
        let before = opened.metadata()?;
        let mut bytes = Vec::new();
        opened
            .by_ref()
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= LIMIT, "Origin file exceeds its bound");
        let after = opened.metadata()?;
        let named = fs::symlink_metadata(path)?;
        ensure!(
            before.dev() == after.dev()
                && before.ino() == after.ino()
                && before.len() == after.len()
                && before.mtime() == after.mtime()
                && before.mtime_nsec() == after.mtime_nsec()
                && before.ctime() == after.ctime()
                && before.ctime_nsec() == after.ctime_nsec()
                && named.dev() == after.dev()
                && named.ino() == after.ino()
                && named.mode() == before.mode()
                && named.uid() == before.uid(),
            "Origin file changed while reading"
        );
        Ok(bytes)
    }
    fn image(path: &Path) -> Result<String> {
        let mut opened = file(path, false, IMAGE_LIMIT)?;
        let before = opened.metadata()?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let count = opened.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            ensure!(total <= IMAGE_LIMIT, "Legacy image exceeds its bound");
            digest.update(&buffer[..count]);
        }
        let after = opened.metadata()?;
        let named = fs::symlink_metadata(path)?;
        ensure!(
            before.dev() == named.dev()
                && before.ino() == named.ino()
                && before.len() == after.len()
                && before.mtime() == after.mtime()
                && before.mtime_nsec() == after.mtime_nsec()
                && before.ctime() == after.ctime()
                && before.ctime_nsec() == after.ctime_nsec(),
            "Legacy image changed while hashing"
        );
        Ok(format!("{:x}", digest.finalize()))
    }
    fn configuration(home: &Path) -> Result<(Config, Vec<u8>)> {
        ensure!(
            home.is_absolute() && dunce::canonicalize(home)? == home,
            "Use the canonical installation home"
        );
        amplifier_native_platform::validate_private_directory(home)?;
        let bytes = read_owned(&home.join("config.json"), true)?;
        let config: Config = serde_json::from_slice(&bytes)?;
        config.validate()?;
        ensure!(
            uuid::Uuid::parse_str(&config.id)?.to_string() == config.id,
            "Invalid installation identity"
        );
        Ok((config, bytes))
    }
    fn service(id: &str) -> Result<Service> {
        ensure!(
            uuid::Uuid::parse_str(id)?.to_string() == id,
            "Invalid installation identity"
        );
        let name = format!("{}-{id}.service", APPLICATION.startup_prefix);
        let path = amplifier_native_platform::user_home()?
            .join(".config/systemd/user")
            .join(&name);
        Ok(Service {
            manager: installation::ServiceManager::SystemdUser,
            name,
            definition_path: Some(path),
        })
    }
    fn decode(value: &Value) -> Result<Origin> {
        ensure!(
            serde_json::to_vec(value)?.len() <= LIMIT,
            "Origin evidence exceeds its bound"
        );
        let origin: Origin = serde_json::from_value(value.clone())?;
        ensure!(
            origin.schema == 1
                && origin.original_running
                && uuid::Uuid::parse_str(&origin.transaction_id)?.to_string()
                    == origin.transaction_id
                && origin.account == account()
                && origin.home.is_absolute()
                && origin.service == service(&origin.installation_id)?
                && origin.executable
                    == amplifier_native_platform::installed_binary(&APPLICATION, &origin.home)
                && valid_hash(&origin.binary_sha256)
                && valid_hash(&origin.config_sha256)
                && valid_hash(&origin.runtime_sha256)
                && sha(origin.definition.as_bytes()) == origin.definition_sha256,
            "Legacy origin binding is invalid"
        );
        ensure!(
            !origin.members.is_empty() && origin.members.len() <= MEMBER_LIMIT,
            "Origin lacks bounded original process generations"
        );
        let mut pids = BTreeSet::new();
        for member in &origin.members {
            ensure!(
                member.as_object().is_some_and(|m| m.len() == 3)
                    && member["pid"]
                        .as_u64()
                        .is_some_and(|p| p > 0 && p <= i32::MAX as u64 && pids.insert(p))
                    && member["start_ticks"]
                        .as_str()
                        .is_some_and(|s| s.parse::<u64>().is_ok_and(|n| n > 0))
                    && member["boot_id"]
                        .as_str()
                        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok()),
                "Invalid original process generation"
            );
        }
        cgroup_path(&origin.control_group, &origin.service.name)?;
        ensure!(
            origin.listener.port() > 0 && !origin.listener.ip().is_unspecified(),
            "Invalid captured listener"
        );
        let original = &origin.native_snapshot;
        require_native_profile(original, &origin.service, &origin.definition_sha256)?;
        ensure!(
            original["active"] == true && original["native"]["SubState"] == "running",
            "Origin is not an originally running service"
        );
        fixed_command(
            &original["native"]["ExecStart"],
            &origin.executable,
            &origin.home,
        )?;
        let main = main_identity(original)?;
        ensure!(
            origin.members.contains(main),
            "Original main generation is outside captured cgroup"
        );
        Ok(origin)
    }
    fn bound(home: &Path, origin: &Origin) -> Result<()> {
        let (config, _) = configuration(home)?;
        ensure!(
            origin.home == home
                && origin.installation_id == config.id
                && origin.account == account(),
            "Origin belongs to another installation or account"
        );
        Ok(())
    }

    fn require_native_profile(
        snapshot: &Value,
        service: &Service,
        definition_hash: &str,
    ) -> Result<()> {
        ensure!(
            snapshot["schema"] == 1
                && snapshot["service"] == serde_json::to_value(service)?
                && snapshot["definition_matches"] == true
                && snapshot["transitioning"] == false
                && snapshot["loaded"] == true
                && snapshot["masked"] == false
                && snapshot["definition"]["sha256"] == definition_hash
                && snapshot["definition"]["path"]
                    == serde_json::to_value(&service.definition_path)?
                && snapshot["native"]["Id"] == service.name
                && snapshot["native"]["DropInPaths"] == ""
                && snapshot["native"]["NeedDaemonReload"] == "no"
                && snapshot["native"]["KillMode"] == "control-group"
                && snapshot["native"]["RefuseManualStart"] == "no"
                && snapshot["native"]["RefuseManualStop"] == "no",
            "Custom service identity, loaded definition or policy is unavailable"
        );
        ensure!(
            snapshot["processes"].is_array(),
            "Missing native process observations"
        );
        Ok(())
    }
    fn command_identity(value: &Value) -> Result<String> {
        let text = value.as_str().context("Missing effective ExecStart")?;
        ensure!(
            !text.chars().any(char::is_control),
            "Malformed effective ExecStart"
        );
        let record = text
            .strip_prefix("{ ")
            .and_then(|s| s.strip_suffix(" }"))
            .context("Malformed ExecStart")?;
        let fields: Vec<_> = record.split(" ; ").collect();
        ensure!(
            fields.len() == 8
                && fields[0].starts_with("path=/")
                && fields[1].starts_with("argv[]=")
                && matches!(fields[2], "ignore_errors=no" | "ignore_errors=yes")
                && fields[3].starts_with("start_time=[")
                && fields[4].starts_with("stop_time=[")
                && fields[5]
                    .strip_prefix("pid=")
                    .is_some_and(|p| p.parse::<u32>().is_ok())
                && fields[6].starts_with("code=")
                && fields[7].starts_with("status="),
            "Unknown effective ExecStart representation"
        );
        Ok(fields[..3].join(" ; "))
    }
    fn fixed_command(value: &Value, executable: &Path, home: &Path) -> Result<()> {
        let identity = command_identity(value)?;
        let fields: Vec<_> = identity.split(" ; ").collect();
        let executable = executable.to_str().context("Non-UTF8 legacy executable")?;
        let home = home.to_str().context("Non-UTF8 legacy home")?;
        ensure!(
            fields[0] == format!("path={executable}")
                && fields[2] == "ignore_errors=no"
                && (fields[1] == format!("argv[]={executable} --home {home} serve")
                    || fields[1] == format!("argv[]={executable} serve --home {home}")),
            "Loaded startup command does not match the fixed installation"
        );
        Ok(())
    }
    fn static_profile(snapshot: &Value) -> Result<Value> {
        let mut value = snapshot.clone();
        let object = value.as_object_mut().context("Invalid native snapshot")?;
        for dynamic in ["active", "processes", "transitioning"] {
            object.remove(dynamic);
        }
        let native = object
            .get_mut("native")
            .and_then(Value::as_object_mut)
            .context("Missing native policy")?;
        for dynamic in [
            "MainPID",
            "ControlPID",
            "ActiveState",
            "SubState",
            "InvocationID",
        ] {
            native.remove(dynamic);
        }
        let command = command_identity(native.get("ExecStart").context("Missing ExecStart")?)?;
        native.insert("ExecStart".into(), Value::String(command));
        Ok(value)
    }
    pub(crate) fn native_profile(origin: &Value, current: &Value) -> Result<()> {
        let origin = decode(origin)?;
        require_native_profile(current, &origin.service, &origin.definition_sha256)?;
        ensure!(
            static_profile(&origin.native_snapshot)? == static_profile(current)?,
            "Original service definition or policy changed; preserved"
        );
        Ok(())
    }
    fn main_identity(snapshot: &Value) -> Result<&Value> {
        let processes = snapshot["processes"]
            .as_array()
            .context("Missing native processes")?;
        let mains: Vec<_> = processes
            .iter()
            .filter(|p| p["role"] == "MainPID")
            .collect();
        ensure!(
            mains.len() == 1,
            "Original service lacks one exact main generation"
        );
        Ok(&mains[0]["identity"])
    }

    fn read_fixed(path: &Path, limit: usize) -> Result<Vec<u8>> {
        let mut result = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?
            .take(limit as u64 + 1)
            .read_to_end(&mut result)?;
        ensure!(
            result.len() <= limit,
            "Native observation exceeds its bound"
        );
        Ok(result)
    }
    fn cgroup_path(group: &str, service_name: &str) -> Result<PathBuf> {
        let path = Path::new(group);
        ensure!(
            path.is_absolute()
                && group.len() <= 4096
                && !group.chars().any(char::is_control)
                && path.file_name().is_some_and(|name| name == service_name)
                && path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                && path.components().count() >= 3,
            "Invalid fixed service cgroup"
        );
        Ok(Path::new("/sys/fs/cgroup").join(path.strip_prefix("/")?))
    }
    fn ordinary_directory(path: &Path) -> Result<()> {
        let m = fs::symlink_metadata(path)?;
        ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "Native cgroup is not an ordinary directory"
        );
        Ok(())
    }
    fn cgroup_members(path: &Path, allow_absent: bool) -> Result<BTreeSet<u32>> {
        // Absence is accepted only beneath inspected ordinary ancestors. A
        // redirected missing descendant must not become an empty-cgroup proof.
        let mut inspected = PathBuf::from("/sys/fs/cgroup");
        ordinary_directory(&inspected)?;
        let relative = path.strip_prefix(&inspected)?;
        for component in relative.components() {
            ensure!(
                matches!(component, Component::Normal(_)),
                "Invalid cgroup traversal"
            );
            inspected.push(component);
            match fs::symlink_metadata(&inspected) {
                Err(error) if allow_absent && error.kind() == ErrorKind::NotFound => {
                    return Ok(BTreeSet::new());
                }
                result => {
                    result?;
                }
            }
            ordinary_directory(&inspected)?;
        }
        let mut pending = vec![path.to_path_buf()];
        let mut directories = 0;
        let mut members = BTreeSet::new();
        while let Some(directory) = pending.pop() {
            directories += 1;
            ensure!(
                directories <= DIRECTORY_LIMIT,
                "Service cgroup nesting exceeds its bound"
            );
            ordinary_directory(&directory)?;
            let text =
                String::from_utf8(read_fixed(&directory.join("cgroup.procs"), MANAGER_LIMIT)?)?;
            for line in text.lines() {
                let pid: u32 = line.parse().context("Malformed cgroup process identity")?;
                ensure!(
                    pid > 0 && pid <= i32::MAX as u32 && members.len() < MEMBER_LIMIT,
                    "Cgroup process list exceeds its bound"
                );
                members.insert(pid);
            }
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                ensure!(!kind.is_symlink(), "Symlink inside service cgroup");
                if kind.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
        Ok(members)
    }

    // One fixed read-only query, using nonblocking pipes rather than background
    // reader threads which could survive this bounded observation.
    fn control_group(service: &Service) -> Result<String> {
        let mut command = Command::new("/usr/bin/systemctl");
        command
            .args([
                "--user",
                "--no-pager",
                "--no-ask-password",
                "show",
                "--property=ControlGroup",
                "--value",
                "--",
                &service.name,
            ])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .context("Cannot observe the fixed service cgroup")?;
        let result = (|| -> Result<String> {
            let mut out = child.stdout.take().context("Missing manager output")?;
            let mut err = child
                .stderr
                .take()
                .context("Missing manager error output")?;
            for fd in [out.as_raw_fd(), err.as_raw_fd()] {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                ensure!(
                    flags >= 0
                        && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
                    "Cannot bound manager observation"
                );
            }
            let deadline =
                Instant::now() + crate::native_update_clock::cap(Duration::from_secs(5))?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut out_done = false;
            let mut err_done = false;
            loop {
                drain(&mut out, &mut stdout, &mut out_done)?;
                drain(&mut err, &mut stderr, &mut err_done)?;
                if let Some(status) = child.try_wait()?
                    && out_done
                    && err_done
                {
                    ensure!(status.success(), "Fixed manager cgroup query failed");
                    let text = String::from_utf8(stdout)?;
                    let group = text.strip_suffix('\n').unwrap_or(&text);
                    ensure!(!group.contains(['\r', '\n']), "Ambiguous native cgroup");
                    return Ok(group.to_string());
                }
                ensure!(
                    Instant::now() < deadline,
                    "Manager cgroup observation did not settle"
                );
                thread::sleep(Duration::from_millis(10));
            }
        })();
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result
    }
    fn drain(input: &mut impl Read, result: &mut Vec<u8>, done: &mut bool) -> Result<()> {
        let mut bytes = [0u8; 4096];
        loop {
            match input.read(&mut bytes) {
                Ok(0) => {
                    *done = true;
                    return Ok(());
                }
                Ok(n) => {
                    result.extend_from_slice(&bytes[..n]);
                    ensure!(
                        result.len() <= MANAGER_LIMIT,
                        "Manager output exceeds its bound"
                    );
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
    fn endpoint(config: &Config, runtime: &Value) -> Result<SocketAddr> {
        let port: u16 = runtime["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0)
            .context("Runtime lacks a concrete listener port")?;
        let scheme = match config.transport {
            crate::config::Transport::Https => "https",
            crate::config::Transport::LocalHttp => "http",
        };
        ensure!(
            runtime["provider_id"] == config.provider_id()
                && runtime["public_url"] == config.public_url
                && runtime["url"] == format!("{scheme}://127.0.0.1:{port}")
                && (config.port == 0 || config.port == port),
            "Runtime endpoint does not match the installation"
        );
        let bind: IpAddr = config.bind.parse()?;
        let target = match bind {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            ip => ip,
        };
        Ok(SocketAddr::new(target, port))
    }
    fn listening_inodes(bytes: &[u8], listener: SocketAddr) -> Result<BTreeSet<u64>> {
        let text = std::str::from_utf8(bytes)?;
        let mut found = BTreeSet::new();
        for line in text.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            ensure!(fields.len() >= 10, "Malformed native TCP table");
            let (local_ip, local_port) = fields[1]
                .rsplit_once(':')
                .context("Malformed native TCP endpoint")?;
            let local_port = u16::from_str_radix(local_port, 16)?;
            let ip = match local_ip.len() {
                8 => IpAddr::V4(Ipv4Addr::from(
                    u32::from_str_radix(local_ip, 16)?.to_ne_bytes(),
                )),
                32 => {
                    let mut bytes = [0u8; 16];
                    for (index, group) in local_ip.as_bytes().as_chunks::<8>().0.iter().enumerate()
                    {
                        bytes[index * 4..index * 4 + 4].copy_from_slice(
                            &u32::from_str_radix(std::str::from_utf8(group)?, 16)?.to_ne_bytes(),
                        );
                    }
                    IpAddr::V6(Ipv6Addr::from(bytes))
                }
                _ => anyhow::bail!("Malformed native TCP address"),
            };
            if fields[3] == "0A"
                && local_port == listener.port()
                && (ip == listener.ip()
                    || (ip.is_unspecified() && ip.is_ipv4() == listener.is_ipv4()))
            {
                found.insert(
                    fields[9]
                        .parse()
                        .context("Malformed native socket identity")?,
                );
            }
        }
        Ok(found)
    }
    fn listener_owned(pid: u32, listener: SocketAddr) -> Result<()> {
        ensure!(
            fs::read_link(format!("/proc/{pid}/ns/net"))? == fs::read_link("/proc/self/ns/net")?,
            "Original listener is in another network namespace"
        );
        let mut inodes =
            listening_inodes(&read_fixed(Path::new("/proc/net/tcp"), LIMIT)?, listener)?;
        inodes.extend(listening_inodes(
            &read_fixed(Path::new("/proc/net/tcp6"), LIMIT)?,
            listener,
        )?);
        ensure!(
            !inodes.is_empty(),
            "Captured runtime has no observed listening socket"
        );
        let mut count = 0;
        for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
            count += 1;
            ensure!(
                count <= MEMBER_LIMIT,
                "Original process descriptor list exceeds its bound"
            );
            let target = fs::read_link(entry?.path())?;
            if target
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|inode| inodes.contains(&inode))
            {
                return Ok(());
            }
        }
        anyhow::bail!("Original main generation does not own the captured listener")
    }
    // A known descendant outside the captured service cgroup invalidates the
    // stop boundary. Cgroup members additionally retain reparented processes;
    // ancestry alone would lose those after their immediate parent exits.
    fn descendants_within_group(main: &Value, members: &[Value]) -> Result<()> {
        let known: BTreeMap<u32, &Value> = members
            .iter()
            .map(|m| {
                Ok((
                    u32::try_from(m["pid"].as_u64().context("Missing member PID")?)?,
                    m,
                ))
            })
            .collect::<Result<_>>()?;
        let mut pending = vec![main.clone()];
        let mut seen = BTreeSet::new();
        let mut task_count = 0usize;
        while let Some(parent) = pending.pop() {
            let pid = u32::try_from(parent["pid"].as_u64().context("Missing parent PID")?)?;
            if !seen.insert(pid) {
                continue;
            }
            ensure!(
                seen.len() <= MEMBER_LIMIT && process::matches(&parent)?,
                "Original ancestry changed"
            );
            for task in fs::read_dir(format!("/proc/{pid}/task"))? {
                task_count += 1;
                ensure!(
                    task_count <= MEMBER_LIMIT,
                    "Original task inventory exceeds its bound"
                );
                let task = task?;
                ensure!(
                    task.file_name()
                        .to_str()
                        .is_some_and(|s| s.parse::<u32>().is_ok_and(|p| p > 0)),
                    "Unknown native task identity"
                );
                let children =
                    String::from_utf8(read_fixed(&task.path().join("children"), MANAGER_LIMIT)?)?;
                for child in children.split_whitespace() {
                    let child: u32 = child.parse()?;
                    let generation = known
                        .get(&child)
                        .context("Original descendant is outside its service cgroup")?;
                    ensure!(
                        process::parent_identity(generation)?.as_ref() == Some(&parent),
                        "Original descendant ancestry changed"
                    );
                    pending.push((*generation).clone());
                }
            }
            ensure!(
                process::matches(&parent)?,
                "Original ancestry changed while reading"
            );
        }
        Ok(())
    }
    fn original_command(pid: u32, executable: &Path, home: &Path) -> Result<()> {
        let bytes = read_fixed(
            &PathBuf::from(format!("/proc/{pid}/cmdline")),
            MANAGER_LIMIT,
        )?;
        let args: Vec<_> = bytes.split(|b| *b == 0).filter(|a| !a.is_empty()).collect();
        let exe = executable.as_os_str().as_encoded_bytes();
        let home = home.as_os_str().as_encoded_bytes();
        ensure!(
            args == [exe, b"--home", home, b"serve"] || args == [exe, b"serve", b"--home", home],
            "Original process command does not match the fixed installation"
        );
        Ok(())
    }

    /// Capture only; the installation owner performs any later maintenance Stop.
    pub fn capture(home: &Path, transaction: &str) -> Result<Value> {
        ensure!(
            uuid::Uuid::parse_str(transaction)?.to_string() == transaction,
            "Invalid adoption transaction"
        );
        let (config, config_bytes) = configuration(home)?;
        let service = service(&config.id)?;
        let path = service
            .definition_path
            .as_ref()
            .context("Missing fixed definition path")?;
        ensure!(
            dunce::canonicalize(path)? == *path,
            "Custom unit path is redirected"
        );
        let definition_bytes = read_owned(path, false)?;
        let definition = String::from_utf8(definition_bytes.clone())?;
        let native = installation::snapshot(&service)?;
        require_native_profile(&native, &service, &sha(&definition_bytes))?;
        ensure!(
            native["active"] == true && native["native"]["SubState"] == "running",
            "Capture requires the original running service before owner Stop"
        );
        let main = main_identity(&native)?;
        let pid = main["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .context("Missing main PID")?;
        let group = control_group(&service)?;
        let group_path = cgroup_path(&group, &service.name)?;
        let member_pids = cgroup_members(&group_path, false)?;
        ensure!(
            member_pids.contains(&pid),
            "Original main process is outside its manager cgroup"
        );
        let mut members = Vec::new();
        for member in &member_pids {
            let identity = process::identity(*member)?
                .context("Original cgroup generation vanished during capture")?;
            let membership = String::from_utf8(read_fixed(
                &PathBuf::from(format!("/proc/{member}/cgroup")),
                MANAGER_LIMIT,
            )?)?;
            ensure!(
                membership
                    .lines()
                    .any(
                        |line| line.strip_prefix("0::").is_some_and(|path| path == group
                            || path
                                .strip_prefix(&group)
                                .is_some_and(|suffix| suffix.starts_with('/')))
                    ),
                "Original cgroup membership changed"
            );
            members.push(identity);
        }
        descendants_within_group(main, &members)?;
        let executable = amplifier_native_platform::installed_binary(&APPLICATION, home);
        ensure!(
            fs::read_link(format!("/proc/{pid}/exe"))? == executable,
            "Original main image is not the fixed installed executable"
        );
        original_command(pid, &executable, home)?;
        let binary_sha256 = image(&executable)?;
        // The proc image can differ from the named image after an uncoordinated
        // replacement. Compare both while retaining the original generation.
        let mut process_image = File::open(format!("/proc/{pid}/exe"))?;
        let mut digest = Sha256::new();
        let copied = std::io::copy(
            &mut process_image.by_ref().take(IMAGE_LIMIT + 1),
            &mut digest,
        )?;
        ensure!(
            copied <= IMAGE_LIMIT && format!("{:x}", digest.finalize()) == binary_sha256,
            "Original running image differs from installed bytes"
        );
        let runtime_bytes = read_owned(&home.join("runtime.json"), true)?;
        let runtime: Value = serde_json::from_slice(&runtime_bytes)?;
        let listener = endpoint(&config, &runtime)?;
        listener_owned(pid, listener)?;
        descendants_within_group(main, &members)?;
        ensure!(
            installation::snapshot(&service)? == native,
            "Original native service snapshot changed during capture"
        );
        ensure!(
            control_group(&service)? == group,
            "Original service cgroup changed during capture"
        );
        ensure!(
            cgroup_members(&group_path, false)? == member_pids,
            "Original service cgroup members changed during capture"
        );
        ensure!(
            members
                .iter()
                .map(process::matches)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .all(|matched| matched),
            "Original service process generation changed during capture"
        );
        ensure!(
            read_owned(path, false)? == definition_bytes,
            "Original service definition changed during capture"
        );
        ensure!(
            read_owned(&home.join("config.json"), true)? == config_bytes,
            "Original configuration changed during capture"
        );
        ensure!(
            read_owned(&home.join("runtime.json"), true)? == runtime_bytes,
            "Original runtime record changed during capture"
        );
        let origin = Origin {
            schema: 1,
            transaction_id: transaction.into(),
            installation_id: config.id,
            home: home.into(),
            account: account(),
            executable,
            binary_sha256,
            config_sha256: sha(&config_bytes),
            service,
            definition,
            definition_sha256: sha(&definition_bytes),
            native_snapshot: native,
            control_group: group,
            members,
            listener,
            runtime_sha256: sha(&runtime_bytes),
            original_running: true,
        };
        let value = serde_json::to_value(origin)?;
        decode(&value)?;
        Ok(value)
    }

    /// Current definition/policy proof only: a candidate may now occupy the same
    /// fixed executable path. This does not require the original image bytes.
    pub fn validate(home: &Path, origin: &Value) -> Result<Service> {
        let decoded = decode(origin)?;
        bound(home, &decoded)?;
        let bytes = read_owned(
            decoded
                .service
                .definition_path
                .as_ref()
                .context("Missing definition")?,
            false,
        )?;
        ensure!(
            bytes == decoded.definition.as_bytes(),
            "Original custom unit changed"
        );
        native_profile(origin, &installation::snapshot(&decoded.service)?)?;
        Ok(decoded.service)
    }
    fn stopped(snapshot: &Value) -> Result<()> {
        ensure!(
            snapshot["active"] == false
                && snapshot["transitioning"] == false
                && snapshot["processes"].as_array().is_some_and(Vec::is_empty)
                && matches!(
                    snapshot["native"]["ActiveState"].as_str(),
                    Some("inactive" | "failed")
                )
                && snapshot["native"]["MainPID"] == "0"
                && snapshot["native"]["ControlPID"] == "0",
            "Original service is not conclusively stopped"
        );
        Ok(())
    }
    fn listener_refused(result: std::io::Result<TcpStream>) -> Result<()> {
        match result {
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => Ok(()),
            Ok(_) => anyhow::bail!("Original listener remains available"),
            Err(_) => anyhow::bail!("Original listener absence is unknown"),
        }
    }
    /// Caller must continuously hold the existing update and service locks.
    /// This observes stopped processes, not settlement of external effects.
    pub fn verify_stopped(home: &Path, origin: &Value) -> Result<()> {
        let decoded = decode(origin)?;
        ensure!(
            image(&decoded.executable)? == decoded.binary_sha256,
            "Original installed binary changed before adoption"
        );
        verify_absent(home, origin)
    }
    /// Same stopped boundary after a journaled image exchange. This confers no
    /// Start authority and deliberately does not require the old image bytes.
    pub fn verify_absent(home: &Path, origin: &Value) -> Result<()> {
        let decoded = decode(origin)?;
        let service = validate(home, origin)?;
        let before = installation::snapshot(&service)?;
        native_profile(origin, &before)?;
        stopped(&before)?;
        for original in &decoded.members {
            ensure!(
                !process::matches(original)?,
                "Original process generation is still present"
            );
        }
        let group = control_group(&service)?;
        ensure!(
            group.is_empty() || group == decoded.control_group,
            "Manager cgroup changed unexpectedly"
        );
        let path = cgroup_path(&decoded.control_group, &service.name)?;
        ensure!(
            cgroup_members(&path, true)?.is_empty(),
            "Original service cgroup still contains processes"
        );
        listener_refused(TcpStream::connect_timeout(
            &decoded.listener,
            crate::native_update_clock::cap(Duration::from_millis(500))?,
        ))?;
        let after = installation::snapshot(&service)?;
        native_profile(origin, &after)?;
        stopped(&after)?;
        ensure!(
            after == before && cgroup_members(&path, true)?.is_empty(),
            "Stopped origin changed during verification"
        );
        Ok(())
    }
    pub fn committed_definition(
        home: &Path,
        id: &str,
        fixed_path: &Path,
    ) -> Result<Option<String>> {
        let path = home.join(RECORD);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            result => {
                result?;
            }
        }
        let bytes = read_owned(&path, true)?;
        let wrapper: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            wrapper["schema"] == 1 && wrapper["kind"] == "legacy_adoption_origin",
            "Unknown legacy origin record"
        );
        let origin = decode(&wrapper["origin"])?;
        bound(home, &origin)?;
        ensure!(
            origin.installation_id == id
                && origin.service.definition_path.as_deref() == Some(fixed_path)
                && wrapper["transaction_id"] == origin.transaction_id,
            "Committed origin binding changed"
        );
        let installed_path = home.join("native-installation.json");
        match fs::symlink_metadata(&installed_path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            result => {
                result?;
            }
        }
        let installation_bytes = read_owned(&installed_path, true)?;
        let installed: Value = serde_json::from_slice(&installation_bytes)?;
        ensure!(
            installed["schema"] == 1
                && installed["id"] == id
                && installed["legacy_origin_sha256"] == sha(&bytes)
                && installed["package"].is_object()
                && wrapper["candidate"].is_object(),
            "Committed native installation does not bind this origin"
        );
        Ok(Some(origin.definition))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::{PermissionsExt, symlink};

        fn snapshot() -> Value {
            let id = "00000000-0000-4000-8000-000000000001";
            let service = service(id).unwrap();
            json!({"schema":1,"service":service,"definition":{"path":service.definition_path,"sha256":sha(b"unit"),"bytes":4},
                "effective_definition":{"text":"unit","sha256":sha(b"unit")},"definition_matches":true,"enabled":true,"start_allowed":true,
                "masked":false,"loaded":true,"active":true,"transitioning":false,
                "processes":[{"role":"MainPID","identity":{"pid":42,"start_ticks":"123","boot_id":"00000000-0000-4000-8000-000000000002"}}],
                "native":{"Id":service.name,"Names":service.name,"LoadState":"loaded","UnitFileState":"enabled","FragmentPath":service.definition_path,
                    "SourcePath":"","DropInPaths":"","NeedDaemonReload":"no","RefuseManualStart":"no","RefuseManualStop":"no",
                    "MainPID":"42","ControlPID":"0","InvocationID":"first","ActiveState":"active","SubState":"running",
                    "ExecStart":"{ path=/a ; argv[]=/a --home /h serve ; ignore_errors=no ; start_time=[now] ; stop_time=[n/a] ; pid=42 ; code=(null) ; status=0/0 }",
                    "ExecStop":"","Restart":"on-failure","KillMode":"control-group","Type":"simple"}})
        }
        fn origin() -> Value {
            let mut snapshot = snapshot();
            let home = PathBuf::from("/fixed/private/home");
            let executable = amplifier_native_platform::installed_binary(&APPLICATION, &home);
            snapshot["native"]["ExecStart"] = json!(format!(
                "{{ path={} ; argv[]={} --home {} serve ; ignore_errors=no ; start_time=[now] ; stop_time=[n/a] ; pid=42 ; code=(null) ; status=0/0 }}",
                executable.display(),
                executable.display(),
                home.display()
            ));
            serde_json::to_value(Origin {
                schema: 1,
                transaction_id: "00000000-0000-4000-8000-000000000003".into(),
                installation_id: "00000000-0000-4000-8000-000000000001".into(),
                home: home.clone(),
                account: account(),
                executable: amplifier_native_platform::installed_binary(&APPLICATION, &home),
                binary_sha256: sha(b"binary"),
                config_sha256: sha(b"config"),
                service: serde_json::from_value(snapshot["service"].clone()).unwrap(),
                definition: "unit".into(),
                definition_sha256: sha(b"unit"),
                native_snapshot: snapshot.clone(),
                control_group: format!(
                    "/user.slice/user-{}.slice/{}",
                    uid(),
                    snapshot["service"]["name"].as_str().unwrap()
                ),
                members: vec![snapshot["processes"][0]["identity"].clone()],
                listener: "127.0.0.1:8443".parse().unwrap(),
                runtime_sha256: sha(b"runtime"),
                original_running: true,
            })
            .unwrap()
        }
        #[test]
        fn custom_profile_preserves_policy_and_ignores_only_runtime_generation() {
            let original = origin();
            let mut current = original["native_snapshot"].clone();
            current["active"] = json!(false);
            current["processes"] = json!([]);
            current["native"]["ActiveState"] = json!("inactive");
            current["native"]["SubState"] = json!("dead");
            current["native"]["MainPID"] = json!("0");
            current["native"]["InvocationID"] = json!("");
            current["native"]["ExecStart"] = json!(format!(
                "{{ {} ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}",
                command_identity(&original["native_snapshot"]["native"]["ExecStart"]).unwrap()
            ));
            native_profile(&original, &current).unwrap();
            stopped(&current).unwrap();
            for key in [
                "Restart",
                "KillMode",
                "Type",
                "UnitFileState",
                "ExecStop",
                "Names",
            ] {
                let mut changed = current.clone();
                changed["native"][key] = json!("changed");
                assert!(native_profile(&original, &changed).is_err(), "{key}");
            }
            for key in [
                "enabled",
                "start_allowed",
                "masked",
                "definition_matches",
                "loaded",
            ] {
                let mut changed = current.clone();
                changed[key] = json!("unknown");
                assert!(native_profile(&original, &changed).is_err(), "{key}");
            }
            let mut changed = current.clone();
            changed["native"]["ExecStart"] = json!(
                "{ path=/evil ; argv[]=/evil ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
            );
            assert!(native_profile(&original, &changed).is_err());
        }
        #[test]
        fn missing_foreign_and_forged_origin_are_refused() {
            for field in [
                "transaction_id",
                "installation_id",
                "account",
                "binary_sha256",
                "definition_sha256",
                "control_group",
            ] {
                let mut changed = origin();
                changed[field] = json!("invalid");
                assert!(decode(&changed).is_err(), "{field}");
            }
            for field in ["original_running", "members", "native_snapshot"] {
                let mut changed = origin();
                changed[field] = Value::Null;
                assert!(decode(&changed).is_err(), "{field}");
            }
            let mut changed = origin();
            changed["members"][0]["start_ticks"] = json!("0");
            assert!(decode(&changed).is_err());
            let mut changed = origin();
            changed["definition"] = json!("drift");
            assert!(decode(&changed).is_err());
            let mut changed = origin();
            changed["extra"] = json!(true);
            assert!(decode(&changed).is_err());
        }
        #[test]
        fn stopped_never_accepts_unknown_or_remaining_native_processes() {
            let mut value = snapshot();
            value["active"] = json!(false);
            value["processes"] = json!([]);
            value["native"]["ActiveState"] = json!("inactive");
            value["native"]["MainPID"] = json!("0");
            value["native"]["SubState"] = json!("dead");
            stopped(&value).unwrap();
            for key in ["active", "transitioning", "processes"] {
                let mut bad = value.clone();
                bad[key] = Value::Null;
                assert!(stopped(&bad).is_err());
            }
            let mut bad = value.clone();
            bad["native"]["ControlPID"] = json!("42");
            assert!(stopped(&bad).is_err());
            let mut bad = value;
            bad["processes"] = snapshot()["processes"].clone();
            assert!(stopped(&bad).is_err());
            assert!(
                listener_refused(Err(std::io::Error::from(ErrorKind::ConnectionRefused))).is_ok()
            );
            for error in [
                ErrorKind::TimedOut,
                ErrorKind::WouldBlock,
                ErrorKind::PermissionDenied,
                ErrorKind::ConnectionReset,
            ] {
                assert!(listener_refused(Err(std::io::Error::from(error))).is_err());
            }
        }
        #[test]
        fn cgroup_paths_are_bound_and_no_links_are_followed() {
            assert!(cgroup_path("/user.slice/app.service", "app.service").is_ok());
            for path in [
                "/app.service",
                "/user.slice/../app.service",
                "user.slice/app.service",
                "/user.slice/other.service",
                "/user.slice/app.service\n",
            ] {
                assert!(cgroup_path(path, "app.service").is_err(), "{path}");
            }
            let temp = tempfile::tempdir().unwrap();
            let real = temp.path().join("real");
            fs::create_dir(&real).unwrap();
            let link = temp.path().join("link");
            symlink(&real, &link).unwrap();
            assert!(ordinary_directory(&link).is_err());
        }
        #[test]
        fn only_exact_native_receipt_commits_immutable_origin_and_future_packages_keep_binding() {
            let temp = tempfile::tempdir().unwrap();
            crate::config::private_dir(temp.path()).unwrap();
            let mut config = Config::local(temp.path(), "fixture", 8443).unwrap();
            config.id = "00000000-0000-4000-8000-000000000001".into();
            config.save(temp.path()).unwrap();
            let mut record = origin();
            record["home"] = json!(temp.path());
            let executable = amplifier_native_platform::installed_binary(&APPLICATION, temp.path());
            record["executable"] = json!(executable);
            record["native_snapshot"]["native"]["ExecStart"] = json!(format!(
                "{{ path={} ; argv[]={} --home {} serve ; ignore_errors=no ; start_time=[now] ; stop_time=[n/a] ; pid=42 ; code=(null) ; status=0/0 }}",
                executable.display(),
                executable.display(),
                temp.path().display()
            ));
            let fixed = service(&config.id).unwrap().definition_path.unwrap();
            let wrapper = json!({"schema":1,"kind":"legacy_adoption_origin","transaction_id":record["transaction_id"],
                "origin":record,"snapshot":{},"candidate":{"original_native_package":true}});
            let bytes = serde_json::to_vec(&wrapper).unwrap();
            crate::config::private_write(&temp.path().join(RECORD), &bytes).unwrap();
            assert!(
                committed_definition(temp.path(), &config.id, &fixed)
                    .unwrap()
                    .is_none()
            );
            let receipt = json!({"schema":1,"id":config.id,"package":{"future_native_package":true},"legacy_origin_sha256":sha(&bytes)});
            let installed = temp.path().join("native-installation.json");
            crate::config::private_write(&installed, &serde_json::to_vec(&receipt).unwrap())
                .unwrap();
            assert_eq!(
                committed_definition(temp.path(), &config.id, &fixed).unwrap(),
                Some("unit".into())
            );
            let mut bad = receipt.clone();
            bad["legacy_origin_sha256"] = json!(sha(b"other"));
            crate::config::private_write(&installed, &serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(committed_definition(temp.path(), &config.id, &fixed).is_err());
            assert_eq!(fs::read(temp.path().join(RECORD)).unwrap(), bytes);
            crate::config::private_write(&installed, &serde_json::to_vec(&receipt).unwrap())
                .unwrap();
            assert!(
                committed_definition(temp.path(), "00000000-0000-4000-8000-000000000004", &fixed)
                    .is_err()
            );
            assert!(
                committed_definition(temp.path(), &config.id, Path::new("/other/unit")).is_err()
            );
        }
        #[test]
        fn loaded_command_cannot_change_the_fixed_start_target() {
            let original = origin();
            let mut changed = original.clone();
            changed["native_snapshot"]["native"]["ExecStart"] =
                snapshot()["native"]["ExecStart"].clone();
            assert!(decode(&changed).is_err());
            let mut changed = original;
            let command = changed["native_snapshot"]["native"]["ExecStart"]
                .as_str()
                .unwrap()
                .replace("ignore_errors=no", "ignore_errors=yes");
            changed["native_snapshot"]["native"]["ExecStart"] = json!(command);
            assert!(decode(&changed).is_err());
        }
        #[test]
        fn unsafe_existing_origin_file_is_preserved() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("origin");
            fs::write(&path, b"original").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(read_owned(&path, true).unwrap(), b"original");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_owned(&path, true).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"original");
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
            let link = temp.path().join("link");
            symlink(&path, &link).unwrap();
            assert!(read_owned(&link, false).is_err());
        }
        #[test]
        fn runtime_and_tcp_projection_do_not_invent_process_binding() {
            let temp = tempfile::tempdir().unwrap();
            let config = Config::local(temp.path(), "fixture", 8443).unwrap();
            let runtime = json!({"port":8443,"url":"http://127.0.0.1:8443","public_url":config.public_url,"provider_id":config.provider_id()});
            assert_eq!(
                endpoint(&config, &runtime).unwrap(),
                "127.0.0.1:8443".parse::<SocketAddr>().unwrap()
            );
            let mut bad = runtime.clone();
            bad["provider_id"] = json!("web:other");
            assert!(endpoint(&config, &bad).is_err());
            let mut bad = runtime;
            bad["port"] = json!(8444);
            assert!(endpoint(&config, &bad).is_err());
            let table=b"header\n0: 0100007F:20FB 00000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 12345\n";
            assert_eq!(
                listening_inodes(table, "127.0.0.1:8443".parse().unwrap()).unwrap(),
                BTreeSet::from([12345])
            );
            assert!(
                listening_inodes(table, "127.0.0.1:8444".parse().unwrap())
                    .unwrap()
                    .is_empty()
            );
            assert!(
                listening_inodes(b"header\nbroken\n", "127.0.0.1:8443".parse().unwrap()).is_err()
            );
        }
    }
}
