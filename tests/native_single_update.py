#!/usr/bin/env python3
"""One actual previous-package -> candidate -> previous roundtrip.

The candidate's ordinary desktop entry opens its restricted update assistant
against an existing managed disposable home. Existing native lifecycle helpers
own manager observations, preservation checkpoints and checked retirement.
Browser and API-only cases are distinct; neither sends the old CHECK sequence.
"""
from __future__ import annotations

import argparse
import errno
import http.client
import http.cookiejar
import importlib.util
import json
import os
from pathlib import Path, PureWindowsPath
import queue
import re
import select
import shutil
import socket
import stat
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("single_update_shared", ROOT / "tests/native_consumer_update.py")
shared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shared)
consumer, lifecycle = shared.consumer, shared.lifecycle
require, read_json, fingerprint = shared.require, shared.read_json, shared.fingerprint
API = shared.API
ACK_TARGET_SECONDS, REQUEST_SECONDS, BACKGROUND_SECONDS = 5, 40, 360


def ordinary_identity(path, *, directory=False):
    value = path.lstat()
    predicate = stat.S_ISDIR if directory else stat.S_ISREG
    require(predicate(value.st_mode) and not getattr(value, "st_file_attributes", 0) & 0x400,
            "Fixture path is not an ordinary directory" if directory else "Fixture path is not an ordinary file")
    return value.st_dev, value.st_ino, stat.S_IFMT(value.st_mode)


def artifact_identity(path):
    require(path.is_absolute() and ".." not in path.parts, "Browser artifacts require an absolute unambiguous path")
    return tuple(ordinary_identity(parent, directory=True) for parent in [path, *path.parents])


def fresh_browser_artifacts(path):
    """Reserve new captures before any installation or manager activity."""
    require(path.is_absolute() and ".." not in path.parts, "Browser artifacts require an absolute unambiguous path")
    parents = tuple(ordinary_identity(parent, directory=True) for parent in path.parents)
    path.mkdir(mode=0o700)  # Exclusive: an existing file, directory or link refuses.
    bound = artifact_identity(path)
    require(bound[1:] == parents and not any(path.iterdir()), "Browser artifact parent changed during creation")
    return bound


def assistant_lock_available(home, expected):
    """Use the lifecycle fixture's flock/msvcrt exclusion on this existing lock.

    No file is created, rewritten or repaired. Path and opened-handle identity
    bracket acquisition so a replaced lock cannot manufacture release proof.
    """
    path = home / "native-update-assistant.lock"
    require(ordinary_identity(path) == expected, "Assistant lock identity changed")
    with path.open("r+b") as stream:
        def opened_identity():
            value = os.fstat(stream.fileno())
            require(stat.S_ISREG(value.st_mode) and not getattr(value, "st_file_attributes", 0) & 0x400,
                    "Opened assistant lock is not an ordinary file")
            return value.st_dev, value.st_ino, stat.S_IFMT(value.st_mode)
        require(opened_identity() == expected, "Assistant lock changed before acquisition")
        if os.name == "nt":
            import msvcrt
            acquire = lambda: msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            release = lambda: msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
        else:
            import fcntl
            acquire = lambda: fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            release = lambda: fcntl.flock(stream, fcntl.LOCK_UN)
        try:
            acquire()
        except OSError:
            free = False  # Never promote an acquisition error to release.
        else:
            release()  # A failed unlock is uncertainty, not successful cleanup.
            free = True
        require(opened_identity() == expected and ordinary_identity(path) == expected,
                "Assistant lock changed during acquisition")
    require(ordinary_identity(path) == expected, "Assistant lock changed after acquisition")
    return free


def version(value):
    require(isinstance(value, str) and re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", value),
            "Single Update requires explicit ordinary release versions")
    return tuple(map(int, value.split(".")))


