#!/usr/bin/env python3
"""Qualify a packaged consumer launch using private disposable installations.

Python is a CI controller dependency. App/native helper children have an empty
PATH and no inherited provider credentials. A separate Mac LaunchServices check
uses the OS application's environment and makes no clean-machine claim.
"""
from __future__ import annotations

import argparse
import hashlib
import http.cookiejar
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("consumer_lifecycle", ROOT / "tests/native_update_lifecycle.py")
lifecycle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lifecycle)
require = lifecycle.require
read_json, write_json = lifecycle.read_json, lifecycle.write_json
PASSWORD = "fixture-local-account-password-2026"
FIXTURE_KEY = "fixture-consumer-key-not-a-real-provider-credential"
AGENT_ID = "consumer-fixture"
AGENT_LABEL = "Native consumer fixture"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class ConsumerInstallation(lifecycle.Installation):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.cookies = http.cookiejar.CookieJar()
        self.browser = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(self.cookies), NoRedirect())
        self.health = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        self.csrf = None

    def desktop_start(self, launcher: Path | None = None, mac_app: Path | None = None):
        require(self.service_lock_available(), "Disposable service is already running")
        stream = (self.root / f"owned-service-{len(self.children)}.log").open("w", encoding="utf-8")
        self.streams.append(stream)
        arguments = [str(launcher or self.worker), "--home", str(self.home), "--no-browser"]
        if mac_app is not None:
            require(platform.system() == "Darwin", "LaunchServices requires the actual native Mac host")
            arguments = ["/usr/bin/open", "-W", "-n", "-a", str(mac_app), "--args", "--home", str(self.home), "--no-browser"]
        operation = self.begin_operation({"kind": "desktop_launch", "entrypoint": "mac_application" if mac_app else "native_no_argument"}, 60)
        started, outcome = time.monotonic(), "error"
        try:
            child = subprocess.Popen(arguments, env=self.env, cwd=self.root, stdin=subprocess.DEVNULL,
                                     stdout=stream, stderr=stream)
            self.children.append(child)
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                require(child.poll() is None, "Disposable desktop launcher exited before readiness")
                status = self.running()
                if status:
                    outcome = "healthy"
                    return child, status
                time.sleep(0.1)
            raise AssertionError("Disposable desktop launch did not become ready within 60 seconds")
        finally:
            self.end_operation(operation, started, outcome)

    def running(self):
        try:
            config = read_json(self.home / "config.json")
            runtime = read_json(self.home / "runtime.json")
            require(runtime["url"] == config["public_url"] and runtime["url"].startswith("http://127.0.0.1:"),
                    "Consumer fixture listener is not the saved literal loopback origin")
            with self.health.open(runtime["url"] + "/health", timeout=1) as response:
                require(response.status == 200 and json.load(response)["ok"], "Invalid consumer health response")
        except (OSError, ValueError):
            return None
        self.ports.add(runtime["port"])
        # The native CLI proves the listener before transmitting its credential.
        value = self.api()
        require(value["id"] == config["id"] and value["pid"] == runtime["pid"]
                and value["process_nonce"] == runtime["process_nonce"], "Consumer listener process identity differs")
        return value

    def api(self, method="GET", path="/api/v1/native-update/status", body=None):
        arguments = ["api", method, path]
        if body is not None:
            payload = self.root / "Controller requests" / (str(uuid.uuid4()) + ".json")
            write_json(payload, body)
            arguments += ["--body-file", payload]
        return self.command(arguments, timeout=30)

    def browser_request(self, method, path, body=None, *, expected=200, headers=None):
        origin = read_json(self.home / "config.json")["public_url"]
        request_headers = {"Origin": origin, **(headers or {})}
        if self.csrf:
            request_headers["X-CSRF-Token"] = self.csrf
        data = None
        if body is not None:
            request_headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        request = urllib.request.Request(origin + path, data=data, headers=request_headers, method=method)
        try:
            response = self.browser.open(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            require(response.status == expected, f"Consumer HTTP {method} {path} returned unexpected status")
            value = json.load(response)
        require(FIXTURE_KEY not in json.dumps(value) and PASSWORD not in json.dumps(value), "Consumer response exposed a private input")
        return value

    def snapshot(self):
        files = {str(path.relative_to(self.home)): lifecycle.digest(path.read_bytes())
                 for path in [self.home / "config.json", self.home / "retained-unknown-evidence.json",
                              *sorted((self.home / "secrets").glob("*"))] if path.is_file()}
        tables = ["agent_endpoints", "conversations", "messages", "room_members", "room_agents", "grants",
                  "browser_sessions", "companion_calls", "companion_delegations", "companion_actions",
                  "input_deliveries", "companion_observations"]
        with self.db() as database:
            rows = {name: sorted([list(row) for row in database.execute(f'SELECT * FROM "{name}"')], key=repr) for name in tables}
        companion = read_json(self.home / "companion.json")
        companion.pop("node")
        companion.pop("bridge")
        return {"files": files, "tables": rows, "companion_policy": companion}


def first_account(install: ConsumerInstallation, setup_token: str):
    value = install.browser_request("POST", "/auth/first-account", {
        "setup_token": setup_token, "password": PASSWORD, "start_at_signin": False})
    require(value["account_created"] and value["authenticated"], "Consumer account was not created and signed in")
    install.csrf = value["csrf"]
    require(all(not cookie.secure and cookie.has_nonstandard_attr("HttpOnly") for cookie in install.cookies),
            "Explicit local HTTP cookie does not retain the required browser boundary")


def add_acp_fixture(install: ConsumerInstallation):
    """Simulated fixed Instance launcher; actual official SDK initialize only."""
    fixture = install.root / "Disposable ACP fixture"
    workspace, binary = fixture / "workspace", fixture / "bin" / ("agent-instance.exe" if os.name == "nt" else "agent-instance")
    workspace.mkdir(parents=True)
    binary.parent.mkdir()
    shutil.copy2(install.node, binary)
    write_json(fixture / "config.json", {"id": str(uuid.uuid4()), "fixture": "initialize-only"})
    write_json(workspace / "package.json", {"type": "module"})
    log = fixture / "initialize.jsonl"
    script = (ROOT / "tests/native/fixture.mjs").read_text(encoding="utf-8")
    require(script.count("process.env.NATIVE_FIXTURE_LOG") == 1, "ACP fixture logging contract changed")
    (workspace / "acp").write_text(script.replace("process.env.NATIVE_FIXTURE_LOG", json.dumps(str(log))), encoding="utf-8")
    shutil.copytree(ROOT / "tests/native/node_modules", workspace / "node_modules")
    reply = install.api("POST", "/api/v1/companion/local-agent", {"id": AGENT_ID, "label": AGENT_LABEL, "home": str(fixture)})
    require(reply["saved"] and reply["connection_checked"], "Consumer local ACP fixture was not checked and saved")
    events = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
    require([event["method"] for event in events] == ["initialize"], "Consumer connection check created non-initialize work")
    require(events[0]["provider_key_present"] is False, "Provider credential reached the ACP fixture")
    require(Path(events[0]["node"]).samefile(binary), "ACP fixture used another runtime")
    return log


def retain_browser_output(artifacts, stdout, stderr, secrets, *, returncode, timed_out):
    """Retain a bounded diagnostic after replacing every supplied private input."""
    def sanitized(value):
        if isinstance(value, bytes):
            value = value.decode("utf-8", errors="replace")
        value = value or ""
        # Redact before truncating, so a capture boundary cannot retain part of
        # a known secret. JSON escaping is included for controller diagnostics.
        for secret in sorted({item for item in secrets if item}, key=len, reverse=True):
            for spelling in {secret, json.dumps(secret)[1:-1]}:
                value = value.replace(spelling, "[redacted]")
        encoded = value.encode("utf-8", errors="replace")
        return {"text": encoded[-32768:].decode("utf-8", errors="ignore"),
                "truncated": len(encoded) > 32768}
    write_json(artifacts / "controller-browser-output.json", {
        "schema": 1, "return_code": returncode, "timed_out": timed_out,
        "maximum_bytes_per_stream": 32768,
        "stdout": sanitized(stdout), "stderr": sanitized(stderr)})


def run_browser(install, script, node, artifacts, setup_token):
    artifacts.mkdir(parents=True, exist_ok=True)
    payload = {"url": read_json(install.home / "config.json")["public_url"], "setup_token": setup_token,
               "password": PASSWORD, "home": str(install.home), "artifact_dir": str(artifacts),
               "agent_id": AGENT_ID, "agent_label": AGENT_LABEL, "fixture_key": FIXTURE_KEY}
    child = subprocess.Popen([str(node), str(script)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, text=True, encoding="utf-8", cwd=ROOT)
    try:
        stdout, stderr = child.communicate(json.dumps(payload), timeout=120)
    except subprocess.TimeoutExpired as first_timeout:
        child.kill()
        stdout, stderr = first_timeout.stdout, first_timeout.stderr
        try:
            stdout, stderr = child.communicate(timeout=10)
        except subprocess.TimeoutExpired as drain_timeout:
            if drain_timeout.stdout is not None:
                stdout = drain_timeout.stdout
            if drain_timeout.stderr is not None:
                stderr = drain_timeout.stderr
            install.unsettled_commands.append(child)
        retain_browser_output(artifacts, stdout, stderr, [setup_token, PASSWORD, FIXTURE_KEY],
                              returncode=child.returncode, timed_out=True)
        raise TimeoutError("Consumer browser fixture exceeded its 120-second allowance") from None
    retain_browser_output(artifacts, stdout, stderr, [setup_token, PASSWORD, FIXTURE_KEY],
                          returncode=child.returncode, timed_out=False)
    require(child.returncode == 0, "Consumer browser fixture failed; inspect its private browser artifacts")
    require(not any(secret in stdout + stderr for secret in [setup_token, PASSWORD, FIXTURE_KEY]),
            "Consumer browser fixture output contained private input")
    result = json.loads(stdout)
    require(result.get("status") == "passed", "Consumer browser did not report passing qualification")
    return result


def prepare_consumer_installation(install, *, browser_test=None, browser_node=None, browser_artifacts=None, managed_parent=False):
    """Create the existing private account/contact fixture without updating it.

    Returned private values stay in controller memory; receipts retain only
    independent comparisons and digests of the protected installation state.
    """
    child, original = install.desktop_start()
    config_bytes = (install.home / "config.json").read_bytes()
    config = json.loads(config_bytes)
    require(config["auth_mode"] == "local_password" and config["transport"] == "local_http"
            and config.get("agent") is None, "No-argument launch did not create the local empty account mode")
    require(not (install.home / "tls").exists() and not (install.home / "companion.json").exists(),
            "Fresh launch introduced manual TLS or enabled voice")
    require(install.api("GET", "/api/v1/agents")["items"] == [], "Fresh consumer launch invented an agent")
    require(install.browser_request("GET", "/auth/session")["first_account_needed"] is True, "Missing first-account browser state")
    install.browser_request("GET", "/auth/session", expected=403, headers={"Host": "attacker.invalid"})
    install.browser_request("GET", "/auth/session", expected=403, headers={"Origin": "http://attacker.invalid"})
    setup = read_json(install.home / "secrets/first-account.json")
    install.check("library-assisted fresh setup and actual manager launch publish one local HTTP identity before account setup, without TLS, dummy agent, voice configuration or provider work"
                  if managed_parent else "ordinary no-argument launch publishes one local HTTP identity with no TLS setup, dummy agent, voice configuration or provider work")
    fixture_log = add_acp_fixture(install)
    if browser_test is not None:
        require(browser_node is not None and browser_artifacts is not None, "Browser fixture requires explicit controller Node and artifact paths")
        install.result["browser"] = run_browser(install, browser_test, browser_node, browser_artifacts, setup["token"])
    else:
        first_account(install, setup["token"])
    install.browser_request("POST", "/auth/first-account", {"setup_token": setup["token"], "password": PASSWORD, "start_at_signin": False}, expected=409)
    signed_in = install.browser_request("POST", "/auth/login", {"username": "owner", "password": PASSWORD})
    install.csrf = signed_in["csrf"]
    require(signed_in["authenticated"] and signed_in["auth_mode"] == "local_password", "Local password did not establish the existing browser session flow")
    reply = install.browser_request("POST", "/api/v1/companion/setup", {"agent_id": AGENT_ID, "voice": "marin", "openai_key": FIXTURE_KEY})
    require(reply["saved"] and reply["provider_verified"] is False, "Saving a contact claimed provider readiness")
    require((install.home / "secrets/openai-api-key").read_text() == FIXTURE_KEY, "Fixture provider key was not retained privately")
    account_bytes = (install.home / "secrets/local-password.json").read_bytes()
    require(PASSWORD.encode() not in account_bytes, "Local password was saved in clear text")
    require(not (install.home / "secrets/first-account.json").exists(), "Claimed setup capability remained active")
    room = install.browser_request("POST", "/api/v1/conversations", {"title": "Retained fixture conversation", "agent_ids": [AGENT_ID]}, expected=201)
    with install.db() as database:
        database.execute("UPDATE room_agents SET session_id='retained-fixture-session',endpoint='retained-fixture-endpoint',last_seq=91 WHERE conversation_id=?", (room["id"],))
        database.execute("INSERT INTO grants(hash,conversation_id,expires_at) VALUES(?,?,?)", ("a" * 64, room["id"], 4102444800000))
    write_json(install.home / "retained-unknown-evidence.json", {"historical_outcome": "unknown", "retry": False})
    before = install.snapshot()
    for _ in range(2):
        selected = install.browser_request("POST", "/api/v1/companion/setup", {"agent_id": AGENT_ID, "voice": "marin"})
        require(selected["added"] is False and install.snapshot() == before, "Reselecting contact changed its endpoint, binding, grant or profile")
    public = install.browser_request("GET", "/api/v1/companion/setup")
    require(public["provider_verified"] is False and public["credential"]["configured"] is True, "Credential setup status is not honest")
    with install.db() as database:
        require(database.execute("SELECT count(*) FROM companion_calls").fetchone()[0] == 0, "Setup created a voice call")
        require(not any(FIXTURE_KEY in row[0] or PASSWORD in row[0] for row in database.execute("SELECT data FROM audit")), "Audit exposed private setup input")
    install.check("real local first-account/login and initialize-only ACP connection; saved contact/key and repeated selection preserve original sessions and grants without calls")
    reopened = install.command(["--no-browser"], timeout=30)
    after = install.await_running()
    require(reopened["ready"] and reopened["id"] == config["id"] and after["process_nonce"] == original["process_nonce"], "Second open started another process or identity")
    require((install.home / "config.json").read_bytes() == config_bytes and (install.home / "secrets/local-password.json").read_bytes() == account_bytes,
            "Reopen changed the saved identity or account")
    install.check("second open proves the same actual process and preserves the existing identity/account")
    install.baseline = install.snapshot()
    return {"fixture_log": fixture_log, "original_process": original, "original_child": child,
            "config_bytes": config_bytes, "account_bytes": account_bytes,
            "conversation_id": room["id"], "baseline": install.baseline}


def exercise(install, a, b, *, browser_test=None, browser_node=None, browser_artifacts=None):
    prepared = prepare_consumer_installation(install, browser_test=browser_test,
                                            browser_node=browser_node, browser_artifacts=browser_artifacts)
    fixture_log = prepared["fixture_log"]
    applied = install.update("run", "--package", b)
    require(applied["restored"] is True, "Local HTTP update did not restore availability")
    install.await_running()
    install.selected(b)
    rolled_back = install.update("rollback", applied["transaction_id"])
    require(rolled_back["restored"] is True and rolled_back["transaction_id"] != applied["transaction_id"], "Local HTTP rollback did not use a fresh settled transaction")
    install.await_running()
    install.selected(a)
    require(install.browser_request("GET", "/auth/session")["authenticated"] is True, "Update lost the saved browser session")
    require([json.loads(line)["method"] for line in fixture_log.read_text().splitlines()] == ["initialize"], "Setup or update redispatched ACP work")
    install.check("whole-bundle local HTTP update and fresh rollback preserve account, browser session, contacts, key, grants and historical unknown evidence")


def qualify(archive: Path, receipt: Path, output: Path, *, browser_test=None, browser_node=None, browser_artifacts=None):
    # A controller configuration error must fail before inspecting/extracting a
    # package or publishing any disposable installation identity.
    if browser_test is not None:
        require(browser_node is not None and browser_artifacts is not None,
                "Browser fixture requires explicit controller Node and artifact paths")
        require(isinstance(browser_node, Path) and browser_node.is_absolute()
                and browser_node.is_file() and os.access(browser_node, os.X_OK),
                "Browser fixture requires an absolute executable controller Node file")
    provenance = read_json(receipt)
    package = lifecycle.clean.package_module()
    manifest, contents = package.inspect_archive(archive, provenance["archive_sha256"])
    require(manifest["target"] == lifecycle.clean.native_target(), "Consumer package target differs from actual host")
    require(lifecycle.digest(contents["manifest.json"]) == provenance["manifest_sha256"], "Consumer manifest receipt mismatch")
    root = Path(tempfile.mkdtemp(prefix="Companion consumer café "))
    result = {"schema": 1, "status": "running", "target": manifest["target"], "source_revision": manifest["source_revision"],
              "archive_sha256": provenance["archive_sha256"], "manifest_sha256": provenance["manifest_sha256"],
              "host": {"system": platform.system(), "machine": platform.machine()}, "checks": [],
              "model_requests": 0, "provider_requests": 0, "voice_calls": 0, "speech_release": "unchanged_paused",
              "limitations": ["controller-hosted native package, not a fresh consumer OS or signing qualification",
                  "ACP endpoint is a disposable fixed Instance-launcher simulation using bundled Node and official SDK; initialize only",
                  "A/B have identical tested executable/runtime bytes; B changes version/support document only",
                  "Mac LaunchServices has its OS-managed environment; the separate native CLI launch proves isolated PATH",
                  "no real provider account, physical media, sign-out/sign-in or startup registration is exercised"]}
    installations, failure = [], None
    try:
        a, b = root / "Release A", root / "Release B"
        lifecycle.extract(contents, manifest, a)
        lifecycle.derive_bundle(a, b, "consumer-b")
        install = ConsumerInstallation(root, a, manifest, result, checkpoint_output=output)
        installations.append(("consumer", install))
        exercise(install, a, b, browser_test=browser_test, browser_node=browser_node, browser_artifacts=browser_artifacts)
        if platform.system() == "Darwin":
            app_receipt = provenance["application"]
            app_manifest, app_files = package.inspect_application_archive(archive.parent / app_receipt["archive"], app_receipt["archive_sha256"])
            require(app_manifest == manifest, "Mac application differs from qualified CLI package")
            app_root = root / "Ordinary Mac application"
            app_root.mkdir()
            for name, data in app_files.items():
                path = app_root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            for name in [app_receipt["launcher"], *(app_receipt["bundle"] + "/" + row["path"] for row in manifest["files"] if row["executable"])]:
                (app_root / name).chmod(0o755)
            mac_home_root = root / "LaunchServices installation"
            mac_home_root.mkdir()
            mac = ConsumerInstallation(mac_home_root, app_root / app_receipt["bundle"], manifest, result, checkpoint_output=output)
            installations.append(("mac_application", mac))
            _, before = mac.desktop_start(mac_app=app_root / app_receipt["root"])
            require(mac.api("GET", "/api/v1/agents")["items"] == [], "Ordinary Mac application invented an agent")
            first_account(mac, read_json(mac.home / "secrets/first-account.json")["token"])
            reopened = mac.command(["--no-browser"], timeout=30)
            require(reopened["id"] == before["id"] and mac.await_running()["process_nonce"] == before["process_nonce"], "Mac reopen changed identity/process")
            result["mac_application_archive_sha256"] = app_receipt["archive_sha256"]
            mac.check("actual ordinary Mac .app LaunchServices entrypoint creates the local account and reopens the same installed process")
    except Exception as error:
        failure = error
        result.update(status="failed", error=str(error))
        result["failure_installation_snapshots"] = lifecycle.capture_failure_snapshots(installations)
    finally:
        receipts, cleanup_errors = {}, []
        for name, install in reversed(installations):
            try:
                receipts[name] = install.cleanup(root / "Release B")
            except Exception as error:
                receipts[name] = {"status": "uncertain", "error": str(error)}
                cleanup_errors.append(name)
        result["installation_cleanup"] = receipts
        if cleanup_errors:
            result.update(status="failed", retained_test_directory=str(root))
            failure = failure or RuntimeError("Consumer cleanup remains uncertain")
        else:
            shutil.rmtree(root)
            result["temporary_installation_removed"] = True
            if failure is None:
                result["status"] = "passed"
        lifecycle.persist_checkpoint(output, result)
    if failure:
        raise failure
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["archive", "receipt", "output"]:
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ["browser-test", "browser-node", "browser-artifacts"]:
        parser.add_argument("--" + name, type=Path)
    args = parser.parse_args()
    result = qualify(args.archive.resolve(), args.receipt.resolve(), args.output.resolve(),
                     browser_test=args.browser_test, browser_node=args.browser_node, browser_artifacts=args.browser_artifacts)
    print(json.dumps({"status": result["status"], "checks": len(result["checks"]), "temporary_installation_removed": result.get("temporary_installation_removed")}))
