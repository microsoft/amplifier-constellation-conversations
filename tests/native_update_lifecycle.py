#!/usr/bin/env python3
"""Exercise a packaged Companion's real standalone/enrolled update lifecycle.

The controller needs Python; every application/updater process has an empty PATH
and no inherited provider credentials. It uses one disposable installation and
only local authenticated HTTP. A/B share the tested executable: B changes a
manifest version and support document. C deliberately uses the bundled Node as
the service to prove failed real startup rolls back. These are lifecycle tests,
not qualification of a different application release, OS login, or signatures.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("native_clean_install", ROOT / "tests/native_clean_install.py")
clean = importlib.util.module_from_spec(spec)
spec.loader.exec_module(clean)
require = clean.require


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)


def extract(contents: dict[str, bytes], manifest: dict, destination: Path) -> None:
    destination.mkdir()
    for name, data in contents.items():
        target = destination.joinpath(*name.split("/"))
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    for row in manifest["files"]:
        if row["executable"]:
            (destination / row["path"]).chmod(0o755)


def derive_bundle(source: Path, destination: Path, label: str, *, broken: bool = False) -> dict:
    """Make an explicitly synthetic release with independently copied bytes."""
    shutil.copytree(source, destination)
    manifest = read_json(destination / "manifest.json")
    manifest["version"] += "+lifecycle-" + label
    relative = "ops/native-update-lifecycle-fixture.txt"
    target = destination / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    payload = ("Disposable update lifecycle fixture " + label + "\n").encode()
    target.write_bytes(payload)
    manifest["files"].append({"path": relative, "sha256": digest(payload), "bytes": len(payload), "executable": False})
    if broken:
        service = manifest["entrypoints"]["service"]
        payload = (destination / manifest["entrypoints"]["node"]).read_bytes()
        (destination / service).write_bytes(payload)
        for row in manifest["files"]:
            if row["path"] == service:
                row.update(sha256=digest(payload), bytes=len(payload))
    write_json(destination / "manifest.json", manifest)
    return manifest


def participant_context(transaction: str, components: list[str], *, plan_id: str | None = None) -> dict:
    return {"id": plan_id or str(uuid.uuid4()), "participant": "companion", "plan": {
        "schema": 1, "transaction_id": transaction, "operation": "apply", "components": components,
        "restart_scope": {"services": [], "instance": False, "slack_homes": []}}}


class Installation:
    def __init__(self, root: Path, bundle: Path, manifest: dict, result: dict):
        self.root, self.bundle, self.manifest, self.result = root, bundle, manifest, result
        self.home = root / "Companion installation 日本語"
        self.worker = bundle / manifest["entrypoints"]["service"]
        self.node = bundle / manifest["entrypoints"]["node"]
        self.owner_config = root / "Fixed owner configuration" / "config.json"
        user = root / "Isolated user"
        user.mkdir()
        empty = root / "Empty PATH"
        empty.mkdir()
        self.env = clean.isolated_environment(user, empty)
        self.children: list[subprocess.Popen] = []
        self.streams = []
        self.ports: set[int] = set()
        self.fixture_calls: set[str] = set()
        self.baseline = None

    def command(self, arguments: list, *, expect_success: bool = True, extra_env=None, timeout=120):
        env = dict(self.env)
        env.update(extra_env or {})
        completed = subprocess.run([str(self.worker), "--home", str(self.home), *map(str, arguments)],
                                   env=env, cwd=self.root, capture_output=True, text=True,
                                   encoding="utf-8", timeout=timeout)
        if expect_success:
            require(completed.returncode == 0, f"Native command {arguments[:2]} failed: {completed.stderr[-3000:]}")
            return json.loads(completed.stdout)
        require(completed.returncode != 0, f"Native command unexpectedly succeeded: {arguments[:2]}")
        return completed

    def update(self, *arguments, **kwargs):
        return self.command(["native-update", *arguments], **kwargs)

    def status(self):
        return self.update("status")

    def journal(self):
        return read_json(self.home / "native-update.json")

    def protocol(self, mode: str, action: str, *, transaction=None, envelope=None, outcome="unchanged", expect_success=True):
        extra = {}
        if transaction:
            path = self.root / "Protocol envelopes" / f"{transaction}-{mode}-{action}-{uuid.uuid4()}.json"
            write_json(path, envelope)
            extra = {"CONSTELLATION_UPDATE_TRANSACTION": transaction,
                     "CONSTELLATION_UPDATE_PLAN": str(path), "CONSTELLATION_UPDATE_OUTCOME": outcome}
        return self.command(["native-update-protocol", "--config", self.owner_config,
                             "--mode", mode, "--action", action], extra_env=extra, expect_success=expect_success)

    def db(self):
        db = sqlite3.connect(self.home / "state.sqlite3", timeout=5)
        db.execute("PRAGMA foreign_keys=ON")
        return db

    def api(self, method="GET", path="/api/v1/native-update/status", body=None):
        runtime = read_json(self.home / "runtime.json")
        self.ports.add(runtime["port"])
        headers = {"Authorization": "Bearer " + (self.home / "secrets/control-token").read_text(encoding="utf-8").strip()}
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        request = urllib.request.Request(runtime["url"] + path, data=data, headers=headers, method=method)
        with urllib.request.urlopen(request, context=clean.strict_tls_context(self.home / "tls/root-ca.crt"), timeout=3) as response:
            return json.load(response)

    def running(self):
        try:
            value = self.api()
        except (OSError, ValueError):
            return None
        require(value["id"] == read_json(self.home / "config.json")["id"], "Unexpected listener identity")
        return value

    def await_running(self):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            value = self.running()
            if value:
                require(value["binary_sha256"] == self.installed()["package"]["binary_sha256"], "Running executable differs from selected bundle")
                return value
            time.sleep(0.1)
        raise AssertionError("Owned service did not become healthy")

    def await_stopped(self):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if not self.running() and self.service_lock_available():
                return
            time.sleep(0.1)
        raise AssertionError("Owned service did not stop or retained service.lock")

    def service_lock_available(self):
        # Match Rust fs2's process exclusion. Never infer death from a stale PID.
        path = self.home / "service.lock"
        if not path.exists():
            return True
        with path.open("r+b") as stream:
            try:
                if os.name == "nt":
                    import msvcrt
                    msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
                    msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(stream, fcntl.LOCK_UN)
                return True
            except OSError:
                return False

    def installed(self):
        return read_json(self.home / "native-installation.json")

    def start(self):
        require(self.service_lock_available(), "A service already owns the installation")
        executable = self.home / "bin" / self.worker.name
        stream = (self.root / f"owned-service-{len(self.children)}.log").open("w", encoding="utf-8")
        self.streams.append(stream)
        child = subprocess.Popen([str(executable), "--home", str(self.home), "serve"],
                                 env=self.env, cwd=self.root, stdout=stream, stderr=stream)
        self.children.append(child)
        return child, self.await_running()

    def initialize(self):
        workspace = self.root / "Agent workspace"
        workspace.mkdir()
        owner = os.environ.get("USERNAME") or os.environ.get("USER") or "fixture-owner"
        clean.run([self.worker, "--home", self.home, "init", "--owner", owner,
                   "--agent-command", self.node, "--agent-arg=--version", "--agent-cwd", workspace], self.env, self.root)
        write_json(self.home / "companion.json", {"enabled": False, "node": str(self.node),
                   "bridge": str(self.bundle / self.manifest["entrypoints"]["bridge"]), "profiles": {}, "max_call_seconds": 17})
        self.update("bootstrap", "--package", self.bundle)
        child, _ = self.start()
        # This is an exact Popen child before any update/worker-created process.
        child.terminate()
        child.wait(timeout=10)
        self.await_stopped()
        imported = clean.run([self.worker, "--home", self.home, "credential", "import-openai"], self.env, self.root,
                             input_text="fixture-native-key-not-a-real-provider-credential\n")
        require(json.loads(imported.stdout)["stored"] is True, "Credential import failed")
        with self.db() as db:
            db.execute("INSERT INTO conversations(id,title,created_at,updated_at,status,note,session_id,owner) VALUES('retained-room','Retained room',100,100,'ready','Retained note','retained-session',?)", (owner,))
            db.execute("INSERT INTO room_members VALUES('retained-room',?,'owner',100)", (owner,))
            db.execute("INSERT INTO room_agents(conversation_id,agent_id,session_id,endpoint,last_seq) VALUES('retained-room','default','retained-session','retained-endpoint',0)")
            db.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('retained-message','retained-room','assistant','Earlier completed work; do not replay.','complete',100,100,'fixture','default')")
            self.insert_call(db, "retained-call", "closed")
            db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES('retained-call','retained-delegation',0,'completed','retained-message',100)")
            db.execute("INSERT INTO companion_actions VALUES('retained-call','retained-action','retained-delegation','fixture-connection','Earlier settled work.','{}','completed','{\"state\":\"completed\"}',100)")
        (self.home / "retained-unknown-evidence.json").write_text('{"historical_outcome":"unknown","retry":false}\n', encoding="utf-8")
        self.baseline = self.snapshot()
        self.check("initialized real service/database; retained room, session, settled work, historical uncertainty and private credential")

    @staticmethod
    def insert_call(db, identity: str, state: str):
        db.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES(?, 'fixture-owner',?, 'fixture-hash','default','retained-room',?,100,100,'fixture-endpoint')", (identity, identity, state))

    def snapshot(self):
        files = {str(path.relative_to(self.home)): digest(path.read_bytes())
                 for path in [self.home / "config.json", self.home / "retained-unknown-evidence.json",
                              *sorted((self.home / "secrets").glob("*")), *sorted((self.home / "tls").glob("*"))] if path.is_file()}
        tables = ["conversations", "messages", "room_members", "room_agents", "companion_calls",
                  "companion_delegations", "companion_actions", "input_deliveries", "companion_observations"]
        with self.db() as db:
            rows = {table: sorted([list(row) for row in db.execute(f'SELECT * FROM "{table}"')], key=repr) for table in tables}
            endpoint = db.execute("SELECT config FROM agent_endpoints WHERE id='default'").fetchone()[0]
        companion = read_json(self.home / "companion.json")
        companion.pop("node")
        companion.pop("bridge")
        return {"files": files, "tables": rows, "agent_configuration": endpoint, "companion_policy": companion}

    def continuity(self):
        require(self.snapshot() == self.baseline, "Retained application configuration, credential, room/session/work or unknown evidence changed")
        configured = read_json(self.home / "companion.json")
        selected = self.installed()["package"]["bundle"]["entrypoints"]
        for key in ["node", "bridge"]:
            require(Path(configured[key]).samefile(selected[key]), "Whole-bundle binding did not follow selected manifest")

    def selected(self, bundle: Path):
        require(self.status()["selected_manifest"] == digest((bundle / "manifest.json").read_bytes()), "Wrong immutable bundle selected")
        self.continuity()

    def check(self, text: str, **evidence):
        self.result["checks"].append({"name": text, **evidence})

    def enroll(self, candidate: Path):
        write_json(self.owner_config, {"schema": 1, "installation_id": read_json(self.home / "config.json")["id"],
            "home": str(self.home), "component_id": "companion", "participant_id": "companion",
            "components": ["companion", "instance"], "candidate": str(candidate), "helper_package": str(self.bundle),
            "helper_manifest_sha256": digest((self.bundle / "manifest.json").read_bytes()), "helper_executable": str(self.worker)})
        self.protocol("enrollment", "enroll")
        registration = self.protocol("enrollment", "registration")
        require(registration["components"][0]["restart_scope"] == {"services": [], "instance": False, "slack_homes": []}, "Generic worker would duplicate service ownership")
        require(registration["participants"][0]["protocol"] == 1, "Unexpected lifecycle protocol")
        return registration

    def busy_cases(self, candidate: Path):
        before_process = self.await_running()
        for kind in ["call", "work", "unknown"]:
            identity = "busy-fixture-" + str(uuid.uuid4())
            self.fixture_calls.add(identity)
            with self.db() as db:
                self.insert_call(db, identity, "connected" if kind == "call" else "closed")
                if kind == "work":
                    db.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES(?,?,0,'dispatched','retained-message',100)", (identity, identity))
                if kind == "unknown":
                    db.execute("INSERT INTO companion_actions VALUES(?,?,?,'fixture-connection','No actual effect.','{}','unknown','{\"transport_submitted\":null}',100)", (identity, identity, identity))
            before = self.snapshot()
            declined = self.update("run", "--package", candidate)
            require(declined.get("ready") is False, f"{kind} did not defer update")
            require(self.snapshot() == before, f"Deferred {kind} altered original evidence")
            after = self.await_running()
            require(after["process_nonce"] == before_process["process_nonce"], "Busy update restarted the service")
            require(after["admission"]["closed"] is False, "Declined prepare retained its admission gate")
            self.remove_fixture_call(identity)
            self.continuity()
            self.check(f"durable {kind} fixture defers prepare without service change, acknowledgement or evidence mutation")

    def remove_fixture_call(self, identity):
        with self.db() as db:
            db.execute("DELETE FROM companion_calls WHERE id=?", (identity,))
        self.fixture_calls.discard(identity)

    def cleanup(self, candidate: Path):
        for identity in list(self.fixture_calls):
            self.remove_fixture_call(identity)
        current = self.running()
        if current:
            journal_path = self.home / "native-update.json"
            journal = read_json(journal_path) if journal_path.exists() else None
            if journal and journal["phase"] != "restored" and current["admission"]["closed"]:
                self.api("POST", "/api/v1/native-update/stop", {"transaction_id": journal["transaction_id"], "process_nonce": current["process_nonce"]})
            else:
                if not self.owner_config.exists():
                    self.enroll(candidate)
                transaction = str(uuid.uuid4())
                context = participant_context(transaction, ["companion"])
                value = self.protocol("participant", "prepare", transaction=transaction, envelope=context)
                require(value.get("ready") is True, "Cleanup cannot quiesce the exact owned installation")
        # Reap only Popen children actually created by this controller. A stale
        # PID from runtime.json is never a kill target.
        for child in self.children:
            if child.poll() is None:
                if current is None:
                    child.terminate()
                child.wait(timeout=12)
        self.await_stopped()
        for port in self.ports:
            with socket.socket() as connection:
                connection.settimeout(0.2)
                require(connection.connect_ex(("127.0.0.1", port)) != 0, "A test listener remains open")
        for stream in self.streams:
            stream.close()
        return {"service_lock_released": True, "owned_listener_ports_closed": True,
                "owned_children_reaped": True, "final_disposable_home_retired_without_restore": True}


def exercise(install: Installation, a: Path, b: Path, c: Path):
    install.initialize()
    applied = install.update("run", "--package", b)
    require(applied["restored"] is True and install.status()["original_running"] is False, "Stopped update did not restore stopped state")
    install.await_stopped()
    install.selected(b)
    install.check("stopped A→B performs actual gated verification and returns to stopped", transaction=applied["transaction_id"])
    rollback = install.update("rollback", applied["transaction_id"])
    require(rollback["transaction_id"] != applied["transaction_id"], "Explicit rollback reused completed transaction")
    install.await_stopped()
    install.selected(a)
    install.check("explicit completed-update rollback creates fresh stopped transaction", transaction=rollback["transaction_id"])
    _, old_process = install.start()
    install.busy_cases(b)
    applied = install.update("run", "--package", b)
    running = install.await_running()
    require(running["process_nonce"] != old_process["process_nonce"] and not running["admission"]["closed"], "Running update did not restore a new open service")
    install.selected(b)
    install.check("running A→B stops old process, verifies actual candidate HTTPS and restores availability", transaction=applied["transaction_id"])
    install.update("recover", applied["transaction_id"])
    require(install.await_running()["process_nonce"] == running["process_nonce"], "Completed recovery restarted service")
    install.check("fresh recovery process consumes completed standalone receipt without restarting")
    install.update("rollback", applied["transaction_id"])
    install.await_running()
    install.selected(a)
    install.check("explicit fresh rollback preserves originally running state and original bundle bindings")
    failure = install.update("run", "--package", c, expect_success=False)
    require("previous bundle was restored" in failure.stderr, "Failed candidate did not report verified rollback")
    require(install.journal()["direction"] == "rolled_back" and install.journal()["phase"] == "restored", "Failure did not settle verified rollback")
    install.await_running()
    install.selected(a)
    install.check("deliberately incompatible native Node-as-service candidate fails real launch and rolls back actual app")

    registration = install.enroll(b)
    install.update("run", "--package", b, expect_success=False)
    install.check("one enrolled owner supplies protocol 2/lifecycle 1 fixed commands with empty generic restart scope", component=registration["components"][0]["id"])
    frozen = install.protocol("provider", "plan")
    transaction = str(uuid.uuid4())
    context = participant_context(transaction, ["companion", "unrecognized-future-component"])
    envelope = {"id": context["id"], "component": "companion", "plan": frozen}
    require(install.protocol("participant", "prepare", transaction=transaction, envelope=context)["ready"], "Enrolled prepare failed")
    install.await_stopped()
    changed = copy.deepcopy(context)
    changed["plan"]["components"].append("instance")
    install.protocol("participant", "verify", transaction=transaction, envelope=changed, expect_success=False)
    install.protocol("provider", "apply", transaction=transaction, envelope=envelope)
    install.protocol("provider", "verify", transaction=transaction, envelope=envelope, outcome="applied")
    install.selected(b)
    require(install.await_running()["admission"]["closed"], "Candidate verification prematurely opened admission")
    install.protocol("provider", "rollback", transaction=transaction, envelope=envelope, outcome="rolled_back")
    install.protocol("participant", "verify", transaction=transaction, envelope=context, outcome="rolled_back")
    install.protocol("participant", "restore", transaction=transaction, envelope=context, outcome="rolled_back")
    restored = install.await_running()
    install.selected(a)
    install.protocol("participant", "restore", transaction=transaction, envelope=context, outcome="rolled_back")
    require(install.await_running()["process_nonce"] == restored["process_nonce"], "Lost restore acknowledgement repeated startup")
    install.protocol("provider", "rollback", transaction=transaction, envelope=envelope, outcome="rolled_back", expect_success=False)
    install.check("enrolled real verify/rollback re-quiesces candidate, preserves frozen context and restores once", transaction=transaction)

    transaction = str(uuid.uuid4())
    context = participant_context(transaction, ["instance"])
    require(install.protocol("participant", "prepare", transaction=transaction, envelope=context)["ready"], "Shared-only prepare ignored enrolled participant")
    install.await_stopped()
    install.protocol("participant", "verify", transaction=transaction, envelope=context, outcome="applied")
    # Explicit disposable durable checkpoint injection: the real finish request
    # opens admission, but the updater journal still records restoring. This is
    # the crash/lost-ack window, not a completed receipt replay or a process kill.
    journal = install.journal()
    require(journal["phase"] == "verified", "Lost-ack fixture requires actual successful verification")
    journal.update(restore_started=True, phase="restoring", restore_outcome="applied", restore_boundary_verified=True)
    write_json(install.home / "native-update.json", journal)
    install.api("POST", "/api/v1/native-update/finish", {"transaction_id": transaction, "launch_nonce": journal["launch_nonce"]})
    opened = install.await_running()
    require(not opened["admission"]["closed"], "Real finish did not reopen admission")
    identity = "post-finish-unknown-" + str(uuid.uuid4())
    install.fixture_calls.add(identity)
    with install.db() as db:
        install.insert_call(db, identity, "closed")
        db.execute("INSERT INTO companion_actions VALUES(?,?,?,'fixture-connection','New work after admission reopened.','{}','unknown','{\"transport_submitted\":null}',100)", (identity, identity, identity))
    post_finish = install.snapshot()
    install.protocol("participant", "restore", transaction=transaction, envelope=context, outcome="applied")
    restored = install.await_running()
    require(restored["process_nonce"] == opened["process_nonce"], "Lost finish acknowledgement restarted service")
    require(install.snapshot() == post_finish, "Restore-only recovery altered newly arrived unknown work")
    require(install.journal()["phase"] == "restored", "Restore-only recovery did not settle its journal")
    install.remove_fixture_call(identity)
    install.selected(a)
    install.protocol("participant", "restore", transaction=transaction, envelope=context, outcome="applied")
    require(install.await_running()["process_nonce"] == restored["process_nonce"], "Repeated shared-only restoration restarted service")
    install.check("shared-only real lifecycle and lost finish acknowledgement recover without reverify or restart; new unknown work preserved",
                  transaction=transaction, fault_injection="durable restoring journal checkpoint before actual finish; no process killed")
    require(not restored["admission"]["closed"], "Final restoration left admission closed")


def qualify(archive: Path, receipt: Path, output: Path) -> dict:
    provenance = read_json(receipt)
    manifest, contents = clean.package_module().inspect_archive(archive, provenance["archive_sha256"])
    require(manifest["target"] == clean.native_target(), "Package is not the actual native target")
    require(digest(contents["manifest.json"]) == provenance["manifest_sha256"], "Manifest receipt mismatch")
    root = Path(tempfile.mkdtemp(prefix="Native lifecycle café "))
    result = {"schema_version": 1, "status": "running", "target": manifest["target"],
              "source_revision": manifest["source_revision"], "archive_sha256": provenance["archive_sha256"],
              "manifest_sha256": provenance["manifest_sha256"], "checks": [],
              "host": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
              "scope": "real packaged local HTTPS/CLI/SQLite lifecycle with developer tools absent from child PATH",
              "limitations": ["A/B use the same tested service binary, private Node and SDK bytes",
                  "B changes a support document and manifest version; C deliberately replaces service with bundled Node",
                  "controller-hosted package test, not a fresh consumer OS or different release migration",
                  "no OS startup registration/login in this harness; qualified separately",
                  "no physical media, provider, model, game, or voice call; speech release remains paused"],
              "model_requests": 0, "speech_release": "unchanged_paused"}
    install = None
    failure = None
    try:
        a, b, c = root / "Release A", root / "Release B", root / "Deliberate failed C"
        extract(contents, manifest, a)
        derive_bundle(a, b, "b")
        derive_bundle(a, c, "incompatible-c", broken=True)
        result["fixtures"] = {name: digest((bundle / "manifest.json").read_bytes()) for name, bundle in [("A", a), ("B", b), ("C", c)]}
        install = Installation(root, a, manifest, result)
        exercise(install, a, b, c)
        result["status"] = "passed"
    except Exception as error:
        failure = error
        result.update(status="failed", error=str(error))
    finally:
        try:
            if install is not None:
                result["cleanup"] = install.cleanup(root / "Release B")
            shutil.rmtree(root)
            result["temporary_installation_removed"] = True
        except Exception as cleanup_error:
            result["status"] = "failed"
            result["cleanup"] = {"status": "uncertain", "error": str(cleanup_error), "retained_test_directory": str(root)}
            failure = failure or cleanup_error
        output.parent.mkdir(parents=True, exist_ok=True)
        write_json(output, result)
    if failure:
        raise failure
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    print(json.dumps(qualify(arguments.archive.resolve(), arguments.receipt.resolve(), arguments.output.resolve())))