def assistant_receipt(install):
    directory = install.home / "native-browser-update"
    raw = shared._check_observation_bytes(directory / "assistant-control.json", parents=(install.home, directory))
    return json.loads(raw), lifecycle.digest(raw)


def launch_assistant(install, candidate, manifest):
    """One ordinary launch; only the exact child and its fixed receipt are used."""
    executable = (candidate / manifest["entrypoints"]["service"]).resolve()
    require(not (install.home / "native-browser-update/assistant-control.json").exists(),
            "Fresh disposable fixture already has an assistant receipt")
    before = install.await_running()
    row = {"entrypoint": "candidate --home existing-home --no-browser", "status": "starting", "attempts": 1}
    install.result["assistant"] = row
    install.checkpoint()
    stream = (install.root / "owned-update-assistant.log").open("w", encoding="utf-8")
    install.streams.append(stream)
    child = subprocess.Popen([str(executable), "--home", str(install.home), "--no-browser"],
                             env=install.env, cwd=install.root, stdin=subprocess.DEVNULL, stdout=stream, stderr=stream)
    install.children.append(child)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        require(child.poll() is None, "Candidate launcher exited before assistant readiness")
        if (install.home / "native-browser-update/assistant-control.json").exists():
            saved, digest = assistant_receipt(install)
            break
        time.sleep(0.1)
    else:
        raise TimeoutError("Candidate update assistant did not publish readiness within 60 seconds")
    require(saved.get("schema") == 1 and saved.get("installation_id") == before["id"]
            and saved.get("generation", {}).get("pid") == child.pid
            and Path(saved["candidate"]).samefile(executable) and saved["candidate_version"] == manifest["version"]
            and type(saved.get("port")) is int and 0 < saved["port"] < 65536
            and str(uuid.UUID(saved["offer_id"])) == saved["offer_id"], "Assistant receipt is not bound to this candidate/installation/process")
    require(saved["port"] != read_json(install.home / "config.json")["port"], "Assistant reused the installed service listener")
    after = install.await_running()
    require(after["pid"] == before["pid"] and after["process_nonce"] == before["process_nonce"],
            "Opening the candidate changed the running installed application")
    shared.protected_receipt(install)
    install.assistant_lock_identity = ordinary_identity(install.home / "native-update-assistant.lock")
    install.assistant_home_identity = ordinary_identity(install.home, directory=True)
    row.update(status="ready", pid=child.pid, generation_sha256=fingerprint(saved["generation"]), receipt_sha256=digest,
               candidate_sha256=lifecycle.digest(executable.read_bytes()), offer_id=saved["offer_id"], port=saved["port"],
               lock_identity_sha256=fingerprint(install.assistant_lock_identity), installed_application_unchanged=True)
    install.checkpoint()
    return child, saved


def authenticated_checkpoint(install):
    """A normal assistant login may add one session, not reset other state."""
    old, current = install.baseline, install.snapshot()
    old_other = {**old, "tables": {k: v for k, v in old["tables"].items() if k != "browser_sessions"}}
    new_other = {**current, "tables": {k: v for k, v in current["tables"].items() if k != "browser_sessions"}}
    require(old_other == new_other, "Assistant login changed protected installation or accepted-work data")
    before, after = old["tables"]["browser_sessions"], current["tables"]["browser_sessions"]
    require(len(after) == len(before) + 1 and all(row in after for row in before),
            "Assistant login did not preserve the original sessions and create exactly one session")
    install.result["assistant_login"] = {"original_sessions_retained": True, "new_sessions": 1,
        "before_sha256": fingerprint(old), "after_sha256": fingerprint(current)}
    install.baseline = current
    install.checkpoint()


