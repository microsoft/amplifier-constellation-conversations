#!/usr/bin/env python3
"""Exercise one disposable current-user startup registration on a native CI host.

The test controller may use Python; the installed package uses only its private
runtime and OS tools. This is not a fresh consumer OS or a sign-out/sign-in test.
No account, model session, voice call, or domain action is created.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shlex
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
OPT_IN = "CONSTELLATION_NATIVE_STARTUP_TEST"
RETAIN = ".retain-after-uncertain-cleanup"
TASK_QUERY_STAGE_PREFIX = "CONSTELLATION_STARTUP_QUERY_STAGE:"
TASK_QUERY_STAGES = {"started", "encodingConfigured", "utilityImporting", "utilityImported", "COMcreating", "COMcreated", "connected", "folder", "task", "serialized"}


def task_query_timeout_stage(stderr) -> str:
    # TimeoutExpired can carry bytes even with text capture. Retain only the
    # last exact fixed marker, never PowerShell errors, argv, labels or paths.
    if isinstance(stderr, bytes):
        text = stderr[-8192:].decode("ascii", errors="replace")
    elif isinstance(stderr, str):
        text = stderr[-8192:]
    else:
        return "unobserved"
    last = "unobserved"
    for line in text.splitlines():
        if line.startswith(TASK_QUERY_STAGE_PREFIX):
            stage = line[len(TASK_QUERY_STAGE_PREFIX):]
            if stage in TASK_QUERY_STAGES:
                last = stage
    return last


def load_clean_install():
    spec = importlib.util.spec_from_file_location("native_clean_install", ROOT / "tests/native_clean_install.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


clean = load_clean_install()


def load_update_lifecycle():
    spec = importlib.util.spec_from_file_location("native_update_lifecycle", ROOT / "tests/native_update_lifecycle.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require(value: bool, message: str) -> None:
    if not value:
        raise AssertionError(message)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@contextmanager
def temporary_installation():
    root = Path(tempfile.mkdtemp(prefix="Companion startup café 子 "))
    try:
        yield root
    finally:
        # Never delete executable/configuration files from beneath a service
        # whose stop/removal could not be established. The receipt retains this
        # owned temporary path for CI diagnosis; no other installation is used.
        if not (root / RETAIN).exists():
            shutil.rmtree(root)


def command(argv: list[str | Path], *, env: dict[str, str], check: bool = True, timeout: int = 30) -> subprocess.CompletedProcess:
    result = subprocess.run([str(x) for x in argv], env=env, capture_output=True,
                            encoding="utf-8", errors="replace", timeout=timeout)
    if check:
        require(result.returncode == 0,
                f"{Path(str(argv[0])).name} failed ({result.returncode}): {result.stderr[-2000:]}")
    return result


def startup_environment(root: Path) -> dict[str, str]:
    # Unlike clean-install qualification, startup registration belongs to the
    # actual disposable CI account. Do not redirect HOME to another account or
    # modify that account's global service-manager environment.
    keep = {"HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "SYSTEMROOT", "WINDIR",
            "COMSPEC", "SYSTEMDRIVE", "PATHEXT", "USER", "USERNAME", "USERDOMAIN",
            "LANG", "LC_ALL", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "TMPDIR", "TEMP", "TMP"}
    env = {key: value for key, value in os.environ.items() if key.upper() in keep}
    tools = root / "OS tools only"
    tools.mkdir()
    if os.name != "nt":
        systemctl = shutil.which("systemctl") if platform.system() == "Linux" else None
        if systemctl:
            (tools / "systemctl").symlink_to(Path(systemctl).resolve())
    # System32 can expose wsl.exe on Windows 11. Keep the application PATH
    # isolated on Windows too; this controller names required OS tools exactly.
    env["PATH"] = str(tools)
    for tool in clean.TOOLS:
        require(shutil.which(tool, path=env["PATH"]) is None,
                f"Developer tool is present in startup CLI PATH: {tool}")
    return env


def windows_task_matches(document: str, executable: Path, home: Path) -> bool:
    # Independent inspection of the actual registered task. The installation's
    # native idempotent registration also checks account SID and fixed argv.
    ns = {"t": "http://schemas.microsoft.com/windows/2004/02/mit/task"}
    root = ET.fromstring(document)
    actions = root.findall("t:Actions/t:Exec", ns)
    if len(actions) != 1 or root.findall("t:Actions/t:ComHandler", ns):
        return False
    executable_text = actions[0].findtext("t:Command", namespaces=ns)
    arguments = actions[0].findtext("t:Arguments", namespaces=ns)
    try:
        same_executable = bool(executable_text) and Path(executable_text).samefile(executable)
    except OSError:
        return False
    # The public command accepts the canonical installation path. Unicode is
    # compared without console-codepage conversion or prefix stripping.
    return same_executable and arguments == f'--home "{str(home).rstrip(chr(92))}" serve'


class Startup:
    def __init__(self, home: Path, label: str, executable: Path, env: dict[str, str]):
        self.home, self.label, self.executable, self.env = home, label, executable, env
        self.system = platform.system()
        self.user_home = Path(env.get("USERPROFILE" if os.name == "nt" else "HOME", ""))
        require(self.user_home.is_absolute(), "Current-user home is unavailable")
        self.path = (self.user_home / ".config/systemd/user" / f"{label}.service" if self.system == "Linux"
                     else self.user_home / "Library/LaunchAgents" / f"{label}.plist")
        self.domain = f"gui/{os.getuid()}" if self.system == "Darwin" else None
        self.definition_hash = None

    def task(self) -> dict | None:
        # Fixed OS COM query, not a product host-command surface. Output is
        # explicitly UTF-8; schtasks' console XML may use a lossy codepage.
        system32 = Path(self.env["SYSTEMROOT"]) / "System32"
        powershell = system32 / "WindowsPowerShell/v1.0/powershell.exe"
        script = r"""[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:started');
$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false);
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:encodingConfigured');
$PSModuleAutoLoadingPreference='None';
$env:PSModulePath=[System.IO.Path]::Combine($PSHOME,'Modules');
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:utilityImporting');
$utilityManifest=[System.IO.Path]::Combine($PSHOME,'Modules\Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1');
Import-Module -Name $utilityManifest -ErrorAction Stop;
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:utilityImported');
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:COMcreating');
$s=New-Object -ComObject 'Schedule.Service';
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:COMcreated');
$s.Connect(); [Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:connected');
try {$folder=$s.GetFolder('\'); [Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:folder'); $t=$folder.GetTask($env:CONSTELLATION_TEST_TASK_LABEL)}
catch {if ($_.Exception.HResult -eq -2147024894 -or $_.Exception.InnerException.HResult -eq -2147024894) {[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:task'); 'null'; [Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:serialized'); exit 0}; throw}
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:task');
$json=@{xml=$t.Xml; state=[int]$t.State; enabled=[bool]$t.Enabled; instances=[int]($t.GetInstances(0).Count)} | ConvertTo-Json -Compress;
[Console]::Error.WriteLine('CONSTELLATION_STARTUP_QUERY_STAGE:serialized'); $json"""
        env = dict(self.env, CONSTELLATION_TEST_TASK_LABEL=self.label)
        # Name-based module autoload can search user and machine directories,
        # even with an isolated PATH. Load only the in-box Utility manifest above
        # for New-Object/ConvertTo-Json; this does not change the application.
        # Only this independent controller query gets the fixed OS search path.
        # Application CLI calls retain their empty PATH and WSL exclusion; no
        # account/global manager environment or registration privilege changes.
        env["PATH"] = ";".join([str(system32), str(powershell.parent)])
        try:
            result = command([powershell, "-NoProfile", "-NonInteractive", "-Command", script], env=env, timeout=30)
        except subprocess.TimeoutExpired as error:
            stage = task_query_timeout_stage(error.stderr)
            raise TimeoutError("Windows startup controller task query exceeded 30 seconds; "
                               f"last_observed_stage={stage}") from None
        return json.loads(result.stdout.lstrip("\ufeff"))

    def exists(self) -> bool:
        return self.task() is not None if self.system == "Windows" else self.path.exists()

    def preflight(self) -> None:
        if self.system == "Linux":
            command(["systemctl", "--user", "show-environment"], env=self.env)
        elif self.system == "Darwin":
            command(["/bin/launchctl", "print", self.domain], env=self.env)
        elif self.system == "Windows":
            self.task()
        else:
            raise RuntimeError("Unsupported OS manager")
        require(not self.exists(), "Unique test startup identity already exists")

    def inspect(self) -> dict:
        if self.system == "Windows":
            task = self.task()
            require(task is not None and windows_task_matches(task["xml"], self.executable, self.home),
                    "Actual Windows task does not match the owned executable and Unicode home")
            require(task["enabled"], "Registered Windows task is disabled")
            current_hash = hashlib.sha256(task["xml"].encode("utf-8")).hexdigest()
            require(self.definition_hash in {None, current_hash}, "Owned Windows task definition changed")
            self.definition_hash = current_hash
            return {"kind": "scheduled-task", "label": self.label, "enabled": True,
                    "definition_sha256": current_hash}
        require(self.path.is_file() and not self.path.is_symlink(), "Owned startup definition is absent or replaced")
        current_hash = digest(self.path)
        require(self.definition_hash in {None, current_hash},
                "Owned startup definition changed; preserving it for inspection")
        if self.system == "Darwin":
            value = plistlib.loads(self.path.read_bytes())
            require(value.get("Label") == self.label and value.get("ProgramArguments") ==
                    [str(self.executable), "--home", str(self.home), "serve"],
                    "Actual LaunchAgent does not match the owned executable and home")
        else:
            execute = [line for line in self.path.read_text(encoding="utf-8").splitlines()
                       if line.startswith("Exec")]
            require(len(execute) == 1 and execute[0].startswith("ExecStart=") and
                    shlex.split(execute[0].removeprefix("ExecStart=")) ==
                    [str(self.executable), "--home", str(self.home), "serve"],
                    "Actual user service does not match the owned executable and home")
            command(["systemctl", "--user", "is-enabled", f"{self.label}.service"], env=self.env)
        self.definition_hash = current_hash
        return {"kind": "launchd" if self.system == "Darwin" else "systemd-user", "label": self.label,
                "definition_sha256": current_hash}

    def state(self) -> dict:
        definition = self.inspect()
        if self.system == "Windows":
            task = self.task()
            require(task is not None, "Owned task disappeared")
            return {"definition": definition, "enabled": task["enabled"], "masked": False,
                    "active": task["state"] == 4 and task["instances"] == 1, "manager_state": task["state"],
                    "instances": task["instances"], "pid": None}
        if self.system == "Linux":
            result = command(["systemctl", "--user", "show", f"{self.label}.service",
                "--property=ActiveState", "--property=SubState", "--property=MainPID",
                "--property=UnitFileState", "--property=LoadState", "--property=FragmentPath",
                "--property=DropInPaths"], env=self.env)
            values = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
            require(all(key in values for key in ["ActiveState", "SubState", "MainPID", "UnitFileState", "LoadState", "FragmentPath", "DropInPaths"]),
                    "Systemd did not report complete owned service state")
            return {"definition": definition, "enabled": values["UnitFileState"],
                    "masked": values["LoadState"] == "masked" or values["UnitFileState"].startswith("masked"),
                    "active": values["ActiveState"] == "active" and values["SubState"] == "running",
                    "manager_state": [values["LoadState"], values["ActiveState"], values["SubState"]],
                    "fragment_path": values["FragmentPath"], "drop_in_paths": values["DropInPaths"],
                    "pid": int(values["MainPID"])}
        disabled = command(["/bin/launchctl", "print-disabled", self.domain], env=self.env).stdout
        override = re.search(r'"' + re.escape(self.label) + r'"\s*=>\s*(true|false)', disabled)
        result = command(["/bin/launchctl", "print", f"{self.domain}/{self.label}"], env=self.env, check=False)
        state = re.search(r"^\s*state\s*=\s*(\S+)", result.stdout, re.MULTILINE)
        pid = re.search(r"^\s*pid\s*=\s*(\d+)", result.stdout, re.MULTILINE)
        return {"definition": definition, "enabled": override is None or override.group(1) == "false",
                "disabled_override": override.group(1) if override else "absent",
                "masked": False, "active": result.returncode == 0 and state is not None and state.group(1) == "running",
                "manager_state": state.group(1) if state else "unloaded", "pid": int(pid.group(1)) if pid else 0}

    def await_active(self, application_pid: int) -> dict:
        deadline = time.monotonic() + 20
        last = None
        while time.monotonic() < deadline:
            last = self.state()
            if last["active"]:
                require(last["pid"] in {None, application_pid}, "Manager's process is not the healthy application")
                return last
            time.sleep(0.1)
        raise AssertionError(f"Application is healthy but owned OS manager is not active: {last}")

    def start(self) -> None:
        self.inspect()
        if self.system == "Linux":
            command(["systemctl", "--user", "start", f"{self.label}.service"], env=self.env)
        elif self.system == "Darwin":
            command(["/bin/launchctl", "bootstrap", self.domain, self.path], env=self.env)
        else:
            command([Path(self.env["SYSTEMROOT"]) / "System32/schtasks.exe", "/Run", "/TN", self.label], env=self.env)

    def stop(self) -> None:
        # Inspect the complete fixed command before affecting a registration.
        self.inspect()
        if self.system == "Linux":
            command(["systemctl", "--user", "stop", f"{self.label}.service"], env=self.env)
            state = command(["systemctl", "--user", "show", f"{self.label}.service",
                             "--property=ActiveState", "--property=MainPID"], env=self.env).stdout
            require("MainPID=0" in state and "ActiveState=inactive" in state, "Owned service did not stop")
        elif self.system == "Darwin":
            # A failed bootstrap can leave an unloaded but correctly owned plist.
            target = f"{self.domain}/{self.label}"
            loaded = command(["/bin/launchctl", "print", target], env=self.env, check=False)
            if loaded.returncode == 0:
                command(["/bin/launchctl", "bootout", target], env=self.env)
            require(command(["/bin/launchctl", "print", target], env=self.env, check=False).returncode != 0,
                    "Owned LaunchAgent remains loaded")
        else:
            task = self.task()
            if task is not None and task["state"] in {2, 4}:
                command([Path(self.env["SYSTEMROOT"]) / "System32/schtasks.exe", "/End", "/TN", self.label], env=self.env)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                task = self.task()
                if task is None or task["state"] not in {2, 4}:
                    return
                time.sleep(0.1)
            raise RuntimeError("Owned Windows task remains queued or running")


def await_health(home: Path, timeout: int = 30) -> tuple[str, object]:
    context = clean.strict_tls_context(home / "tls/root-ca.crt")
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            runtime = json.loads((home / "runtime.json").read_text(encoding="utf-8"))
            with urllib.request.urlopen(runtime["url"] + "/health", context=context, timeout=1) as response:
                require(response.status == 200 and json.load(response)["ok"], "Invalid native health result")
            return runtime["url"], context
        except (OSError, ValueError) as error:
            if clean.certificate_failure(error):
                raise AssertionError(f"Strict HTTPS verification failed: {error}") from error
            last = str(error)
            time.sleep(0.1)
    raise RuntimeError(f"Registered native service never became healthy: {last}")


def await_closed(url: str, context: object) -> None:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            urllib.request.urlopen(url + "/health", context=context, timeout=1).close()
        except urllib.error.HTTPError:
            pass  # An HTTP error still proves a listener is alive.
        except (OSError, ValueError):
            return
        time.sleep(0.1)
    raise RuntimeError("Disposable application listener remains reachable after stop")


def stable_manager_state(value: dict) -> dict:
    return {key: item for key, item in value.items() if key != "pid"}


def update_registered(startup: Startup, binary: Path, home: Path, bundle: Path, env: dict, root: Path, result: dict) -> tuple:
    lifecycle = load_update_lifecycle()
    candidate = root / "Disposable whole bundle B"
    lifecycle.derive_bundle(bundle, candidate, "registered-b")

    def app_status():
        return json.loads(command([binary, "--home", home, "api", "GET", "/api/v1/native-update/status"], env=env).stdout)

    initial_app = app_status()
    initial_manager = startup.await_active(initial_app["pid"])
    applied = json.loads(command([binary, "--home", home, "native-update", "run", "--package", candidate], env=env, timeout=180).stdout)
    require(applied.get("restored") is True, "Registered update did not restore")
    live = await_health(home)
    updated_app = app_status()
    updated_manager = startup.await_active(updated_app["pid"])
    require(stable_manager_state(updated_manager) == stable_manager_state(initial_manager),
            "Update changed owned manager definition/enabled/masked/running state")
    require(updated_app["process_nonce"] != initial_app["process_nonce"], "Update did not replace owned service process")
    selected = json.loads(command([binary, "--home", home, "native-update", "status"], env=env).stdout)
    require(selected["selected_manifest"] == digest(candidate / "manifest.json"), "Registered service did not select entire B bundle")
    rolled_back = json.loads(command([binary, "--home", home, "native-update", "rollback", applied["transaction_id"]], env=env, timeout=180).stdout)
    require(rolled_back.get("restored") is True and rolled_back["transaction_id"] != applied["transaction_id"],
            "Registered rollback did not settle a fresh transaction")
    live = await_health(home)
    restored_app = app_status()
    restored_manager = startup.await_active(restored_app["pid"])
    require(stable_manager_state(restored_manager) == stable_manager_state(initial_manager),
            "Rollback changed owned manager definition/enabled/masked/running state")
    selected = json.loads(command([binary, "--home", home, "native-update", "status"], env=env).stdout)
    require(selected["selected_manifest"] == digest(bundle / "manifest.json"), "Registered rollback did not restore A bundle")
    result["managed_update"] = {"apply_transaction": applied["transaction_id"],
        "rollback_transaction": rolled_back["transaction_id"], "before": initial_manager,
        "updated": updated_manager, "rolled_back": restored_manager,
        "limitation": "B changes manifest version/support document; service executable and private Node/SDK remain the same tested bytes"}
    result["checks"].append("whole-bundle update and fresh rollback preserve actual manager registration, enabled/mask state and managed running process")
    return live


def cleanup_owned(startup: Startup, binary: Path, home: Path, env: dict[str, str],
                  live: tuple | None, root: Path, result: dict) -> None:
    try:
        if startup.exists():
            startup.stop()
            if live:
                await_closed(*live)
            command([binary, "--home", home, "startup", "remove"], env=env)
            require(not startup.exists(), "Owned startup registration remains after removal")
        result["cleanup"] = "exact owned registration stopped and removed"
    except Exception as error:
        (root / RETAIN).write_text("Cleanup could not be established; preserve this owned test installation.\n",
                                  encoding="utf-8")
        result["cleanup_error"] = str(error)
        result["retained_temporary_installation"] = str(root)
        raise


def qualify(archive: Path, receipt: Path, output: Path) -> dict:
    result = {"schema_version": 1, "status": "not_tested", "target": clean.native_target(),
              "host": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
              "scope": "unique current-user registration/start/stop/removal on a disposable native CI account",
              "fresh_consumer_os": "not_tested", "sign_out_sign_in": "not_tested",
              "os_password_sign_in": "not_tested", "physical_media": "not_tested", "signing": "not_tested",
              "managed_service_environment": "OS manager environment; separate clean-PATH package qualification applies",
              "model_requests": 0, "speech_release": "unchanged_paused", "checks": []}
    output.parent.mkdir(parents=True, exist_ok=True)
    try:
        require(os.environ.get(OPT_IN) == "1", "Explicit disposable-current-user startup opt-in required")
        provenance = json.loads(receipt.read_text(encoding="utf-8"))
        manifest, contents = clean.package_module().inspect_archive(archive, provenance["archive_sha256"])
        require(manifest["target"] == clean.native_target(), "Package does not match native host")
        require(hashlib.sha256(contents["manifest.json"]).hexdigest() == provenance["manifest_sha256"],
                "Package receipt manifest digest mismatch")
        result.update(source_revision=manifest["source_revision"], archive_sha256=provenance["archive_sha256"],
                      manifest_sha256=hashlib.sha256(contents["manifest.json"]).hexdigest())
        with temporary_installation() as temp:
            root = temp.resolve()
            bundle = root / "Bundle"
            for name, data in contents.items():
                target = bundle.joinpath(*name.split("/")); target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            for row in manifest["files"]:
                if row["executable"]:
                    (bundle / row["path"]).chmod(0o755)
            home = root / "Native installation"
            env = startup_environment(root)
            binary = bundle / manifest["entrypoints"]["service"]
            installed = json.loads(command([binary, "--home", home, "bundle", "install", "--path", bundle], env=env).stdout)
            binary = Path(installed["entrypoints"]["service"])
            node = Path(installed["entrypoints"]["node"])
            command([binary, "--home", home, "init", "--agent-command", node,
                     "--agent-arg=--version", "--agent-cwd", root], env=env)
            config_path = home / "config.json"
            original_config = config_path.read_bytes()
            config = json.loads(original_config)
            home = Path(config["home"]) if "home" in config else home.resolve()
            command([binary, "--home", home, "native-update", "bootstrap", "--package", installed["bundle"]], env=env)
            label = f'agent-conversations-{config["id"]}'
            copied = home / "bin" / binary.name
            startup = Startup(home, label, copied, env)
            try:
                startup.preflight()
            except Exception as error:
                result["reason"] = f"Native startup manager unavailable or identity not fresh: {error}"
                raise
            result["status"] = "running"
            owned = False
            live = None
            try:
                # Preflight proved this unique registration absent. Once the
                # operation starts, cleanup also covers lost acknowledgement.
                owned = True
                command([binary, "--home", home, "startup", "install"], env=env)
                first = startup.inspect()
                require(digest(copied) == digest(binary), "Startup copy differs from packaged executable")
                command([binary, "--home", home, "startup", "install"], env=env)
                require(startup.inspect() == first, "Idempotent registration changed the owned definition")
                result["registration"] = first
                result["checks"].append("unique native registration and Unicode identity, repeated install preserves definition")
                startup.start()
                live = await_health(home)
                command([copied, "--home", home, "api", "GET", "/api/v1/conversations"], env=env)
                result["checks"].append("OS manager starts packaged application; strict HTTPS and authenticated API pass")
                live = update_registered(startup, binary, home, Path(installed["bundle"]), env, root, result)
                startup.stop()
                await_closed(*live)
                result["checks"].append("exact owned OS registration stopped and application listener closed")
                require(config_path.read_bytes() == original_config, "Startup changed application configuration")
                require(not (home / "companion.json").exists(), "Startup enabled voice configuration")
            finally:
                if owned:
                    cleanup_owned(startup, binary, home, env, live, root, result)
            result["checks"].append("configuration preserved, no voice enabled, owned registration removed")
        result["temporary_installation_removed"] = True
        result["status"] = "passed"
        return result
    except Exception as error:
        if result["status"] == "running":
            result["status"] = "failed"
        result["error"] = str(error)
        raise
    finally:
        output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(qualify(args.archive.resolve(), args.receipt.resolve(), args.output.resolve())))
