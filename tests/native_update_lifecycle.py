#!/usr/bin/env python3
"""Exercise a packaged Companion's real standalone/enrolled update lifecycle.

The controller needs Python; every application/updater process has an empty PATH
and no inherited provider credentials. It uses two disposable installations and
only local authenticated HTTP. A/B share the tested executable: B changes a
manifest version and support document. C deliberately uses the bundled Node as
the service to prove failed real startup rolls back. These are lifecycle tests,
not qualification of a different application release, OS login, or signatures.
"""
from __future__ import annotations

import argparse
from contextlib import contextmanager
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

# Do not synchronously close a timed-out Windows pipe while its daemon reader
# may own the buffered stream lock. Keep it alive until exact service cleanup
# releases any writers, or the disposable controller exits.
UNSETTLED_CAPTURES: list[subprocess.Popen] = []


def capture_summary(value) -> dict:
    """Bounded metadata only; command output can contain credentials/nonces."""
    if value is None:
        return {"available": False, "content_retained": False}
    sample = value[:4096]
    encoded = sample.encode("utf-8", errors="replace") if isinstance(sample, str) else sample
    return {"available": True, "sample_bytes": min(len(encoded), 4096),
            "sample_truncated": len(value) > 4096 or len(encoded) > 4096,
            "content_retained": False}


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)


