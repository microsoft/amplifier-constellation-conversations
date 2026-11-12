//! Capture and restore only Companion's fixed current-user startup registration.
//! No definitions, enabled flags, masks, accounts or scheduler entries are edited.
//! Absence/inactivity means the updater must retain its manual-process lifecycle.
use crate::{config::Config, native_bundle::APPLICATION};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Definition {
    path: Option<PathBuf>,
    kind: String,
    text: String,
    sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    home: PathBuf,
    id: String,
    label: String,
    platform: String,
    account: String,
    executable: PathBuf,
    registered: bool,
    managed: bool,
    definition: Option<Definition>,
    settings: Value,
    original_state: Value,
    manager_pid: Option<u32>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn output(command: &mut Command) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .context("Cannot run the fixed OS startup manager")?;
    let out = child.stdout.take().context("Missing startup output")?;
    let err = child
        .stderr
        .take()
        .context("Missing startup error output")?;
    let read = |pipe: Box<dyn Read + Send>| {
        thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            pipe.take((OUTPUT_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })
    };
    let out = read(Box::new(out));
    let err = read(Box::new(err));
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("OS startup manager did not settle within its bounded operation");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("Startup output reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("Startup error reader failed"))??;
    ensure!(
        stdout.len() <= OUTPUT_LIMIT && stderr.len() <= OUTPUT_LIMIT,
        "Startup manager output exceeded its bound"
    );
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
fn checked(command: &mut Command) -> Result<String> {
    let result = output(command)?;
    ensure!(
        result.status.success(),
        "Fixed OS startup operation failed ({}): {}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
            .chars()
            .take(512)
            .collect::<String>()
    );
    Ok(String::from_utf8(result.stdout)?
        .trim_start_matches('\u{feff}')
        .to_owned())
}
fn account() -> Result<String> {
    #[cfg(unix)]
    {
        Ok(format!("uid:{}", unsafe { libc::geteuid() }))
    }
    #[cfg(windows)]
    {
        amplifier_native_platform::current_user_sid()
    }
}
fn identity(home: &Path, id: &str) -> Result<Snapshot> {
    ensure!(
        home.is_absolute() && uuid::Uuid::parse_str(id)?.to_string() == id,
        "Invalid startup installation identity"
    );
    let home = dunce::canonicalize(home)?;
    ensure!(
        Config::load(&home)?.id == id,
        "Startup installation identity changed"
    );
    let executable = amplifier_native_platform::installed_binary(&APPLICATION, &home);
    Ok(Snapshot {
        schema: 1,
        home,
        id: id.into(),
        label: format!("{}-{id}", APPLICATION.startup_prefix),
        platform: std::env::consts::OS.into(),
        account: account()?,
        executable,
        registered: false,
        managed: false,
        definition: None,
        settings: json!({}),
        original_state: json!({"running":false,"loaded":false}),
        manager_pid: None,
    })
}
#[cfg(unix)]
fn definition(path: &Path, expected: &str, mask: bool) -> Result<Option<Definition>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    use std::os::unix::fs::MetadataExt;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "Startup definition belongs to another account"
    );
    if metadata.file_type().is_symlink() {
        ensure!(
            mask && fs::read_link(path)? == Path::new("/dev/null"),
            "Unexpected startup definition link; preserved"
        );
        return Ok(Some(Definition {
            path: Some(path.to_owned()),
            kind: "mask".into(),
            text: "/dev/null".into(),
            sha256: hash(b"/dev/null"),
        }));
    }
    ensure!(
        metadata.is_file() && metadata.len() <= OUTPUT_LIMIT as u64,
        "Invalid startup definition"
    );
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes.clone())?;
    ensure!(
        text == expected,
        "Startup definition was customized; it was preserved"
    );
    Ok(Some(Definition {
        path: Some(path.to_owned()),
        kind: "file".into(),
        text,
        sha256: hash(&bytes),
    }))
}
#[cfg(unix)]
fn previously_registered(s: &Snapshot) -> Result<bool> {
    let path = s.home.join("native-update.json");
    if !path.exists() {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() <= OUTPUT_LIMIT as u64,
        "Invalid retained startup history"
    );
    let journal: Value = serde_json::from_slice(&fs::read(path)?)?;
    for key in ["startup", "previous_startup"] {
        let previous = &journal[key];
        if previous["registered"] == true {
            ensure!(
                previous["id"] == s.id && previous["home"] == json!(s.home),
                "Retained startup belongs to another installation"
            );
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn systemctl() -> Result<PathBuf> {
    for path in ["/usr/bin/systemctl", "/bin/systemctl"] {
        if Path::new(path).is_file() {
            return Ok(PathBuf::from(path));
        }
    }
    anyhow::bail!("The current-user service manager is unavailable")
}
#[cfg(target_os = "linux")]
fn linux_command(args: &[&str]) -> Result<Command> {
    let mut command = Command::new(systemctl()?);
    command
        .args(["--user", "--no-pager"])
        .args(args)
        .env("LC_ALL", "C");
    // Native apps launched from a GUI or a clean application environment need
    // not inherit a shell's D-Bus variables. Bind the fallback to this OS UID's
    // existing manager endpoint; never create a bus or select another account.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })));
    if runtime.is_dir() {
        command.env("XDG_RUNTIME_DIR", &runtime);
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none_or(|s| s.is_empty())
            && runtime.join("bus").exists()
        {
            command.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", runtime.join("bus").display()),
            );
        }
    }
    Ok(command)
}
#[cfg(target_os = "linux")]
fn properties(text: &str) -> Result<std::collections::BTreeMap<String, String>> {
    let mut properties = std::collections::BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .context("Malformed service-manager property")?;
        ensure!(
            properties.insert(key.into(), value.into()).is_none(),
            "Duplicate service-manager property"
        );
    }
    Ok(properties)
}
#[cfg(target_os = "linux")]
fn absent_unit(text: &str) -> Result<()> {
    let p = properties(text)?;
    ensure!(
        p.get("LoadState").map(String::as_str) == Some("not-found")
            && p.get("ActiveState").map(String::as_str) == Some("inactive")
            && p.get("MainPID").map(String::as_str) == Some("0")
            && p.get("FragmentPath").is_some_and(String::is_empty),
        "The fixed startup label remains loaded or has another definition; preserved"
    );
    Ok(())
}
#[cfg(target_os = "linux")]
fn user_manager_endpoint() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    let mut roots = vec![PathBuf::from(format!("/run/user/{}", unsafe {
        libc::geteuid()
    }))];
    if let Some(path) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        roots.push(path);
    }
    roots.iter().any(|root| {
        [root.join("bus"), root.join("systemd/private")]
            .iter()
            .any(|path| fs::symlink_metadata(path).is_ok())
    })
}
#[cfg(target_os = "linux")]
fn inspect_missing_definition(s: &mut Snapshot, unit: &str) -> Result<()> {
    if systemctl().is_err() || !user_manager_endpoint() {
        ensure!(
            !previously_registered(s)?,
            "A previously captured startup manager is unavailable; preserved"
        );
        // A bare/minimal userland must remain a standalone application. This is
        // an explicit lack-of-manager observation, not historical absence proof.
        s.settings = json!({"manager":"unavailable","reason":"no_user_manager_endpoint_or_tool"});
        return Ok(());
    }
    let result = output(&mut linux_command(&[
        "show",
        unit,
        "--property=LoadState",
        "--property=ActiveState",
        "--property=MainPID",
        "--property=FragmentPath",
    ])?)?;
    // Some systemd versions return a nonzero status for not-found. The exact
    // typed properties, never the exit code alone, establish that fixed absence.
    absent_unit(&String::from_utf8(result.stdout)?)?;
    s.settings = json!({"manager":"available","reason":"fixed_label_absent"});
    Ok(())
}
#[cfg(target_os = "linux")]
fn inspect_platform(s: &mut Snapshot) -> Result<()> {
    let unit = format!("{}.service", s.label);
    let path = amplifier_native_platform::user_home()?
        .join(".config/systemd/user")
        .join(&unit);
    let expected = amplifier_native_platform::systemd_unit(&APPLICATION, &s.executable, &s.home)?;
    let Some(definition) = definition(&path, &expected, true)? else {
        return inspect_missing_definition(s, &unit);
    };
    let text = checked(&mut linux_command(&[
        "show",
        &unit,
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=MainPID",
        "--property=UnitFileState",
        "--property=FragmentPath",
        "--property=DropInPaths",
        "--property=NeedDaemonReload",
    ])?)?;
    let p = properties(&text)?;
    let get = |key: &str| -> Result<&str> {
        p.get(key)
            .map(String::as_str)
            .context("Missing service-manager property")
    };
    let masked = definition.kind == "mask";
    ensure!(
        get("DropInPaths")?.is_empty(),
        "Startup has unreviewed drop-ins; preserved"
    );
    ensure!(
        get("NeedDaemonReload")? == "no",
        "Loaded startup configuration differs from its retained definition"
    );
    ensure!(
        if masked {
            get("LoadState")? == "masked"
        } else {
            get("LoadState")? == "loaded"
        },
        "Startup loading state is unknown"
    );
    ensure!(
        get("FragmentPath")?
            == if masked {
                "/dev/null"
            } else {
                path.to_str().context("Invalid definition path")?
            },
        "Loaded startup definition differs"
    );
    let unit_state = get("UnitFileState")?;
    ensure!(
        matches!(
            unit_state,
            "enabled"
                | "enabled-runtime"
                | "disabled"
                | "static"
                | "indirect"
                | "masked"
                | "masked-runtime"
        ),
        "Startup enabled/mask state is unknown"
    );
    ensure!(
        masked == matches!(unit_state, "masked" | "masked-runtime"),
        "Startup mask state changed"
    );
    let active = get("ActiveState")?;
    let sub = get("SubState")?;
    let pid: u32 = get("MainPID")?.parse()?;
    let running = active == "active" && sub == "running" && pid > 0;
    ensure!(
        running || (matches!(active, "inactive" | "failed") && pid == 0),
        "Startup is transitioning or has unknown process state"
    );
    ensure!(
        !running || !masked,
        "Cannot restore an active masked startup registration without changing its mask"
    );
    s.registered = true;
    s.managed = running;
    s.definition = Some(definition);
    s.settings = json!({"load_state":get("LoadState")?,"unit_file_state":unit_state,"masked":masked,"fragment":get("FragmentPath")?,"drop_ins":get("DropInPaths")?});
    s.original_state =
        json!({"running":running,"loaded":true,"active":active,"substate":sub,"pid":pid});
    s.manager_pid = (pid > 0).then_some(pid);
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_command(args: &[&str]) -> Command {
    let mut command = Command::new("/bin/launchctl");
    command.args(args).env("LC_ALL", "C");
    command
}
#[cfg(target_os = "macos")]
fn launch_field(text: &str, field: &str) -> Result<Option<String>> {
    let prefix = format!("{field} = ");
    let values: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix(&prefix))
        .collect();
    ensure!(values.len() <= 1, "Ambiguous LaunchAgent state");
    Ok(values
        .first()
        .map(|value| value.trim_matches('"').to_string()))
}
#[cfg(target_os = "macos")]
fn disabled_override(text: &str, label: &str) -> Result<Option<bool>> {
    let prefix = format!("\"{label}\" => ");
    let rows: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix(&prefix))
        .collect();
    ensure!(rows.len() <= 1, "Ambiguous LaunchAgent disabled override");
    rows.first()
        .map(|value| match value.trim_end_matches(',') {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => anyhow::bail!("Invalid LaunchAgent disabled override"),
        })
        .transpose()
}
#[cfg(target_os = "macos")]
fn launch_arguments(text: &str) -> Result<Vec<String>> {
    let mut arguments = None;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "arguments = {" {
            continue;
        }
        ensure!(
            arguments.is_none(),
            "Ambiguous loaded LaunchAgent arguments"
        );
        let mut values = Vec::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim() == "}" {
                closed = true;
                break;
            }
            values.push(line.trim().to_owned());
        }
        ensure!(closed, "Unterminated loaded LaunchAgent arguments");
        arguments = Some(values);
    }
    arguments.context("Missing loaded LaunchAgent arguments")
}
#[cfg(target_os = "macos")]
fn inspect_platform(s: &mut Snapshot) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::geteuid() });
    let target = format!("{domain}/{}", s.label);
    let path = amplifier_native_platform::user_home()?
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", s.label));
    let expected =
        amplifier_native_platform::launch_agent(&APPLICATION, &s.label, &s.executable, &s.home)?;
    let Some(definition) = definition(&path, &expected, false)? else {
        let result = output(&mut mac_command(&["print", &target]))?;
        ensure!(
            !result.status.success(),
            "Fixed LaunchAgent is loaded without its owned definition; preserved"
        );
        let error = String::from_utf8(result.stderr)?;
        if error.contains("Could not find service") && error.contains(&s.label) {
            s.settings = json!({"manager":"available","reason":"fixed_label_absent"});
        } else if error.contains("Could not find domain") {
            ensure!(
                !previously_registered(s)?,
                "Previously captured GUI startup domain is unavailable; preserved"
            );
            s.settings = json!({"manager":"unavailable","reason":"no_gui_domain"});
        } else {
            anyhow::bail!("Cannot establish whether the fixed LaunchAgent remains loaded");
        }
        return Ok(());
    };
    // A successful override query proves the current account's GUI domain is
    // available without dumping unrelated services or their environments.
    let overrides = checked(&mut mac_command(&["print-disabled", &domain]))?;
    let disabled = disabled_override(&overrides, &s.label)?;
    let result = output(&mut mac_command(&["print", &target]))?;
    let mut running = false;
    let mut pid = None;
    let mut state = "unloaded".to_owned();
    let loaded = result.status.success();
    if loaded {
        let text = String::from_utf8(result.stdout)?;
        ensure!(
            launch_field(&text, "path")?.as_deref() == path.to_str(),
            "Loaded LaunchAgent definition differs"
        );
        ensure!(
            launch_field(&text, "program")?.as_deref() == s.executable.to_str(),
            "Loaded LaunchAgent executable differs"
        );
        ensure!(
            launch_arguments(&text)?
                == vec![
                    s.executable
                        .to_str()
                        .context("Invalid executable path")?
                        .to_owned(),
                    "--home".into(),
                    s.home
                        .to_str()
                        .context("Invalid installation path")?
                        .to_owned(),
                    "serve".into()
                ],
            "Loaded LaunchAgent command differs"
        );
        state = launch_field(&text, "state")?.context("Missing LaunchAgent state")?;
        pid = launch_field(&text, "pid")?
            .map(|v| v.parse::<u32>())
            .transpose()?;
        running = state == "running" && pid.is_some_and(|v| v > 0);
        ensure!(
            running
                || (matches!(state.as_str(), "waiting" | "not running" | "exited")
                    && pid.is_none_or(|p| p == 0)),
            "LaunchAgent is transitioning or unknown"
        );
    } else {
        let error = String::from_utf8(result.stderr)?;
        ensure!(
            error.contains("Could not find service") && error.contains(&s.label),
            "LaunchAgent query failed for an unknown reason"
        );
    }
    ensure!(
        !running || disabled != Some(true),
        "Cannot restore an active disabled LaunchAgent without changing its policy"
    );
    s.registered = true;
    s.managed = running;
    s.definition = Some(definition);
    s.settings = json!({"domain":domain,"disabled_override":disabled});
    s.original_state = json!({"running":running,"loaded":loaded,"state":state,"pid":pid});
    s.manager_pid = pid;
    Ok(())
}

