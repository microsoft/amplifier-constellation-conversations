#!/usr/bin/env python3
"""Compose real packaged browser/API updates with protected-state checkpoints.

The controller owns only its disposable installation and exact registrations.
Private authentication travels over stdin, never argv or retained receipts.
The manager-bound fixture uses the actual CI user's manager session; it makes
no empty-HOME, fresh consumer OS, physical media or provider readiness claim.
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import platform
import queue
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("native_consumer_update_setup", ROOT / "tests/native_consumer_install.py")
consumer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(consumer)
lifecycle = consumer.lifecycle
require, read_json = consumer.require, consumer.read_json
API = "/api/v1/companion/update"
MAX_FRAME_BYTES = 32768


class PrivateCapture:
    """Redaction precedes retained truncation; overflow drops all raw capture."""
    def __init__(self):
        self.data = bytearray()
        self.overflow = False

    def append(self, data):
        if self.overflow:
            return
        if len(self.data) + len(data) > 1024 * 1024:
            self.data.clear()
            self.overflow = True
            return
        self.data.extend(data)

    def value(self):
        return b"[controller capture exceeded its private bound; raw output omitted]" if self.overflow else bytes(self.data)


def public_observation(value):
    """Keep process generation evidence as hashes, not private protocol values."""
    keys = ["status", "service_name", "manager", "profile_matches", "active", "transitioning",
            "enabled", "masked", "definition_matches", "definition_sha256", "effective_definition_sha256",
            "state_sha256", "registered", "startup_settings_sha256", "previous_process_alive", "launch_receipt",
            "application_manager_binding"]
    result = {key: value[key] for key in keys if key in value}
    result["processes"] = [{"pid": row["identity"]["pid"], "identity_sha256": fingerprint(row["identity"]),
                            "instance_guid_sha256": row.get("instance_guid_sha256")}
                           for row in value.get("processes", [])]
    return result


class NativeObserver:
    def __init__(self, install, controller: Path, *, managed: bool):
        require(controller.is_absolute() and controller.is_file() and os.access(controller, os.X_OK),
                "Native observer requires an explicit executable controller")
        self.install, self.controller, self.managed = install, controller, managed
        self.baseline_startup = None
        self.phase = None

    def call(self, target, *, action="inspect", previous=None):
        require(action in {"inspect", "observe_retirement", "stop"}, "Native controller action is not fixed")
        payload = {"home": str(self.install.home.resolve()), "action": action, "target": target,
                   "previous_process": previous}
        child = subprocess.Popen([str(self.controller)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, cwd=self.install.root, env=self.install.env)
        try:
            stdout, _ = child.communicate(json.dumps(payload).encode(), timeout=45)
        except subprocess.TimeoutExpired:
            child.kill()
            try:
                child.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                self.install.unsettled_commands.append(child)
            if action == "stop":
                raise TimeoutError("Exact disposable Stop outcome is unknown; no retry or deletion permitted") from None
            return {"status": "unavailable", "code": "controller_observation_timeout"}
        require(len(stdout) <= 65536, "Native controller output exceeded its bound")
        value = json.loads(stdout)
        require(isinstance(value, dict), "Native controller returned invalid evidence")
        if action == "stop":
            require(child.returncode == 0 and value.get("dispatch", {}).get("status") == "accepted",
                    "Exact disposable Stop was refused or unknown; preserve its registration")
        elif child.returncode:
            require(value.get("status") == "unavailable", "Native observer failed without an uncertainty receipt")
        return value

    @staticmethod
    def preference(value):
        return {key: value.get(key) for key in ["registered", "service_name", "manager", "enabled", "masked",
                                               "definition_sha256", "effective_definition_sha256", "startup_settings_sha256"]}

    def begin(self, phase):
        require(phase in {"applied", "rolled_back"}, "Unknown observed update phase")
        current = self.install.await_running()
        original = self.call({"kind": "original"})
        require(original.get("status") in {"observed", "absent"}, "Original OS startup state is unobserved")
        require(original.get("registered") is self.managed, "Fixture parent topology differs from its declared manager state")
        identity = original.get("application_identity")
        require(isinstance(identity, dict) and identity.get("pid") == current["pid"], "Original application process generation is not observed")
        if self.managed:
            require(original.get("active") is True and original.get("profile_matches") is True
                    and original.get("application_manager_binding") in {"same_process", "descendant_of_task_engine"},
                    "Declared managed parent does not match its actual native process generation")
        preference = self.preference(original)
        if self.baseline_startup is None:
            self.baseline_startup = preference
        require(preference == self.baseline_startup, "Original startup preference changed before the next operation")
        ledger = self.install.home / "native-browser-update/control.json"
        previous = read_json(ledger).get("current_operation") if ledger.is_file() else None
        self.phase, self.old_identity, self.previous_operation = phase, identity, previous
        self.operation, self.attempt, self.transaction = None, None, None
        self.worker_generation = None
        self.survived_old_exit = False
        self.old_exit = False
        self.worker_observation = None
        self.observations = []

    def sample(self):
        ledger = self.install.home / "native-browser-update/control.json"
        if not ledger.is_file():
            return
        value = read_json(ledger)
        operation_id = value.get("current_operation")
        if not operation_id or operation_id == self.previous_operation:
            return
        operation = value["operations"][operation_id]
        attempt = operation["attempt"]["id"]
        transaction = operation["transaction_id"]
        if self.operation is None:
            self.operation, self.attempt, self.transaction = operation_id, attempt, transaction
        require((operation_id, attempt, transaction) == (self.operation, self.attempt, self.transaction),
                "Operation or launch attempt changed without a new controller mutation")
        worker = self.call({"kind": "worker", "operation": operation_id, "attempt": attempt}, previous=self.old_identity)
        if worker.get("status") != "observed":
            return
        if worker.get("profile_matches") is not True:
            require(worker.get("active") is not True, "Active worker registration does not match its saved fixed profile")
            return  # Marker publication legitimately precedes registration.
        self.worker_observation = worker
        old_gone = worker.get("previous_process_alive") is False
        self.old_exit = self.old_exit or old_gone
        if (worker.get("active") is True and worker.get("transitioning") is False
                and worker.get("processes") and operation["attempt"]["claimed"] is True):
            generation = fingerprint(worker["processes"])
            if self.worker_generation is None:
                self.worker_generation = generation
            require(generation == self.worker_generation, "A second worker process generation appeared for the same attempt")
            self.survived_old_exit = self.survived_old_exit or old_gone
        public = public_observation(worker)
        if not self.observations or public != self.observations[-1]:
            self.observations.append(public)
            require(len(self.observations) <= 64, "Too many worker state changes for one fixed attempt")

    def settled(self, operation, running):
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            self.sample()
            worker = self.worker_observation
            if worker and worker.get("active") is False and worker.get("transitioning") is False and worker.get("processes") == []:
                break
            time.sleep(0.1)
        else:
            raise TimeoutError("Actual worker exit is unobserved; no rollback or retirement permitted")
        require((operation["id"], operation["transaction_id"]) == (self.operation, self.transaction),
                "Settled browser receipt differs from the observed native operation")
        require(self.old_exit and self.survived_old_exit and self.worker_generation is not None,
                "Worker survival after old-parent exit was not actually observed")
        target = ({"kind": "original"} if self.managed else {"kind": "replacement", "transaction": self.transaction})
        replacement = self.call(target)
        require(replacement.get("status") == "observed" and replacement.get("profile_matches") is True
                and replacement.get("active") is True and replacement.get("transitioning") is False,
                "Exact replacement manager is not active")
        require(replacement["service_name"] != worker["service_name"], "Replacement shares its worker's manager identity")
        application = replacement.get("application_identity")
        require(isinstance(application, dict) and application["pid"] == running["pid"]
                and replacement.get("application_manager_binding") in {"same_process", "descendant_of_task_engine"},
                "Manager replacement process does not match the healthy application generation")
        time.sleep(2)
        after = self.call(target)
        require(after.get("active") is True and after.get("application_identity") == application
                and after.get("processes") == replacement["processes"],
                "Replacement did not survive completed worker exit")
        healthy = self.install.await_running()
        require(healthy["process_nonce"] == running["process_nonce"], "Replacement restarted while proving worker-independent survival")
        original = self.call({"kind": "original"})
        require(self.preference(original) == self.baseline_startup, "Original definition, enabled or mask state changed")
        return {"old_parent_exit_observed": True, "worker_survived_old_parent_exit": True,
                "worker_exit_observed": True, "replacement_survived_worker_exit": True,
                "startup_preference_preserved": True, "survival_observation_seconds": 2,
                "old_process_identity_sha256": fingerprint(self.old_identity),
                "worker": public_observation(worker), "replacement": public_observation(after),
                "original_startup": self.baseline_startup, "worker_observations": self.observations}


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def private_inputs(install):
    return [consumer.PASSWORD, consumer.FIXTURE_KEY, *(cookie.value for cookie in install.cookies)]


def protected_receipt(install):
    """Retain comparisons and hashes, never account/session/key database rows."""
    value = install.snapshot()
    require(value == install.baseline, "Update changed protected account/session/work/configuration state")
    return {"snapshot_sha256": fingerprint(value), "files": value["files"],
            "tables": {name: {"rows": len(rows), "sha256": fingerprint(rows)} for name, rows in value["tables"].items()},
            "companion_policy_sha256": fingerprint(value["companion_policy"])}


def seed_retained_work(install, conversation_id):
    """Historical settled records; no call, task, provider or uncertain effect runs."""
    with install.db() as database:
        database.execute("INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id) VALUES('retained-update-message',?,'assistant','Earlier completed work; do not replay.','complete',100,100,'fixture',?)", (conversation_id, consumer.AGENT_ID))
        database.execute("INSERT INTO companion_calls(id,owner,request_id,request_hash,agent_id,conversation_id,state,created_at,heartbeat_at,endpoint_hash) VALUES('retained-update-call','fixture-owner','retained-update-call','fixture-hash',?,?,'closed',100,100,'fixture-endpoint')", (consumer.AGENT_ID, conversation_id))
        database.execute("INSERT INTO companion_delegations(call_id,id,offset_ms,state,message_id,created_at) VALUES('retained-update-call','retained-update-delegation',0,'completed','retained-update-message',100)")
        database.execute("INSERT INTO companion_actions VALUES('retained-update-call','retained-update-action','retained-update-delegation','fixture-connection','Earlier settled work.','{}','completed','{\"state\":\"completed\"}',100)")
    install.baseline = install.snapshot()


def phase_checkpoint(install, bundle, phase, snapshot, observer):
    """Only independent OS observations may establish lifetime/cleanup facts."""
    operation = snapshot.get("operation") or {}
    require(operation.get("phase") == "completed" and operation.get("outcome") == phase,
            "Browser checkpoint does not identify the requested settled outcome")
    install.selected(bundle)
    protected = protected_receipt(install)
    require(install.browser_request("GET", "/auth/session")["authenticated"] is True,
            "Existing browser authentication was lost")
    running = install.await_running()
    require(running["id"] == read_json(install.home / "config.json")["id"], "Replacement changed installation identity")
    # This callback must prove old-parent exit, worker exit and surviving
    # replacement with actual manager observations. Markers alone cannot pass.
    lifetime = observer.settled(operation, running)
    require(lifetime.get("old_parent_exit_observed") is True
            and lifetime.get("worker_survived_old_parent_exit") is True
            and lifetime.get("worker_exit_observed") is True
            and lifetime.get("replacement_survived_worker_exit") is True
            and lifetime.get("startup_preference_preserved") is True,
            "Actual OS lifetime or original startup preference is unproved")
    require(install.snapshot() == install.baseline, "State changed while observing the settled process boundary")
    row = {"phase": phase, "operation_id": operation["id"], "transaction_id": operation["transaction_id"],
           "selected_manifest_sha256": install.status()["selected_manifest"], "protected": protected,
           "process": {"pid": running["pid"], "process_nonce_sha256": fingerprint(running["process_nonce"]),
                       "binary_sha256": running["binary_sha256"]}, "lifetime": lifetime}
    install.result.setdefault("phase_snapshots", []).append(row)
    install.checkpoint()
    return row


def await_operation(install, request_id, outcome, observer, *, timeout=360):
    """Read the retained request only; a lost response never repeats mutation."""
    deadline, transaction = time.monotonic() + timeout, None
    while time.monotonic() < deadline:
        observer.sample()
        value = request_status(install, request_id, tolerate_unavailable=True)
        if value is None:
            time.sleep(0.1)
            continue
        request = value.get("request") or {}
        if request.get("state") == "recorded":
            operation = value.get("operation") or {}
            require(request.get("id") == request_id and operation.get("id") == request.get("operation_id"),
                    "Retained request mapped to a different operation")
            current = operation.get("transaction_id")
            require(isinstance(current, str) and (transaction is None or transaction == current),
                    "Read-only reconciliation changed the original transaction")
            transaction = current
            if operation.get("phase") == "completed":
                require(operation.get("outcome") == outcome, "Native operation settled with another outcome")
                return value
            require(operation.get("phase") not in {"failed", "deferred"}, "Native operation did not complete its requested change")
        time.sleep(0.1)
    raise TimeoutError("Retained update request did not settle within its allowance; no mutation was retried")


def request_status(install, request_id=None, *, tolerate_unavailable=False):
    origin = read_json(install.home / "config.json")["public_url"]
    suffix = "?request_id=" + request_id if request_id is not None else ""
    request = urllib.request.Request(origin + API + suffix, headers={"Origin": origin}, method="GET")
    try:
        try:
            response = install.browser.open(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            if tolerate_unavailable and 500 <= response.status < 600:
                return None
            require(response.status == 200, f"Retained update status returned HTTP {response.status}")
            body = response.read(MAX_FRAME_BYTES + 1)
            require(len(body) <= MAX_FRAME_BYTES, "Retained update status exceeded its bound")
            result = json.loads(body)
            require(not any(secret in json.dumps(result) for secret in private_inputs(install)),
                    "Retained update status exposed private input")
            return result
    except (OSError, urllib.error.URLError, http.client.HTTPException):
        if tolerate_unavailable:
            return None
        raise


def submit_once(install, action, payload):
    """One POST; transport/5xx ambiguity is resolved by original-UUID GETs."""
    require(action in {"apply", "recover"}, "Controller mutation is not a fixed update action")
    origin = read_json(install.home / "config.json")["public_url"]
    row = {"action": action, "request_id": payload["request_id"], "response": "pending", "mutation_retries": 0}
    install.result.setdefault("request_dispatch", []).append(row)
    install.checkpoint()
    headers = {"Origin": origin, "Content-Type": "application/json", "X-CSRF-Token": install.csrf}
    request = urllib.request.Request(origin + API + "/" + action, data=json.dumps(payload).encode(), headers=headers, method="POST")
    try:
        try:
            response = install.browser.open(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            row["http_status"] = response.status
            if 500 <= response.status < 600:
                row["response"] = "server_error_unknown"
                return
            if not 200 <= response.status < 300:
                row["response"] = "refused"
            require(200 <= response.status < 300,
                    f"Update {action} was deterministically refused with HTTP {response.status}")
            # Fully drain a bounded reply, but deliberately do not use it to
            # choose the next transaction or infer durable completion.
            body = response.read(MAX_FRAME_BYTES + 1)
            require(len(body) <= MAX_FRAME_BYTES, "Update acknowledgement exceeded its bound")
            row["response"] = "received_and_discarded"
    except (OSError, urllib.error.URLError, http.client.HTTPException):
        row["response"] = "transport_unknown"
    finally:
        install.checkpoint()


def exercise_api(install, a, b, observer):
    before = request_status(install)
    require(before["owner"] == "standalone" and before["mode"] == "normal" and before["actions"]["check"],
            "Disposable installation cannot check a standalone candidate")
    checked = install.browser_request("POST", API + "/check", {"package_path": str(b)})
    require(checked["check"]["available"] and checked["actions"]["apply"], "Actual complete bundle is not available for apply")
    request_id = str(uuid.uuid4())
    observer.begin("applied")
    # Deliberately discard the successful reply. Reconciliation uses only the
    # original request ID; no second POST is made or inferred from a timeout.
    submit_once(install, "apply", {"request_id": request_id, "check_id": checked["check"]["id"]})
    applied = await_operation(install, request_id, "applied", observer)
    phase_checkpoint(install, b, "applied", applied, observer)
    reloaded = request_status(install, request_id)
    require(reloaded["operation"] == applied["operation"], "Read-only reload changed the completed apply receipt")
    require(reloaded["actions"]["rollback"], "Completed apply does not offer explicit rollback")
    rollback_id = str(uuid.uuid4())
    observer.begin("rolled_back")
    submit_once(install, "recover", {"request_id": rollback_id,
        "operation_id": applied["operation"]["id"], "action": "rollback"})
    restored = await_operation(install, rollback_id, "rolled_back", observer)
    require(restored["operation"]["transaction_id"] != applied["operation"]["transaction_id"],
            "Explicit rollback reused the completed apply transaction")
    phase_checkpoint(install, a, "rolled_back", restored, observer)
    install.check("real authenticated API check/apply, discarded reply reconciliation and fresh rollback preserve original state",
                  apply_request_id=request_id, rollback_request_id=rollback_id, mutation_count=2)
    return {"status": "passed", "before": before, "checked": checked, "applied": applied, "restored": restored,
            "mutation_count": 2, "request_retries": 0}


def write_frame(stream, value):
    data = (json.dumps(value, ensure_ascii=False) + "\n").encode()
    require(len(data) <= MAX_FRAME_BYTES, "Private controller frame exceeded its bound")
    stream.write(data)
    stream.flush()


def read_frames(stream, events, capture, *, frame_limit=MAX_FRAME_BYTES):
    """Bound the line before JSON decoding, and bound retained diagnostics."""
    try:
        while True:
            line = stream.readline(frame_limit + 1)
            if not line:
                events.put(("eof", None))
                return
            if len(line) > frame_limit or not line.endswith(b"\n"):
                events.put(("error", "Browser checkpoint frame exceeded its newline bound"))
                return
            capture.append(line)
            try:
                value = json.loads(line)
            except (ValueError, UnicodeError):
                events.put(("error", "Browser checkpoint frame is not valid JSON"))
                return
            events.put(("frame", value))
    except OSError:
        events.put(("error", "Browser checkpoint stream became unavailable"))


def drain_bounded(stream, capture):
    try:
        while data := stream.read(4096):
            capture.append(data)
    except OSError:
        return


def exercise_browser(install, a, b, observer, script, node, artifacts):
    require(node.is_absolute() and node.is_file() and os.access(node, os.X_OK),
            "Browser update controller requires an explicit absolute executable Node")
    cookies = [{"name": cookie.name, "value": cookie.value} for cookie in install.cookies]
    require(cookies, "Browser update must reuse an existing authenticated session")
    artifacts.mkdir(parents=True, exist_ok=True)
    child = subprocess.Popen([str(node), str(script)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, cwd=ROOT)
    events, stdout, stderr = queue.Queue(maxsize=8), PrivateCapture(), PrivateCapture()
    readers = [threading.Thread(target=read_frames, args=(child.stdout, events, stdout), daemon=True),
               threading.Thread(target=drain_bounded, args=(child.stderr, stderr), daemon=True)]
    for reader in readers:
        reader.start()
    secret_values, phases, result, failure = private_inputs(install), [], None, None
    deadline = time.monotonic() + 900
    try:
        observer.begin("applied")
        write_frame(child.stdin, {"type": "init", "url": read_json(install.home / "config.json")["public_url"],
            "candidate": str(b), "cookies": cookies, "password": consumer.PASSWORD, "artifact_dir": str(artifacts)})
        while time.monotonic() < deadline:
            observer.sample()
            try:
                kind, frame = events.get(timeout=0.1)
            except queue.Empty:
                continue
            require(kind != "error", frame if kind == "error" else "Invalid browser frame")
            if kind == "eof":
                break
            require(isinstance(frame, dict), "Browser checkpoint frame must be an object")
            serialized = json.dumps(frame)
            require(not any(secret in serialized for secret in secret_values), "Browser checkpoint exposed private input")
            if frame.get("type") == "checkpoint":
                phase = frame.get("phase")
                require(phase == ("applied" if not phases else "rolled_back") and len(phases) < 2,
                        "Browser checkpoint order or count changed")
                phase_checkpoint(install, b if phase == "applied" else a, phase, frame["snapshot"], observer)
                phases.append(phase)
                if phase == "applied":
                    observer.begin("rolled_back")
                write_frame(child.stdin, {"type": "ack", "phase": phase,
                    "transaction_id": frame["snapshot"]["operation"]["transaction_id"]})
            elif frame.get("type") == "result":
                require(result is None and phases == ["applied", "rolled_back"], "Browser result lacks exact phase checkpoints")
                require(frame.get("status") == "passed" and frame.get("media_calls") == 0
                        and frame.get("provider_calls") == 0 and frame.get("page_errors") == [],
                        "Browser update qualification did not pass")
                result = frame
            else:
                raise AssertionError("Unexpected browser controller frame")
        if result is None and time.monotonic() >= deadline:
            raise TimeoutError("Browser update controller exceeded its allowance; original transactions are retained")
        require(result is not None, "Browser update ended without a passing result")
        child.wait(timeout=10)
        require(child.returncode == 0, "Browser update driver failed")
        return result
    except Exception as error:
        failure = error
        raise
    finally:
        if child.poll() is None:
            child.kill()  # Only the exact controller child; never a service/worker.
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                install.unsettled_commands.append(child)
        for reader in readers:
            reader.join(timeout=1)
        consumer.retain_browser_output(artifacts, stdout.value(), stderr.value(), secret_values,
                                       returncode=child.returncode, timed_out=isinstance(failure, TimeoutError))
        for stream in [child.stdin, child.stdout, child.stderr]:
            stream.close()


class UpdateInstallation(consumer.ConsumerInstallation):
    def __init__(self, *args, outer_result, **kwargs):
        self.outer_result = outer_result
        super().__init__(*args, **kwargs)

    def checkpoint(self):
        lifecycle.persist_checkpoint(self.checkpoint_output, self.outer_result)


def startup_module():
    spec = importlib.util.spec_from_file_location("consumer_update_startup", ROOT / "scripts/test-native-startup.py")
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def manager_environment(install, startup):
    controller_environment = startup.startup_environment(install.root)
    controller_environment["CONSTELLATION_NATIVE_STARTUP_TEST"] = "1"
    # OS registration belongs to the actual CI user. The product still receives
    # the empty PATH created by ConsumerInstallation; new fixed launch APIs use
    # absolute OS tools/native COM. Legacy startup install/remove alone receive
    # the old startup controller's fixed systemctl allowance.
    install.env = {**controller_environment, "PATH": install.env["PATH"]}
    return controller_environment


def managed_parent(install, prepared, startup, controller_environment):
    config = read_json(install.home / "config.json")
    registration = startup.Startup(install.home, "agent-conversations-" + config["id"],
                                   install.home / "bin" / install.worker.name, controller_environment)
    registration.preflight()
    install.result["original_registration_attempted"] = True
    install.checkpoint()
    install.command(["startup", "install"], extra_env={"PATH": controller_environment["PATH"]})
    first = registration.inspect()
    child = prepared["original_child"]
    require(child.poll() is None, "Prepared foreground parent unexpectedly exited")
    child.terminate()  # Exact controller-owned setup child, before any update.
    child.wait(timeout=12)
    install.await_stopped()
    registration.start()
    running = install.await_running()
    state = registration.await_active(running["pid"])
    require(state["definition"] == first, "Original manager definition changed while starting the fixture")
    require((install.home / "config.json").read_bytes() == prepared["config_bytes"]
            and (install.home / "secrets/local-password.json").read_bytes() == prepared["account_bytes"],
            "Manager startup changed the prepared local account/configuration")
    install.result["original_manager"] = state
    install.check("actual native manager starts the original prepared consumer application")
    return registration


def retained_operations(home):
    value = read_json(home / "native-browser-update/control.json")
    ledger = {key: value[key] for key in ["schema", "installation_id", "current_check", "current_operation",
                                         "checks", "operations", "requests"]}
    history, count, size = {}, 0, 0
    root = home / "native-browser-update/history"
    if root.exists():
        require(root.is_dir() and not root.is_symlink(), "Retained operation history changed type")
        for category in root.iterdir():
            require(category.name in {"operations", "requests", "checks", "transactions"}
                    and category.is_dir() and not category.is_symlink(), "Unexpected retained history category")
            for path in category.iterdir():
                require(path.suffix == ".json" and str(uuid.UUID(path.stem)) == path.stem
                        and path.is_file() and not path.is_symlink(), "Retained history identity changed type")
                count, size = count + 1, size + path.stat().st_size
                require(count <= 64 and size <= 16 * 1024 * 1024, "Disposable history exceeded its expected evidence bound")
                history[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
    return {"ledger": ledger, "immutable_history": history}


def launch_keys(home):
    expected = set()
    for path in home.glob("native-browser-launch-*.json"):
        require(path.is_file() and not path.is_symlink(), "Owned launch marker changed type")
        prepared = read_json(path)["prepared"]
        require(Path(prepared["home"]).samefile(home), "Launch marker belongs to another home")
        identity = prepared["identity"]
        if identity["kind"] == "worker":
            key = ("worker", identity["operation"], identity["attempt"])
        else:
            require(identity["kind"] == "replacement", "Unknown owned launch role")
            key = ("replacement", identity["transaction"])
        require(key not in expected, "Duplicate launch markers identify one attempt")
        expected.add(key)
    require(expected, "Actual updater did not retain any launch identity")
    return expected


def cleanup_case(install, observer, registration, controller_environment):
    """Separate post-proof teardown; it cannot manufacture update success."""
    require(len(install.result.get("phase_snapshots", [])) == 2, "Cleanup cannot precede both settled lifetime proofs")
    require(all(child.poll() is not None for child in install.unsettled_commands), "Unknown controller command remains unsettled")
    ledger_before = retained_operations(install.home)
    expected = launch_keys(install.home)
    cleanup = {"id": str(uuid.uuid4()), "scope": "post-proof disposable teardown", "status": "running",
               "operation_ledger_before_sha256": fingerprint(ledger_before)}
    install.result["cleanup"] = cleanup
    install.checkpoint()
    target = ({"kind": "original"} if observer.managed else
              {"kind": "replacement", "transaction": install.result["phase_snapshots"][-1]["transaction_id"]})
    stopped = observer.call(target, action="stop")
    cleanup["exact_stop"] = {"dispatch": stopped["dispatch"], "before": public_observation(stopped)}
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        state = observer.call(target)
        if state.get("status") == "observed" and state.get("active") is False and state.get("transitioning") is False and state.get("processes") == []:
            break
        time.sleep(0.1)
    else:
        raise TimeoutError("Exact fixture manager Stop remains unobserved; no cleanup retry")
    install.await_stopped()
    require(retained_operations(install.home) == ledger_before, "Teardown Stop changed the native operation ledger")
    maintenance, _ = install.start()
    cleanup["maintenance"] = "separate controller-owned foreground process, not updater lifetime proof"
    protected_receipt(install)
    reply = install.browser_request("POST", API + "/cleanup", {})
    require(isinstance(reply, dict) and isinstance(reply.get("cleanup"), dict), "Checked cleanup returned no receipt")
    receipt = reply["cleanup"]
    cleanup["checked_retirement"] = receipt
    install.checkpoint()
    require(receipt.get("complete") is True, "Checked retirement refused or remains unknown; retain exact fixture and registrations")
    outcomes = receipt.get("outcomes")
    require(isinstance(outcomes, list) and len(outcomes) <= 256, "Checked cleanup outcome list is invalid")
    observed = set()
    for outcome in outcomes:
        require(outcome.get("state") == "retired" and outcome.get("absent") is True,
                "Checked cleanup did not confirm exact native registration absence")
        if outcome.get("kind") == "worker":
            key = ("worker", outcome["operation_id"], outcome["attempt_id"])
        else:
            require(outcome.get("kind") == "replacement", "Checked cleanup returned an unexpected role")
            key = ("replacement", outcome["transaction_id"])
        require(key not in observed, "Checked cleanup duplicated an outcome")
        observed.add(key)
    require(observed <= expected, "Checked cleanup added an unrelated launch registration")
    # Automatic maintenance may already have retired an earlier launch and
    # removed it from the outstanding index. Its original marker remains, so
    # independently reconcile every fixture-owned identity without deleting or
    # retrying any retirement. The native wrapper proves manager AND definition
    # absence, including the distinction from an unloaded macOS plist.
    absence = []
    for key in sorted(expected):
        target = ({"kind": "worker", "operation": key[1], "attempt": key[2]} if key[0] == "worker"
                  else {"kind": "replacement", "transaction": key[1]})
        observation = observer.call(target, action="observe_retirement")
        require(observation.get("status") == "retirement_observed" and observation.get("retired") is True,
                "A fixture-owned launch lacks confirmed manager and definition absence")
        absence.append({"identity": target, "observation": observation})
    cleanup["all_owned_launches_absent"] = absence
    require(retained_operations(install.home) == ledger_before, "Checked cleanup changed retained operation/request evidence")
    protected = protected_receipt(install)
    require(maintenance.poll() is None, "Foreground cleanup process exited unexpectedly")
    maintenance.terminate()
    maintenance.wait(timeout=12)
    install.await_stopped()
    if registration is not None:
        original = observer.call({"kind": "original"})
        require(observer.preference(original) == observer.baseline_startup and original.get("active") is False,
                "Original startup registration changed before its separate removal")
        registration.inspect()
        install.command(["startup", "remove"], extra_env={"PATH": controller_environment["PATH"]})
        require(not registration.exists(), "Exact original startup registration remains")
        cleanup["original_startup"] = "exact inactive fixture registration removed separately"
    require(all(child.poll() is not None for child in install.children), "A controller-owned child remains alive")
    for port in install.ports:
        with socket.socket() as connection:
            connection.settimeout(0.2)
            require(connection.connect_ex(("127.0.0.1", port)) != 0, "Owned fixture listener remains open")
    for stream in install.streams:
        stream.close()
    cleanup.update(status="passed", protected=protected, service_lock_released=True,
                   owned_listeners_closed=True, controller_children_reaped=True,
                   checked_one_shot_retirement_confirmed=True,
                   operation_ledger_after_sha256=fingerprint(retained_operations(install.home)))
    install.checkpoint()


def qualify(archive: Path, receipt: Path, output: Path, controller: Path, *, browser_test=None, browser_node=None, browser_artifacts=None):
    require(os.environ.get("CONSTELLATION_NATIVE_STARTUP_TEST") == "1", "Explicit disposable-current-user manager opt-in required")
    require(controller.is_absolute() and controller.is_file() and os.access(controller, os.X_OK), "Native fixture requires an executable controller")
    if browser_test is not None:
        require(platform.system() == "Linux", "Actual browser controller requires the Linux qualification host")
        require(browser_node is not None and browser_artifacts is not None and browser_node.is_absolute()
                and browser_node.is_file() and os.access(browser_node, os.X_OK), "Actual browser requires explicit controller Node and artifact paths")
    provenance = read_json(receipt)
    package = lifecycle.clean.package_module()
    manifest, contents = package.inspect_archive(archive, provenance["archive_sha256"])
    require(manifest["target"] == lifecycle.clean.native_target(), "Consumer update package target differs from actual host")
    require(lifecycle.digest(contents["manifest.json"]) == provenance["manifest_sha256"], "Consumer update manifest receipt differs")
    root = Path(tempfile.mkdtemp(prefix="Companion update café ")).resolve()
    result = {"schema": 1, "status": "running", "target": manifest["target"], "source_revision": manifest["source_revision"],
              "archive_sha256": provenance["archive_sha256"], "manifest_sha256": provenance["manifest_sha256"],
              "controller_sha256": hashlib.sha256(controller.read_bytes()).hexdigest(), "cases": {},
              "scope": "actual current-user manager parent/worker/replacement qualification on disposable installations",
              "model_requests": 0, "provider_requests": 0, "voice_calls": 0, "speech_release": "unchanged_paused",
              "limitations": ["Manager-bound fixture preserves actual user HOME and manager bus; separate clean-PATH/empty-HOME receipt applies",
                  "A/B use identical tested executable/runtime bytes; B changes bundle version/support document",
                  "No production installation, provider account, OS password sign-in, physical media or signing qualification",
                  "Foreground maintenance is post-proof teardown only; it is not updater lifetime proof"]}
    installations = []
    try:
        a, b = root / "Release A", root / "Release B"
        lifecycle.extract(contents, manifest, a)
        lifecycle.derive_bundle(a, b, "browser-update-b")
        startup = startup_module()
        scenarios = [("managed_api", True)] + ([("manual_browser", False)] if browser_test is not None else [])
        for name, managed in scenarios:
            case_root = root / name
            case_root.mkdir()
            case = {"status": "running", "checks": [], "topology": "manager-parent" if managed else "manual-parent"}
            result["cases"][name] = case
            install = UpdateInstallation(case_root, a, manifest, case, outer_result=result, checkpoint_output=output)
            installations.append((name, install))
            controller_environment = manager_environment(install, startup)
            prepared = consumer.prepare_consumer_installation(install)
            seed_retained_work(install, prepared["conversation_id"])
            registration = managed_parent(install, prepared, startup, controller_environment) if managed else None
            observer = NativeObserver(install, controller, managed=managed)
            case["baseline"] = protected_receipt(install)
            if managed:
                case["api"] = exercise_api(install, a, b, observer)
            else:
                case["browser"] = exercise_browser(install, a, b, observer, browser_test, browser_node, browser_artifacts)
            require([json.loads(line)["method"] for line in prepared["fixture_log"].read_text().splitlines()] == ["initialize"],
                    "Update redispatched ACP work")
            cleanup_case(install, observer, registration, controller_environment)
            case["status"] = "passed"
            install.checkpoint()
        shutil.rmtree(root)
        result.update(status="passed", temporary_installations_removed=True)
    except Exception as error:
        result.update(status="failed", error_type=type(error).__name__, error=str(error), retained_test_directory=str(root),
                      cleanup_policy="Failed or uncertain fixture/registrations retained; no retry or raw removal")
        result["failure_installation_snapshots"] = lifecycle.capture_failure_snapshots(installations)
        lifecycle.persist_checkpoint(output, result)
        raise
    lifecycle.persist_checkpoint(output, result)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["archive", "receipt", "output", "controller"]:
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ["browser-test", "browser-node", "browser-artifacts"]:
        parser.add_argument("--" + name, type=Path)
    args = parser.parse_args()
    result = qualify(args.archive.resolve(), args.receipt.resolve(), args.output.resolve(), args.controller.resolve(),
                     browser_test=args.browser_test, browser_node=args.browser_node, browser_artifacts=args.browser_artifacts)
    print(json.dumps({"status": result["status"], "cases": list(result["cases"]),
                      "temporary_installations_removed": result.get("temporary_installations_removed")}))