def persist_checkpoint(output: Path | None, result: dict) -> None:
    if output is None:
        return
    output.parent.mkdir(parents=True, exist_ok=True)
    # Replace a complete receipt atomically so a terminated CI controller does
    # not leave a half-written JSON document. No package/application file changes.
    descriptor, name = tempfile.mkstemp(prefix=f".{output.name}-", suffix=".tmp", dir=output.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(result, stream, indent=2)
            stream.write("\n")
            stream.flush()
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def command_label(arguments: list) -> dict:
    # Fixed classifications only: never include argv, paths, input, tokens,
    # transaction/launch nonces, stdout, or stderr in progress evidence.
    words = [str(value) for value in arguments]
    kind = words[0] if words else ""
    allowed = {
        "native-update": {"bootstrap", "plan", "run", "status", "recover", "rollback"},
        "bundle": {"verify", "install"}, "credential": {"import-openai"},
    }
    if kind in allowed:
        return {"kind": kind, "action": words[1] if len(words) > 1 and words[1] in allowed[kind] else "unknown"}
    if kind == "init":
        return {"kind": "init"}
    if kind == "native-update-protocol":
        def fixed(flag, choices):
            at = words.index(flag) + 1 if flag in words else len(words)
            return words[at] if at < len(words) and words[at] in choices else "unknown"
        return {"kind": kind, "mode": fixed("--mode", {"enrollment", "provider", "participant"}),
                "action": fixed("--action", {"enroll", "registration", "status", "plan", "prepare", "apply", "verify", "restore", "rollback"})}
    return {"kind": "unknown"}


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
    def __init__(self, root: Path, bundle: Path, manifest: dict, result: dict, *, checkpoint_output: Path | None = None):
        self.root, self.bundle, self.manifest, self.result = root, bundle, manifest, result
        self.checkpoint_output = checkpoint_output
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
        self.unsettled_commands: list[subprocess.Popen] = []
        self.streams = []
        self.ports: set[int] = set()
        self.fixture_calls: set[str] = set()
        self.baseline = None

    def checkpoint(self):
        persist_checkpoint(self.checkpoint_output, self.result)

    def begin_operation(self, operation: dict, timeout: int) -> dict:
        sequence = self.result.get("native_operation_count", 0) + 1
        self.result["native_operation_count"] = sequence
        current = {"sequence": sequence, "operation": operation, "phase": "running", "timeout_seconds": timeout}
        self.result["current_operation"] = current
        self.checkpoint()
        return current

    def end_operation(self, operation: dict, started: float, outcome: str, return_code=None):
        completed = {**operation, "phase": "completed", "outcome": outcome,
                     "elapsed_seconds": time.monotonic() - started}
        if return_code is not None:
            completed["return_code"] = return_code
        self.result["current_operation"] = completed
        timings = self.result.setdefault("native_operation_timings", [])
        timings.append(completed)
        if len(timings) > 128:
            del timings[:-128]
            self.result["native_operation_timings_omitted"] = self.result.get("native_operation_timings_omitted", 0) + 1
        self.checkpoint()

    def command(self, arguments: list, *, expect_success: bool = True, extra_env=None, timeout=120, input_text=None):
        env = dict(self.env)
        env.update(extra_env or {})
        operation = self.begin_operation(command_label(arguments), timeout)
        started, outcome, return_code = time.monotonic(), "error", None
        try:
            argv = [str(self.worker), "--home", str(self.home), *map(str, arguments)]
            process = subprocess.Popen(argv, env=env, cwd=self.root,
                                       stdin=subprocess.PIPE if input_text is not None else None,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       text=True, encoding="utf-8")
            try:
                stdout, stderr = process.communicate(input=input_text, timeout=timeout)
            except subprocess.TimeoutExpired as first_timeout:
                outcome = "timeout"
                return_code = process.poll()
                diagnostic = {"helper_return_code_at_timeout": return_code,
                              "helper_alive_at_timeout": return_code is None,
                              "kill_attempted": return_code is None, "capture_settled": False,
                              "final_drain_seconds": 10, "operation_outcome": "unknown"}
                operation["timeout_diagnostic"] = diagnostic
                self.checkpoint()
                if return_code is None:
                    try:
                        process.kill()  # Only this exact Popen child, never descendants/PID lookup.
                    except OSError as error:
                        diagnostic["kill_error"] = type(error).__name__
                        diagnostic["kill_errno"] = error.errno
                stdout, stderr = first_timeout.stdout, first_timeout.stderr
                try:
                    # subprocess.run() uses an unbounded drain here on Windows.
                    # Do not enter a Popen context manager or call unbounded wait.
                    stdout, stderr = process.communicate(timeout=10)
                    diagnostic["capture_settled"] = True
                except subprocess.TimeoutExpired as drain_timeout:
                    if drain_timeout.stdout is not None:
                        stdout = drain_timeout.stdout
                    if drain_timeout.stderr is not None:
                        stderr = drain_timeout.stderr
                    self.unsettled_commands.append(process)
                    UNSETTLED_CAPTURES.append(process)
                return_code = process.poll()
                diagnostic.update(helper_return_code_after_drain=return_code,
                                  helper_exit_observed=return_code is not None,
                                  stdout=capture_summary(stdout), stderr=capture_summary(stderr))
                # A late successful exit is still a failed timed-out operation.
                # TimeoutExpired's own string exposes argv, so never propagate it.
                raise TimeoutError(f"Native {operation['operation']} exceeded {timeout}s; "
                                   f"helper_return_code={return_code}, capture_settled={diagnostic['capture_settled']}; "
                                   "operation outcome remains unknown") from None
            completed = subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)
            return_code = completed.returncode
            outcome = "succeeded" if return_code == 0 else "nonzero_exit"
            if expect_success:
                require(return_code == 0, f"Native {operation['operation']} failed with exit code {return_code}")
                return json.loads(completed.stdout)
            require(return_code != 0, f"Native {operation['operation']} unexpectedly succeeded")
            return completed
        finally:
            self.end_operation(operation, started, outcome, return_code)

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
        # Match the accepted shared Worker's participant clocks rather than
        # letting this controller's general command allowance hide a hook overrun.
        timeout = (300 if action == "verify" else 30) if mode == "participant" else 120
        started = time.monotonic()
        try:
            return self.command(["native-update-protocol", "--config", self.owner_config,
                                 "--mode", mode, "--action", action], extra_env=extra,
                                expect_success=expect_success, timeout=timeout)
        finally:
            self.result.setdefault("protocol_timings", []).append({
                "mode": mode, "action": action, "timeout_seconds": timeout,
                "elapsed_seconds": time.monotonic() - started})
            self.checkpoint()

    @contextmanager
    def db(self):
        db = sqlite3.connect(self.home / "state.sqlite3", timeout=5)
        try:
            db.execute("PRAGMA foreign_keys=ON")
            with db:
                yield db
        finally:
            # sqlite3's transaction context commits/rolls back but does not close
            # the connection. Close before cleanup can remove WAL/SHM files.
            db.close()

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
        operation = self.begin_operation({"kind": "service_start"}, 30)
        started, outcome = time.monotonic(), "error"
        try:
            child = subprocess.Popen([str(executable), "--home", str(self.home), "serve"],
                                     env=self.env, cwd=self.root, stdout=stream, stderr=stream)
            self.children.append(child)
            healthy = self.await_running()
            outcome = "healthy"
            return child, healthy
        finally:
            self.end_operation(operation, started, outcome)

    def initialize(self, *, baseline_bundle: Path | None = None, launch_initial: bool = True):
        baseline_bundle = baseline_bundle or self.bundle
        baseline_manifest = read_json(baseline_bundle / "manifest.json")
        workspace = self.root / "Agent workspace"
        workspace.mkdir()
        owner = os.environ.get("USERNAME") or os.environ.get("USER") or "fixture-owner"
        self.command(["init", "--owner", owner, "--agent-command", self.node,
                      "--agent-arg=--version", "--agent-cwd", workspace], timeout=60)
        write_json(self.home / "companion.json", {"enabled": False,
                   "node": str(baseline_bundle / baseline_manifest["entrypoints"]["node"]),
                   "bridge": str(baseline_bundle / baseline_manifest["entrypoints"]["bridge"]), "profiles": {}, "max_call_seconds": 17})
        self.update("bootstrap", "--package", baseline_bundle)
        if launch_initial:
            child, _ = self.start()
            # This is an exact Popen child before any update/worker-created process.
            child.terminate()
            child.wait(timeout=10)
        self.await_stopped()
        imported = self.command(["credential", "import-openai"],
                                input_text="fixture-native-key-not-a-real-provider-credential\n", timeout=60)
        require(imported["stored"] is True, "Credential import failed")
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
        self.check("initialized real service/database; retained room, session, settled work, historical uncertainty and private credential"
                   if launch_initial else
                   "initialized real CLI/database and protected state with deliberately incompatible baseline stopped")

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
        self.checkpoint()

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
        require(all(process.poll() is not None for process in self.unsettled_commands),
                "Timed-out native helper exit remains unknown; retaining its disposable installation")
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