#[cfg(windows)]
fn powershell() -> Result<PathBuf> {
    // GetSystemDirectoryW avoids PATH and user-selected command locations.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, length: u32) -> u32;
    }
    let mut buffer = vec![0u16; 32768];
    let count = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    ensure!(
        count > 0 && count < buffer.len(),
        "Cannot locate the Windows system directory"
    );
    let path = PathBuf::from(String::from_utf16(&buffer[..count])?)
        .join("WindowsPowerShell/v1.0/powershell.exe");
    ensure!(
        path.is_absolute() && path.is_file(),
        "Windows startup manager is unavailable"
    );
    Ok(path)
}
#[cfg(windows)]
fn task(label: &str, action: &str) -> Result<Value> {
    ensure!(
        matches!(action, "query" | "stop" | "start"),
        "Invalid fixed task operation"
    );
    const SCRIPT: &str = r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false);
$s=New-Object -ComObject 'Schedule.Service'; $s.Connect();
try {$t=$s.GetFolder('\').GetTask($env:CONSTELLATION_NATIVE_STARTUP_LABEL)}
catch {if ($_.Exception.HResult -eq -2147024894 -or $_.Exception.InnerException.HResult -eq -2147024894) {'null'; exit 0}; throw}
if ($env:CONSTELLATION_NATIVE_STARTUP_ACTION -eq 'stop') {$t.Stop(0)}
elseif ($env:CONSTELLATION_NATIVE_STARTUP_ACTION -eq 'start') {$null=$t.Run($null)}
elseif ($env:CONSTELLATION_NATIVE_STARTUP_ACTION -ne 'query') {throw 'Invalid operation'}
$instances=$t.GetInstances(0);
@{xml=$t.Xml; state=[int]$t.State; enabled=[bool]$t.Enabled; instances=[int]$instances.Count} | ConvertTo-Json -Compress"#;
    let mut command = Command::new(powershell()?);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .env("CONSTELLATION_NATIVE_STARTUP_LABEL", label)
        .env("CONSTELLATION_NATIVE_STARTUP_ACTION", action);
    Ok(serde_json::from_str(&checked(&mut command)?)?)
}
#[cfg(windows)]
fn inspect_platform(s: &mut Snapshot) -> Result<()> {
    let task = task(&s.label, "query")?;
    if task.is_null() {
        return Ok(());
    }
    let document = task["xml"]
        .as_str()
        .context("Missing native task definition")?;
    ensure!(
        amplifier_native_platform::task_matches(
            &APPLICATION,
            document,
            &s.executable,
            &s.home,
            &s.account
        )?,
        "Task account or fixed command differs; preserved"
    );
    let state = task["state"]
        .as_u64()
        .context("Missing native task state")?;
    let enabled = task["enabled"]
        .as_bool()
        .context("Missing task enabled state")?;
    let instances = task["instances"]
        .as_u64()
        .context("Missing running task instances")?;
    ensure!(
        matches!(state, 1 | 3 | 4) && instances <= 1,
        "Task is queued, transitioning or has ambiguous instances"
    );
    let running = state == 4 && instances == 1;
    ensure!(
        running || (state != 4 && instances == 0),
        "Task process state is uncertain"
    );
    ensure!(
        !running || enabled,
        "Cannot restore a running disabled task without changing its policy"
    );
    s.registered = true;
    s.managed = running;
    s.definition = Some(Definition {
        path: None,
        kind: "task-xml".into(),
        text: document.into(),
        sha256: hash(document.as_bytes()),
    });
    s.settings = json!({"enabled":enabled,"account":s.account});
    s.original_state = json!({"running":running,"loaded":true,"state":state,"instances":instances});
    Ok(())
}

fn inspect(home: &Path, id: &str) -> Result<Snapshot> {
    let mut result = identity(home, id)?;
    inspect_platform(&mut result)?;
    Ok(result)
}
fn decode(value: &Value) -> Result<Snapshot> {
    let s: Snapshot = serde_json::from_value(value.clone())?;
    let fixed = identity(&s.home, &s.id)?;
    ensure!(
        s.schema == 1
            && s.label == fixed.label
            && s.platform == fixed.platform
            && s.account == fixed.account
            && s.executable == fixed.executable,
        "Startup snapshot identity or account changed"
    );
    ensure!(
        s.managed == (s.registered && s.original_state["running"] == true),
        "Invalid startup ownership snapshot"
    );
    ensure!(
        s.registered == s.definition.is_some(),
        "Invalid startup definition snapshot"
    );
    if let Some(d) = &s.definition {
        ensure!(
            hash(d.text.as_bytes()) == d.sha256,
            "Startup definition snapshot changed"
        );
    }
    Ok(s)
}
fn compare(saved: &Snapshot, current: &Snapshot) -> Result<()> {
    ensure!(
        saved.registered == current.registered
            && saved.definition == current.definition
            && saved.settings == current.settings,
        "Startup definition, enabled state, mask or account changed; preserved"
    );
    if !saved.managed {
        ensure!(
            saved.original_state == current.original_state,
            "Previously inactive registration changed; preserved"
        );
    }
    Ok(())
}
pub fn capture(home: &Path, id: &str) -> Result<Value> {
    Ok(serde_json::to_value(inspect(home, id)?)?)
}
pub fn validate(snapshot: &Value) -> Result<()> {
    let saved = decode(snapshot)?;
    compare(&saved, &inspect(&saved.home, &saved.id)?)
}
/// Final restoration requires the original manager availability as well as its
/// untouched definition/settings. A restarted managed process may have a new PID.
pub fn verify_restored(snapshot: &Value) -> Result<()> {
    let saved = decode(snapshot)?;
    let current = inspect(&saved.home, &saved.id)?;
    compare(&saved, &current)?;
    if saved.managed {
        ensure!(
            current.original_state["running"] == true
                && current.original_state["loaded"] == saved.original_state["loaded"],
            "Originally active startup registration was not restored"
        );
    }
    Ok(())
}
fn operate(snapshot: &Value, start: bool) -> Result<()> {
    let saved = decode(snapshot)?;
    let before = inspect(&saved.home, &saved.id)?;
    compare(&saved, &before)?;
    if !saved.managed {
        return Ok(());
    }
    if before.original_state["running"] == start
        && (start || before.original_state["loaded"] == false || saved.platform != "macos")
    {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    checked(&mut linux_command(&[
        if start { "start" } else { "stop" },
        &format!("{}.service", saved.label),
    ])?)?;
    #[cfg(target_os = "macos")]
    {
        let domain = saved.settings["domain"]
            .as_str()
            .context("Missing GUI domain")?;
        if start {
            let path = saved
                .definition
                .as_ref()
                .and_then(|d| d.path.as_ref())
                .context("Missing LaunchAgent path")?;
            if before.original_state["loaded"] == true {
                checked(&mut mac_command(&[
                    "kickstart",
                    &format!("{domain}/{}", saved.label),
                ]))?;
            } else {
                checked(&mut mac_command(&[
                    "bootstrap",
                    domain,
                    path.to_str().context("Invalid LaunchAgent path")?,
                ]))?;
            }
        } else {
            checked(&mut mac_command(&[
                "bootout",
                &format!("{domain}/{}", saved.label),
            ]))?;
        }
    }
    #[cfg(windows)]
    {
        task(&saved.label, if start { "start" } else { "stop" })?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match inspect(&saved.home, &saved.id) {
            Ok(current) => {
                compare(&saved, &current)?;
                if current.original_state["running"] == start
                    && (start
                        || current.original_state["loaded"] == false
                        || saved.platform != "macos")
                {
                    return Ok(());
                }
            }
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => {} // Only bounded manager transition settling is retried.
        }
        ensure!(
            Instant::now() < deadline,
            "Owned startup operation did not settle"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
pub fn stop(snapshot: &Value) -> Result<()> {
    operate(snapshot, false)
}
pub fn start(snapshot: &Value) -> Result<()> {
    operate(snapshot, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixed() -> Snapshot {
        Snapshot {
            schema: 1,
            home: PathBuf::from("/unused"),
            id: "unused".into(),
            label: "unused".into(),
            platform: "test".into(),
            account: "same".into(),
            executable: PathBuf::from("/unused/app"),
            registered: true,
            managed: true,
            definition: Some(Definition {
                path: None,
                kind: "fixture".into(),
                text: "definition".into(),
                sha256: hash(b"definition"),
            }),
            settings: json!({"enabled":true,"masked":false}),
            original_state: json!({"running":true,"loaded":true,"pid":123}),
            manager_pid: Some(123),
        }
    }
    #[test]
    fn managed_stop_changes_only_volatile_running_state() {
        let saved = fixed();
        let mut current = saved.clone();
        current.original_state = json!({"running":false,"loaded":false,"pid":0});
        current.manager_pid = None;
        compare(&saved, &current).unwrap();
        current.settings["enabled"] = json!(false);
        assert!(compare(&saved, &current).is_err());
    }
    #[test]
    fn changed_definition_and_newly_active_registration_are_preserved() {
        let mut saved = fixed();
        let mut current = saved.clone();
        current
            .definition
            .as_mut()
            .unwrap()
            .text
            .push_str(" altered");
        assert!(compare(&saved, &current).is_err());
        saved.managed = false;
        saved.original_state = json!({"running":false,"loaded":true,"pid":0});
        assert!(compare(&saved, &fixed()).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_properties_reject_duplicates_and_malformed_rows() {
        assert!(properties("MainPID=10\nMainPID=11\n").is_err());
        assert!(properties("not-a-property\n").is_err());
        assert_eq!(
            properties("MainPID=0\nActiveState=inactive\n").unwrap()["MainPID"],
            "0"
        );
        absent_unit("LoadState=not-found\nActiveState=inactive\nMainPID=0\nFragmentPath=\n")
            .unwrap();
        assert!(
            absent_unit(
                "LoadState=loaded\nActiveState=active\nMainPID=42\nFragmentPath=/custom/unit\n"
            )
            .is_err()
        );
        assert!(
            absent_unit("LoadState=not-found\nActiveState=failed\nMainPID=0\nFragmentPath=\n")
                .is_err()
        );
        assert!(absent_unit("").is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn launch_state_is_exact_and_disabled_override_is_not_inferred() {
        assert_eq!(
            disabled_override("\t\"fixed-label\" => true\n", "fixed-label").unwrap(),
            Some(true)
        );
        assert_eq!(
            disabled_override("\t\"another\" => true\n", "fixed-label").unwrap(),
            None
        );
        assert!(launch_field("pid = 3\npid = 4\n", "pid").is_err());
    }
}