class AssistantSession:
    def __init__(self, install, receipt):
        self.install, self.receipt = install, receipt
        self.origin = f"http://127.0.0.1:{receipt['port']}"
        self.cookies = http.cookiejar.CookieJar()
        self.client = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(self.cookies), consumer.NoRedirect())
        self.csrf = None

    def request(self, method, path, body=None, *, timeout=15, expected=200, tolerate_unavailable=False):
        headers = {"Origin": self.origin}
        if self.csrf:
            headers["X-CSRF-Token"] = self.csrf
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        request = urllib.request.Request(self.origin + path, data=data, headers=headers, method=method)
        try:
            response = self.client.open(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            response = error
        except (OSError, urllib.error.URLError, http.client.HTTPException):
            if method == "GET" and tolerate_unavailable:
                return None
            raise
        with response:
            raw = response.read(shared.MAX_FRAME_BYTES + 1)
            require(len(raw) <= shared.MAX_FRAME_BYTES, "Assistant reply exceeds its bound")
            if method == "GET" and tolerate_unavailable and 500 <= response.status < 600:
                return None
            require(response.status == expected, "Assistant reply has unexpected status")
            value = json.loads(raw)
        require(consumer.PASSWORD not in json.dumps(value) and consumer.FIXTURE_KEY not in json.dumps(value),
                "Assistant response exposed a private input")
        return value

    def login(self):
        value = self.request("POST", "/auth/login", {"username": "owner", "password": consumer.PASSWORD})
        require(value.get("authenticated") is True and value.get("auth_mode") == "local_password", "Assistant login failed")
        self.csrf = value["csrf"]

    def status(self, request_id=None, *, tolerate_unavailable=False):
        query = {"offer_id": self.receipt["offer_id"]}
        if request_id:
            query["request_id"] = request_id
        return self.request("GET", API + "?" + urllib.parse.urlencode(query), tolerate_unavailable=tolerate_unavailable)

    def submit_once(self, action, payload, *, quick=False):
        require(action in {"request", "recover"}, "Unexpected assistant mutation")
        row = {"action": action, "request_id": payload["request_id"], "response": "pending", "mutation_retries": 0}
        self.install.result.setdefault("request_dispatch", []).append(row)
        self.install.checkpoint()
        started = time.monotonic()
        try:
            self.request("POST", API + "/" + action, payload, timeout=REQUEST_SECONDS,
                         expected=202 if action == "request" else 200)
            row.update(response="received_and_discarded", elapsed_seconds=time.monotonic()-started)
            if quick:
                row["target_seconds"] = ACK_TARGET_SECONDS
                require(row["elapsed_seconds"] < ACK_TARGET_SECONDS, "Durable Update acknowledgement exceeded its separate five-second target")
        except (OSError, urllib.error.URLError, http.client.HTTPException):
            row.update(response="transport_unknown", elapsed_seconds=time.monotonic()-started)
            raise TimeoutError("Assistant mutation reply is unknown; no retry or cleanup authority") from None
        finally:
            self.install.checkpoint()


def close_assistant(session):
    status = session.request("GET", API + "/assistant-close")
    require(status.get("can_close") is True and status.get("identity") == session.receipt,
            "Assistant close is unavailable or bound to another listener")
    row = {"state": "intent_recorded", "attempts": 1}
    session.install.result["assistant_close"] = row
    session.install.checkpoint()
    try:
        reply = session.request("POST", API + "/assistant-close", {"listener_nonce": session.receipt["nonce"]}, timeout=REQUEST_SECONDS)
        require(reply.get("closing") is True and reply.get("listener_nonce") == session.receipt["nonce"], "Assistant close reply identity differs")
        row["state"] = "reply_received"
    except (OSError, urllib.error.URLError, http.client.HTTPException):
        row["state"] = "transport_unknown"
    finally:
        session.install.checkpoint()


def await_update(session, request_id, outcome, observer):
    deadline, transaction, child, progress = time.monotonic()+BACKGROUND_SECONDS, None, None, []
    while time.monotonic() < deadline:
        observer.sample()
        value = session.status(request_id, tolerate_unavailable=True)
        if value is None:
            time.sleep(0.1)
            continue
        update, operation = value.get("update"), value.get("operation")
        if outcome == "applied":
            require(isinstance(update, dict) and update.get("id") == request_id, "Saved Update parent changed")
            row = {key: update.get(key) for key in ["id", "revision", "state", "phase", "operation_id"]}
            if (not progress or progress[-1] != row) and len(progress) < 64:
                progress.append(row)
            session.install.result["flow_progress"] = progress
        if operation:
            if child is None:
                child, transaction = operation["id"], operation["transaction_id"]
            require((operation["id"], operation["transaction_id"]) == (child, transaction), "Native child identity changed within one request")
            if outcome == "applied":
                require(update.get("operation_id") == child, "Parent does not bind its native child")
            else:
                require(operation.get("request_id") == request_id, "Rollback reply names another request")
            if operation.get("phase") == "completed":
                require(operation.get("outcome") == outcome, "Native child completed with another outcome")
                require(time.monotonic() <= deadline, "Native completion was observed after the existing background allowance")
                return value
            require(operation.get("phase") not in {"failed", "deferred"}, "Native child did not complete requested change")
        time.sleep(0.1)
    raise TimeoutError("Single Update did not settle within the existing 360-second background allowance")


def checkpoint(install, a, b, observer, phase, snapshot):
    row = shared.phase_checkpoint(install, b if phase == "applied" else a, phase, snapshot, observer)
    row["actual_candidate_worker"] = shared.actual_helper_receipt(install, observer, b)
    install.checkpoint()


def exercise_api(install, a, b, observer, receipt):
    session = AssistantSession(install, receipt)
    session.login()
    authenticated_checkpoint(install)
    before = session.status()
    require(before.get("assistant") is True and before["offer"]["id"] == receipt["offer_id"], "Candidate assistant offer is not selected")
    observer.begin("applied")
    request_id = str(uuid.uuid4())
    session.submit_once("request", {"request_id": request_id, "offer_id": receipt["offer_id"]}, quick=True)
    applied = await_update(session, request_id, "applied", observer)
    checkpoint(install, a, b, observer, "applied", applied)
    observer.begin("rolled_back")
    rollback_id = str(uuid.uuid4())
    session.submit_once("recover", {"request_id": rollback_id, "operation_id": applied["operation"]["id"], "action": "rollback"})
    restored = await_update(session, rollback_id, "rolled_back", observer)
    checkpoint(install, a, b, observer, "rolled_back", restored)
    return {"status": "passed", "scope": "managed_api_only_single_update", "before": before, "applied": applied,
            "restored": restored, "mutation_retries": 0, "browser_interaction": False}, session


def exercise_browser(install, a, b, observer, receipt, script, node, artifacts, artifacts_identity):
    require(artifact_identity(artifacts) == artifacts_identity and not any(artifacts.iterdir()),
            "Reserved browser artifact directory changed before browser launch")
    child = subprocess.Popen([str(node), str(script)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, cwd=ROOT)
    events, stdout, stderr = queue.Queue(maxsize=8), shared.PrivateCapture(), shared.PrivateCapture()
    readers = [threading.Thread(target=shared.read_frames, args=(child.stdout, events, stdout), daemon=True),
               threading.Thread(target=shared.drain_bounded, args=(child.stderr, stderr), daemon=True)]
    for reader in readers:
        reader.start()
    phases, result, failure = [], None, None
    deadline = time.monotonic()+900
    try:
        shared.write_frame(child.stdin, {"type": "init", "url": f"http://127.0.0.1:{receipt['port']}",
            "offer_id": receipt["offer_id"], "assistant_identity": receipt, "password": consumer.PASSWORD, "artifact_dir": str(artifacts),
            "previous_version": read_json(a / "manifest.json")["version"], "candidate_version": receipt["candidate_version"],
            "ack_target_seconds": ACK_TARGET_SECONDS, "request_seconds": REQUEST_SECONDS, "background_seconds": BACKGROUND_SECONDS})
        while time.monotonic() < deadline:
            if observer.phase is not None:
                observer.sample()
            try:
                kind, frame = events.get(timeout=0.1)
            except queue.Empty:
                continue
            require(kind != "error", frame if kind == "error" else "Invalid browser frame")
            if kind == "eof":
                break
            require(isinstance(frame, dict) and not any(secret in json.dumps(frame) for secret in [*shared.private_inputs(install), receipt["nonce"]]), "Browser frame is invalid or exposed private input")
            if frame.get("type") == "acknowledgement":
                require(not install.result.get("request_dispatch"), "Duplicate browser Update acknowledgement")
                install.result["request_dispatch"] = [frame]
                install.checkpoint()
            elif frame.get("type") == "assistant_close":
                require(phases == ["authenticated", "applied", "rolled_back"] and frame.get("attempts") == 1
                        and frame.get("state") in {"intent_recorded", "reply_received", "transport_unknown"}, "Assistant close preceded terminal checkpoints or changed intent")
                prior = install.result.get("assistant_close")
                require(prior is None and frame["state"] == "intent_recorded" or prior is not None and prior["state"] == "intent_recorded" and frame["state"] != "intent_recorded", "Assistant close was duplicated")
                install.result["assistant_close"] = frame
                install.checkpoint()
            elif frame.get("type") == "checkpoint":
                phase = frame.get("phase")
                require(phase == ["authenticated", "applied", "rolled_back"][len(phases)] if len(phases) < 3 else False, "Browser checkpoint order changed")
                if phase == "authenticated":
                    authenticated_checkpoint(install)
                    observer.begin("applied")
                else:
                    checkpoint(install, a, b, observer, phase, frame["snapshot"])
                    if phase == "applied":
                        observer.begin("rolled_back")
                phases.append(phase)
                shared.write_frame(child.stdin, {"type": "ack", "phase": phase,
                    **({"transaction_id": frame["snapshot"]["operation"]["transaction_id"]} if phase != "authenticated" else {})})
            elif frame.get("type") == "result":
                require(result is None and phases == ["authenticated", "applied", "rolled_back"] and frame.get("status") == "passed"
                        and frame.get("media_calls") == frame.get("provider_calls") == 0 and frame.get("page_errors") == [], "Browser result lacks complete bounded proof")
                result = frame
            else:
                raise AssertionError("Unexpected browser frame")
        require(result is not None, "Single Update browser ended without complete proof; original operation retained")
        child.wait(timeout=10)
        require(child.returncode == 0, "Single Update browser failed")
        return result
    except BaseException as error:
        failure = error
        raise
    finally:
        if child.poll() is None:
            child.kill()  # Only this exact fixture browser-controller child.
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                install.unsettled_commands.append(child)
        for reader in readers:
            reader.join(timeout=1)
        consumer.retain_browser_output(artifacts, stdout.value(), stderr.value(), [*shared.private_inputs(install), receipt["nonce"]],
                                       returncode=child.returncode, timed_out=isinstance(failure, TimeoutError))
        for stream in [child.stdin, child.stdout, child.stderr]:
            stream.close()


def await_assistant_stopped(install, saved):
    """Adapt the qualified cleanup observer to the assistant's own lock/port.

    Keep one nonblocking connect until native completion, including Windows
    10035. No installed-service lock, HTTP request, Stop or close replay occurs.
    """
    started = time.monotonic()
    deadline = started + 15
    row = {"status": "observing", "timeout_seconds": 15, "authenticated_requests": 0,
           "samples": 0, "connection_attempts": 0, "poll_slices": 0}
    install.result["assistant"]["cleanup"]["listener_closure"] = row
    install.checkpoint()

    def identity():
        require(ordinary_identity(install.home, directory=True) == install.assistant_home_identity
                and ordinary_identity(install.home / "native-update-assistant.lock") == install.assistant_lock_identity,
                "Assistant listener or lock identity changed")
        current, _digest = assistant_receipt(install)
        require(current == saved, "Assistant listener or lock identity changed")

    connection, pending = None, False
    refused = {errno.ECONNREFUSED, getattr(errno, "WSAECONNREFUSED", 10061)}
    in_progress = {errno.EINPROGRESS, errno.EWOULDBLOCK, getattr(errno, "WSAEWOULDBLOCK", 10035)}
    try:
        identity()
        row["identity_sha256"] = fingerprint(saved)
        row["lock_identity_sha256"] = fingerprint(install.assistant_lock_identity)
        while time.monotonic() < deadline:
            identity()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            if connection is None:
                connection = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                connection.setblocking(False)
                row["connection_attempts"] += 1
                code = connection.connect_ex(("127.0.0.1", saved["port"]))
                pending = code in in_progress
                if code == 0:
                    state = "open"
                elif code in refused:
                    state = "refused"
                elif not pending:
                    row["socket_error"] = code
                    raise AssertionError("Assistant listener closure is unknown after socket error")
            if pending:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                readable, writable, exceptional = select.select([], [connection], [connection], min(0.2, remaining))
                row["poll_slices"] += 1
                require(not readable and all(item is connection for item in writable + exceptional),
                        "Assistant connection readiness is ambiguous")
                if not writable and not exceptional:
                    state = "pending"
                else:
                    code = connection.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
                    pending = False
                    if code in refused:
                        state = "refused"
                    elif code == 0 and writable and not exceptional:
                        state = "open"
                    else:
                        row["socket_error"] = code
                        raise AssertionError("Assistant listener closure is unknown after socket readiness")
            row.update(samples=row["samples"] + 1, listener=state)
            free = assistant_lock_available(install.home, install.assistant_lock_identity)
            row["assistant_lock_available"] = free
            identity()
            remaining = deadline - time.monotonic()
            if not pending:
                # A completed OPEN or a refusal with a held lock needs a fresh
                # later sample. A still-pending connect must never be replaced.
                connection.close()
                connection = None
            if state == "refused" and free and remaining > 0:
                row.update(status="stopped", listener_refused=True, assistant_lock_released=True, connect_errno=code)
                return row
            if remaining <= 0:
                break
            if not pending:
                time.sleep(min(0.1, remaining))
        raise AssertionError("Assistant listener or ownership lock did not close within 15 seconds")
    except BaseException as error:
        row.update(status="unknown", error_type=type(error).__name__)
        raise
    finally:
        try:
            if connection is not None:
                connection.close()
        except Exception as error:
            row.update(status="unknown", error_type=type(error).__name__)
            raise
        finally:
            row["elapsed_seconds"] = time.monotonic() - started
            install.checkpoint()


def verify_assistant_exit(install, child, saved, observer):
    """Observe a supported close; never stop an app or worker by raw PID."""
    row = {"status": "observing"}
    install.result["assistant"]["cleanup"] = row
    install.checkpoint()
    try:
        child.wait(timeout=12)
        require(child.returncode == 0, "Assistant did not return normally after supported close")
        row["normal_exit"] = True
        observed = observer.call({"kind": "original"}, previous=saved["generation"])
        require(observed.get("previous_process_alive") is False, "Exact assistant generation absence is unproved")
        row["exact_generation_absent"] = True
        closed = await_assistant_stopped(install, saved)
        row.update(status="stopped", listener_refused=closed["listener_refused"],
                   assistant_lock_released=closed["assistant_lock_released"], connect_errno=closed["connect_errno"])
    except BaseException as error:
        row.update(status="unknown", error_type=type(error).__name__)
        raise
    finally:
        install.checkpoint()


def verify_exact_children(install):
    """Only the one automatic apply and deliberate rollback may own workers."""
    phases = install.result["phase_snapshots"]
    require([row["phase"] for row in phases] == ["applied", "rolled_back"], "Missing final native checkpoints")
    expected = {row["operation_id"] for row in phases}
    require(len(expected) == 2, "Apply and rollback reused a native operation")
    observed = {"status": "observing", "expected_operation_ids": sorted(expected)}
    install.result["exact_children"] = observed
    install.checkpoint()
    saved = shared.retained_operations(install.home)
    operation_ids = set(saved["ledger"]["operations"])
    # Evidence keys retain the producing host's separators; parsing does not
    # rewrite those keys or their bound fingerprint.
    archived = (PureWindowsPath(name) for name in saved["immutable_history"])
    operation_ids.update(path.stem for path in archived if path.parts[:-1] == ("operations",))
    observed.update(observed_operation_ids=sorted(operation_ids)[:64],
                    observed_operation_count=len(operation_ids), operation_ids_truncated=len(operation_ids) > 64,
                    retained_operation_evidence_sha256=fingerprint(saved))
    install.checkpoint()
    require(operation_ids == expected, "An extra or missing native child was retained")
    workers = [key for key in shared.launch_keys(install.home) if key[0] == "worker"]
    observed.update(observed_worker_count=len(workers), worker_attempts_truncated=len(workers) > 64,
                    observed_worker_attempts=[{"operation_id": key[1], "attempt_id": key[2]}
                                              for key in sorted(workers)[:64]])
    install.checkpoint()
    require(len(workers) == 2 and {key[1] for key in workers} == expected
            and all(key[1] == key[2] for key in workers), "A native child has extra or changed worker attempts")
    observed.update(status="passed", operation_ids=sorted(expected), worker_attempts=2, mutation_retries=0)
    install.checkpoint()


def qualify(previous_archive, previous_receipt, candidate_archive, candidate_receipt, controller, output, *, mode="browser", browser_node=None, browser_artifacts=None):
    require(os.environ.get("CONSTELLATION_NATIVE_STARTUP_TEST") == "1", "Explicit disposable-current-user manager opt-in required")
    require(mode in {"browser", "api"}, "Unknown single Update fixture mode")
    require(output.is_absolute() and not output.exists(), "Single Update requires a fresh absolute receipt path")
    require(controller.is_absolute() and controller.is_file() and os.access(controller, os.X_OK), "Native observer controller is required")
    if mode == "browser":
        require(browser_node is not None and browser_node.is_absolute() and browser_node.is_file()
                and os.access(browser_node, os.X_OK) and browser_artifacts is not None,
                "Browser mode requires an explicit private Node and fresh artifact directory")
        artifacts_identity = fresh_browser_artifacts(browser_artifacts)
    package = lifecycle.clean.package_module()
    baseline = read_json(previous_receipt)
    manifest, contents = package.inspect_archive(previous_archive, baseline["archive_sha256"])
    require(lifecycle.digest(contents["manifest.json"]) == baseline["manifest_sha256"]
            and manifest["source_revision"] == baseline["source_revision"] and manifest["target"] == baseline["target"] == lifecycle.clean.native_target(), "Previous package receipt differs")
    candidate_manifest, candidate_contents, candidate = shared.explicit_candidate(package, manifest, contents, candidate_archive, candidate_receipt)
    require(version(candidate_manifest["version"]) > version(manifest["version"]), "Candidate must be a real newer release; do not relabel binaries")
    root = Path(tempfile.mkdtemp(prefix="Companion single update café ")).resolve()
    result = {"schema": 1, "status": "running", "scope": "managed_actual_browser_single_update" if mode == "browser" else "managed_api_only_single_update",
        "target": manifest["target"], "previous": {"source_revision": manifest["source_revision"], "version": manifest["version"], "archive_sha256": baseline["archive_sha256"], "manifest_sha256": baseline["manifest_sha256"]},
        "candidate": candidate, "controller_sha256": lifecycle.digest(controller.read_bytes()), "cases": {},
        "request_seconds": REQUEST_SECONDS, "background_seconds": BACKGROUND_SECONDS, "ack_target_seconds": ACK_TARGET_SECONDS,
        "model_requests": 0, "provider_requests": 0, "voice_calls": 0, "mutation_retries": 0,
        "limitations": ["Actual user manager session and supplied previous/candidate packages; not clean-OS/signing/media proof", "API mode does not claim browser interaction; browser mode uses the actual candidate UI", "Unknown failures preserve the disposable installation and registrations without replay"]}
    installations = []
    try:
        a, b = root / "Previous release", root / "Candidate release"
        lifecycle.extract(contents, manifest, a)
        lifecycle.extract(candidate_contents, candidate_manifest, b)
        case_root = root / "managed"
        case_root.mkdir()
        case = {"status": "running", "checks": []}
        result["cases"][mode] = case
        install = shared.UpdateInstallation(case_root, a, manifest, case, outer_result=result, checkpoint_output=output, controller=controller, managed=True)
        installations.append((mode, install))
        install.controller_environment = shared.manager_environment(install, shared.startup_module())
        prepared = consumer.prepare_consumer_installation(install, managed_parent=True)
        require(case["setup_receipt"].get("manifest_sha256") == baseline["manifest_sha256"],
                "Managed setup selected a different previous package manifest")
        install.selected(a)
        shared.seed_retained_work(install, prepared["conversation_id"])
        case["baseline"] = shared.protected_receipt(install)
        observer = shared.NativeObserver(install, controller, managed=True, retain_application_identity=True)
        assistant, receipt = launch_assistant(install, b, candidate_manifest)
        if mode == "browser":
            case["browser"] = exercise_browser(install, a, b, observer, receipt, ROOT / "tests/native_single_update_browser.mjs", browser_node, browser_artifacts, artifacts_identity)
        else:
            case["api"], session = exercise_api(install, a, b, observer, receipt)
            close_assistant(session)
        require(case.get("assistant_close", {}).get("state") in {"reply_received", "transport_unknown"}, "No single supported assistant-close observation")
        verify_assistant_exit(install, assistant, receipt, observer)
        verify_exact_children(install)
        require([json.loads(line)["method"] for line in prepared["fixture_log"].read_text().splitlines()] == ["initialize"], "Update redispatched ACP work")
        shared.cleanup_case(install, observer, install.registration, install.controller_environment)
        case["status"] = "passed"
        install.checkpoint()
        shutil.rmtree(root)
        result.update(status="passed", temporary_installations_removed=True)
    except BaseException as error:
        result.update(status="failed", error_type=type(error).__name__, error=str(error), retained_test_directory=str(root),
                      cleanup_policy="Unknown or failed operation retained; no mutation retry or raw service removal")
        result["failure_installation_snapshots"] = lifecycle.capture_failure_snapshots(installations)
        lifecycle.persist_checkpoint(output, result)
        raise
    lifecycle.persist_checkpoint(output, result)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["previous-archive", "previous-receipt", "candidate-archive", "candidate-receipt", "controller", "output"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--mode", choices=["browser", "api"], default="browser")
    parser.add_argument("--browser-node", type=Path)
    parser.add_argument("--browser-artifacts", type=Path)
    args = parser.parse_args()
    result = qualify(args.previous_archive.resolve(), args.previous_receipt.resolve(),
        args.candidate_archive.resolve(), args.candidate_receipt.resolve(), args.controller.resolve(), args.output.resolve(),
        mode=args.mode, browser_node=args.browser_node.resolve() if args.browser_node else None,
        browser_artifacts=args.browser_artifacts)
    print(json.dumps({"status": result["status"], "scope": result["scope"],
                      "temporary_installations_removed": result.get("temporary_installations_removed")}))