def exercise_failed_explicit_rollback(install: Installation, a: Path, b: Path, c: Path):
    # The original immutable C is intentionally unstartable, not edited after
    # capture. A's real CLI initializes the database without launching C; the
    # ordinary C -> B update then establishes an actual verified B installation.
    manifests_before = {name: digest((bundle / "manifest.json").read_bytes())
                        for name, bundle in [("A", a), ("B", b), ("C", c)]}
    install.initialize(baseline_bundle=c, launch_initial=False)
    install.selected(c)
    require(install.installed()["package"]["binary_sha256"] == digest(install.node.read_bytes()),
            "Failed rollback fixture is not the actual bundled Node executable")
    applied = install.update("run", "--package", b)
    require(applied.get("restored") is True and install.status()["original_running"] is False,
            "C -> B did not complete actual verification with stopped state preserved")
    install.await_stopped()
    install.selected(b)
    requested_plan = copy.deepcopy(install.journal()["frozen_plan"])
    require(requested_plan["previous"]["bundle"]["manifest_sha256"] == manifests_before["C"]
            and requested_plan["candidate"]["bundle"]["manifest_sha256"] == manifests_before["B"],
            "Completed transaction did not retain exact C -> B plan")
    _, before = install.start()
    failure = install.update("rollback", applied["transaction_id"], expect_success=False)
    require("Requested rollback failed; the pre-rollback bundle was verified and restored unchanged" in failure.stderr,
            "Failed explicit rollback did not report verified return to B")
    journal = install.journal()
    require(journal["transaction_id"] != applied["transaction_id"] and journal["rollback_aborted"] is True
            and journal["phase"] == "restored" and journal["direction"] == "unchanged"
            and journal["restore_outcome"] == "unchanged" and journal["selected"] == "previous",
            "Explicit rollback failure did not settle its durable abort selection")
    require(journal["rollback_failure"] and "Candidate exited before verification" in journal["rollback_failure"],
            "Rollback failed before the deliberately incompatible process was actually tested")
    require(journal["frozen_plan"] == requested_plan and journal["context"]["rollback_plan"] == requested_plan
            and journal["context"]["previous_transaction"] == applied["transaction_id"]
            and journal["previous"]["bundle"]["manifest_sha256"] == manifests_before["B"]
            and journal["candidate"]["bundle"]["manifest_sha256"] == manifests_before["C"],
            "Abort changed the original requested rollback target or retained plan")
    restored = install.await_running()
    require(restored["process_nonce"] != before["process_nonce"] and not restored["admission"]["closed"],
            "Abort did not verify and restore an actual fresh B service with open admission")
    install.selected(b)
    recovered = install.update("recover", journal["transaction_id"])
    require(recovered.get("restored") is True and recovered.get("rollback_aborted") is True
            and recovered.get("outcome") == "unchanged", "Recovery obscured the failed rollback outcome")
    require(install.await_running()["process_nonce"] == restored["process_nonce"],
            "Completed rollback-abort recovery restarted the working service")
    require(install.journal() == journal, "Completed recovery rewrote original failed rollback evidence")
    install.selected(b)
    require({name: digest((bundle / "manifest.json").read_bytes())
             for name, bundle in [("A", a), ("B", b), ("C", c)]} == manifests_before,
            "Failed rollback qualification edited an immutable source manifest")
    # Verify all canonical installed/source bundle bytes too, not just their
    # manifest pointers. These calls inspect files and never start a service.
    for bundle in [a, b, c, Path(journal["previous"]["bundle"]["bundle"]), Path(journal["candidate"]["bundle"]["bundle"])]:
        install.command(["bundle", "verify", "--path", bundle])
    install.check("real explicit rollback to retained incompatible C fails startup, aborts to verified running B, and preserves original intent/state",
                  applied_transaction=applied["transaction_id"], rollback_transaction=journal["transaction_id"],
                  rollback_aborted=True, outcome="unchanged", preserved_manifest_sha256=manifests_before)


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
    installations = []
    failure = None
    try:
        a, b, c = root / "Release A", root / "Release B", root / "Deliberate failed C"
        result["current_operation"] = {"operation": {"kind": "fixture", "action": "extract"}, "phase": "running"}
        persist_checkpoint(output, result)
        extract(contents, manifest, a)
        result["current_operation"] = {"operation": {"kind": "fixture", "action": "derive_candidates"}, "phase": "running"}
        persist_checkpoint(output, result)
        derive_bundle(a, b, "b")
        derive_bundle(a, c, "incompatible-c", broken=True)
        result["fixtures"] = {name: digest((bundle / "manifest.json").read_bytes()) for name, bundle in [("A", a), ("B", b), ("C", c)]}
        install = Installation(root, a, manifest, result, checkpoint_output=output)
        installations.append(("primary", install))
        exercise(install, a, b, c)
        secondary_root = root / "Explicit rollback failure installation"
        secondary_root.mkdir()
        secondary = Installation(secondary_root, a, manifest, result, checkpoint_output=output)
        installations.append(("explicit_rollback_failure", secondary))
        exercise_failed_explicit_rollback(secondary, a, b, c)
    except Exception as error:
        failure = error
        result.update(status="failed", error=str(error))
    finally:
        try:
            cleanup_errors = []
            cleanup_receipts = {}
            # Attempt every owned cleanup, even if another installation cannot
            # settle. Never delete either home beneath an uncertain process.
            for name, installation in reversed(installations):
                try:
                    cleanup_receipts[name] = installation.cleanup(root / "Release B")
                except Exception as cleanup_error:
                    cleanup_errors.append(f"{name}: {cleanup_error}")
                    cleanup_receipts[name] = {"status": "uncertain", "error": str(cleanup_error)}
            result["installation_cleanup"] = cleanup_receipts
            if cleanup_errors:
                raise RuntimeError("; ".join(cleanup_errors))
            result["cleanup"] = cleanup_receipts.get("primary", {"no_application_started": True})
            result["current_operation"] = {"operation": {"kind": "fixture", "action": "remove_temporary_files"}, "phase": "running"}
            persist_checkpoint(output, result)
            shutil.rmtree(root)
            result["temporary_installation_removed"] = True
            result["current_operation"]["phase"] = "completed"
            if failure is None:
                result["status"] = "passed"
        except Exception as cleanup_error:
            result["status"] = "failed"
            result["cleanup"] = {"status": "uncertain", "error": str(cleanup_error), "retained_test_directory": str(root)}
            failure = failure or cleanup_error
        persist_checkpoint(output, result)
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
