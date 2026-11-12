#!/usr/bin/env python3
"""Qualify a real native package with developer tools absent from the child PATH.

This orchestrator runs on a build/CI machine and is not an application dependency.
It does not claim a freshly provisioned consumer OS, OS sign-in, browser media,
Gatekeeper/notarization or Authenticode. No model/provider call is made.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ("cargo", "rustc", "python", "python3", "pip", "pip3", "node", "npm", "npx", "cc", "gcc", "clang", "cl", "wsl")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def native_target() -> str:
    system = {"Windows": "windows", "Darwin": "macos", "Linux": "linux"}[platform.system()]
    # Windows's PROCESSOR_ARCHITECTURE must reflect the running native process;
    # PROCESSOR_ARCHITEW6432 reveals an x64/ARM emulation mismatch separately.
    machine = platform.machine().lower()
    arch = {"amd64": "x64", "x86_64": "x64", "arm64": "arm64", "aarch64": "arm64"}[machine]
    return f"{system}-{arch}"


def isolated_environment(user_home: Path, empty_path: Path) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items()
           if key.upper() in {"SYSTEMROOT", "WINDIR", "COMSPEC", "SYSTEMDRIVE", "PATHEXT", "USER", "USERNAME", "USERDOMAIN", "LANG"}}
    temp = user_home / "temporary"
    temp.mkdir()
    env.update({"HOME": str(user_home), "USERPROFILE": str(user_home),
                "LOCALAPPDATA": str(user_home / "Local"), "APPDATA": str(user_home / "Roaming"),
                "XDG_CONFIG_HOME": str(user_home / "config"), "XDG_DATA_HOME": str(user_home / "data"),
                "TEMP": str(temp), "TMP": str(temp), "TMPDIR": str(temp), "PATH": str(empty_path)})
    for name in TOOLS:
        require(shutil.which(name, path=env["PATH"]) is None, f"Developer tool remains discoverable: {name}")
    return env


def package_module():
    spec = importlib.util.spec_from_file_location("native_package", ROOT / "scripts/package-native.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(command: list, env: dict, cwd: Path, *, timeout: int = 60) -> subprocess.CompletedProcess:
    result = subprocess.run([str(v) for v in command], env=env, cwd=cwd,
                            capture_output=True, text=True, encoding="utf-8", timeout=timeout)
    require(result.returncode == 0,
            f"Native operation failed ({Path(str(command[0])).name}): {result.stderr[-3000:]}")
    return result


def _qualify(archive: Path, receipt: Path, output: Path) -> dict:
    package = package_module()
    provenance = json.loads(receipt.read_text())
    manifest, contents = package.inspect_archive(archive, provenance["archive_sha256"])
    require(manifest["target"] == native_target(), "Package target is not the native test host architecture")
    require(hashlib.sha256(contents["manifest.json"]).hexdigest() == provenance["manifest_sha256"],
            "Package receipt manifest digest mismatch")
    checks = []
    result = {"schema_version": 1, "target": manifest["target"], "source_revision": manifest["source_revision"],
              "archive_sha256": provenance["archive_sha256"], "manifest_sha256": provenance["manifest_sha256"],
              "host": {"system": platform.system(), "release": platform.release(), "version": platform.version(),
                       "machine": platform.machine()}, "checks": checks,
              "scope": "native CI host with isolated application HOME and developer-tools-absent child PATH",
              "fresh_consumer_os": "not_tested", "os_password_sign_in": "not_tested",
              "physical_browser_microphone_camera": "not_tested", "signing_notarization": "not_tested",
              "model_requests": 0, "speech_release": "unchanged_paused"}
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="Native install café ") as temporary:
        root = Path(temporary)
        unpacked = root / "Original extraction"
        unpacked.mkdir()
        for name, data in contents.items():
            path = unpacked.joinpath(*name.split("/"))
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        for row in manifest["files"]:
            if row["executable"]:
                (unpacked / row["path"]).chmod(0o755)
        # Move after extraction; this catches absolute build/extraction paths.
        bundle = root / "Relocated Companion 日本語"
        unpacked.rename(bundle)
        home = root / "Fresh installation"
        user_home = root / "New user"
        user_home.mkdir()
        empty_path = root / "No development tools"
        empty_path.mkdir()
        env = isolated_environment(user_home, empty_path)
        checks.append("all developer tools absent from child PATH; no inherited provider keys or NODE_PATH")
        service = bundle / manifest["entrypoints"]["service"]
        node = bundle / manifest["entrypoints"]["node"]
        version = json.loads(run([node, "-p", "JSON.stringify({version:process.versions.node,arch:process.arch,platform:process.platform})"], env, root).stdout)
        require(version["version"] == manifest["node_version"], "Bundled Node version mismatch")
        require(version["arch"] == manifest["platform"]["arch"], "Bundled Node used a different architecture")
        checks.append("actual bundled Node launches natively at exact frozen version")
        run([service, "--version"], env, root)
        run([service, "--home", home, "bundle", "verify", "--path", bundle], env, root)
        installed = json.loads(run([service, "--home", home, "bundle", "install", "--path", bundle], env, root).stdout)
        # The installer returns exact absolute entrypoints. It may retain
        # additional fields; the native package is not selected by PATH.
        entrypoints = installed["entrypoints"]
        installed_service = Path(entrypoints["service"])
        installed_node = Path(entrypoints["node"])
        require(installed_service.is_absolute() and installed_node.is_absolute(), "Installer must return absolute entrypoints")
        require(installed_service.resolve().is_relative_to(home.resolve())
                and installed_node.resolve().is_relative_to(home.resolve()), "Installation escaped its own home")
        run([installed_service, "--home", home, "bundle", "verify", "--path", installed_service.parent], env, root)
        checks.append("native bundle verify/install and relocated installed executable pass")
        fixture = root / "Read only ACP fixture"
        fixture.mkdir()
        shutil.copy2(ROOT / "tests/native/fixture.mjs", fixture / "fixture.mjs")
        shutil.copytree(ROOT / "tests/native/node_modules", fixture / "node_modules")
        workspace = root / "Workspace"
        workspace.mkdir()
        owner = os.environ.get("USERNAME") or os.environ.get("USER") or "fixture-owner"
        run([installed_service, "--home", home, "init", "--owner", owner,
             "--agent-command", installed_node, "--agent-arg", fixture / "fixture.mjs",
             "--agent-cwd", workspace], env, root)
        config_path = home / "config.json"
        config = json.loads(config_path.read_text())
        log = root / "ACP initialized.jsonl"
        config["agent"]["env"]["NATIVE_FIXTURE_LOG"] = str(log)
        # This is disposable owner configuration, never a production installation.
        config_path.write_text(json.dumps(config), encoding="utf-8")
        probe = json.loads(run([installed_service, "--home", home, "probe"], env, root).stdout)
        require(probe["ok"] and probe["agent_info"]["name"] == "native-package-fixture", "Official SDK initialize did not complete")
        events = [json.loads(line) for line in log.read_text().splitlines()]
        require([event["method"] for event in events] == ["initialize"], "Installation probe created a session or prompt")
        require(Path(events[0]["node"]).resolve() == installed_node.resolve(), "ACP fixture used an ambient Node installation")
        checks.append("official ACP SDK initialize through installed private Node; no session or model prompt")
        bridge = Path(entrypoints["bridge"])
        import_script = """const {createRequire}=require('node:module'); const {pathToFileURL}=require('node:url');
          const r=createRequire(process.argv[1]); (async()=>{for(const n of ['openai','ws','zod','@modelcontextprotocol/sdk/client/index.js','@slack/web-api'])await import(pathToFileURL(r.resolve(n))); console.log('bundled SDK imports passed')})().catch(e=>{console.error(e);process.exitCode=1});"""
        run([installed_node, "-e", import_script, bridge], env, root)
        run([installed_node, "--check", bridge], env, root)
        checks.append("private bridge SDK dependencies import without npm or ambient node_modules")
        context = ssl.create_default_context(cafile=str(home / "tls/root-ca.crt"))
        with (root / "server.log").open("w", encoding="utf-8") as stream:
            server = subprocess.Popen([str(installed_service), "--home", str(home), "serve"],
                                      env=env, cwd=root, stdout=stream, stderr=stream)
            try:
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    require(server.poll() is None, "Disposable HTTPS service exited before readiness: "
                            + (root / "server.log").read_text(encoding="utf-8")[-3000:])
                    try:
                        runtime = json.loads((home / "runtime.json").read_text())
                        with urllib.request.urlopen(runtime["url"] + "/health", context=context, timeout=1) as response:
                            require(response.status == 200 and json.load(response)["ok"], "Invalid health response")
                        break
                    except (OSError, ValueError):
                        time.sleep(0.1)
                else:
                    raise AssertionError("Native HTTPS service did not become healthy")
                run([installed_service, "--home", home, "api", "GET", "/api/v1/conversations"], env, root)
                try:
                    urllib.request.urlopen(runtime["url"] + "/api/v1/conversations", context=context, timeout=5)
                except urllib.error.HTTPError as error:
                    require(error.code in {401, 403}, "Unexpected unauthenticated API status")
                else:
                    raise AssertionError("Protected API allowed an unauthenticated request")
                checks.append("native HTTPS health and authenticated API; certificate verified with installation CA; anonymous API denied")
            finally:
                server.terminate()
                try:
                    server.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
        require(server.poll() is not None, "Disposable service was not stopped")
        checks.append("disposable service stopped; no surviving application listener")
        result["status"] = "passed"
    result["temporary_installation_removed"] = True
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return result


def qualify(archive: Path, receipt: Path, output: Path) -> dict:
    try:
        return _qualify(archive, receipt, output)
    except Exception as error:
        output.parent.mkdir(parents=True, exist_ok=True)
        # Keep failed native qualification distinct from no run. Inputs contain
        # only disposable credentials and no provider account or live session.
        output.write_text(json.dumps({"schema_version": 1, "status": "failed",
            "error": str(error), "host": {"system": platform.system(), "release": platform.release(),
                "machine": platform.machine()}, "scope": "native CI clean-path install",
            "model_requests": 0, "speech_release": "unchanged_paused"}, indent=2) + "\n", encoding="utf-8")
        raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(qualify(args.archive.resolve(), args.receipt.resolve(), args.output.resolve())))


if __name__ == "__main__":
    main()
