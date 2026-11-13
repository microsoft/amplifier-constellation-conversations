#!/usr/bin/env python3
"""Actual disposable manager/application pre-status rollback qualification.

This is deliberately separate from the normal consumer and immutable Windows
diagnostic fixtures. A is the exact supplied product package. B changes only
the service to the independently hashed test-only native-prestatus-app example
and adds the existing synthetic bundle marker. No production fault flag exists.

The default verdict requires proved stop-only cleanup and separately verified
previous restoration. --expect-gap retains an exact older-source red result;
it cannot be reported as successful recovery. Never retry an update mutation.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import stat
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("prestatus_consumer_update", ROOT / "tests/native_consumer_update.py")
consumer_update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(consumer_update)
consumer = consumer_update.consumer
lifecycle = consumer_update.lifecycle
require, read_json = lifecycle.require, lifecycle.read_json

CONTROL = "native-prestatus-fixture-control.json"
OBSERVATION = "native-prestatus-fixture-observation.json"
MAX_OBSERVATION_BYTES = 16384
DIAGNOSTIC_LAUNCH_CODES = frozenset({
    "dispatch_or_retirement_in_progress", "dispatch_already_attempted", "outstanding_index_busy",
    "dispatch_intent_unconfirmed", "dispatch_index_unconfirmed", "dispatch_authority_unconfirmed",
    "fixed_profile_mismatch", "existing_or_transitioning_instance", "manager_dispatch_unconfirmed",
    "dispatch_receipt_unconfirmed",
})


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def diagnostic_uuid(value):
    return isinstance(value, str) and len(value) == 36 and str(uuid.UUID(value)) == value


def diagnostic_json(path, limit):
    """Bounded fixed-file read; return no file text, path, or exception text."""
    try:
        parent = path.parent.lstat()
        require(stat.S_ISDIR(parent.st_mode) and not getattr(parent, "st_file_attributes", 0) & 0x400,
                "Diagnostic parent is not ordinary")
        before = path.lstat()
        require(stat.S_ISREG(before.st_mode) and not getattr(before, "st_file_attributes", 0) & 0x400,
                "Diagnostic file is not ordinary")
        if before.st_size > limit:
            return "too_large", None, None
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0))
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            require(stat.S_ISREG(opened.st_mode) and not getattr(opened, "st_file_attributes", 0) & 0x400
                    and (opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino),
                    "Diagnostic file changed")
            raw = stream.read(limit + 1)
            after = os.fstat(stream.fileno())
        if len(raw) > limit:
            return "too_large", None, None
        require((opened.st_size, opened.st_mtime_ns) == (after.st_size, after.st_mtime_ns),
                "Diagnostic file changed")
        value = json.loads(raw)
        require(type(value) is dict, "Diagnostic JSON is not an object")
        return "available", value, hashlib.sha256(raw).hexdigest()
    except FileNotFoundError:
        return "absent", None, None
    except (ValueError, UnicodeError, AssertionError):
        return "invalid", None, None
    except Exception:
        return "unavailable", None, None


def diagnostic_launch_receipt(value):
    if value is None:
        return {"status": "not_recorded"}
    require(type(value) is dict and value.get("status") in {"accepted", "refused", "unknown"},
            "Unknown launch receipt")
    expected = {"status"} if value["status"] == "accepted" else {"status", "code"}
    require(set(value) == expected, "Malformed launch receipt")
    result = {"status": value["status"]}
    if "code" in value:
        require(isinstance(value["code"], str) and len(value["code"]) <= 1024, "Invalid receipt code")
        result["code"] = value["code"] if value["code"] in DIAGNOSTIC_LAUNCH_CODES else "unknown"
        result["code_sha256"] = fingerprint(value["code"])
    return result


def diagnostic_worker_observation(value):
    """Project even unavailable native observations without private arguments."""
    try:
        require(type(value) is dict and value.get("status") in {"observed", "unavailable", "absent", "not_dispatched"},
                "Unrecognized worker observation")
        result = {"status": value["status"]}
        for key in ("profile_matches", "active", "transitioning", "enabled", "masked", "definition_matches",
                    "registered", "previous_process_alive", "actual_worker_alive"):
            if key in value:
                # Native snapshots and an unclaimed worker use null for an
                # unknown optional flag. Preserve it without inferring false.
                require(value[key] is None or type(value[key]) is bool, "Invalid observation flag")
                result[key] = value[key]
        for key in ("manager", "application_manager_binding", "actual_worker_manager_binding"):
            if key in value:
                require(value[key] in {None, "systemd_user", "launch_agent", "scheduled_task",
                                      "same_process", "descendant_of_task_engine"}, "Invalid observation enum")
                result[key] = value[key]
        for key in ("code", "service_name", "actual_worker_identity"):
            if key in value and value[key] is not None:
                result[key + "_sha256"] = fingerprint(value[key])
        if "launch_receipt" in value:
            result["launch_receipt"] = diagnostic_launch_receipt(value["launch_receipt"])
        if "processes" in value:
            require(type(value["processes"]) is list and len(value["processes"]) <= 16,
                    "Too many diagnostic processes")
            result["processes"] = []
            for item in value["processes"]:
                require(type(item) is dict and type(item.get("identity")) is dict, "Invalid process identity")
                result["processes"].append({"identity_sha256": fingerprint(item["identity"])})
        return result
    except Exception:
        return {"status": "unknown", "reason": "invalid_native_observation"}


class FaultObserver(consumer_update.NativeObserver):
    """Retain results of existing inspections only; never make an extra call."""
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.diagnostic_observations = []
        self.diagnostic_observation_count = 0
        self.diagnostic_request_id = None
        self.diagnostic_check_id = None

    def call(self, target, **kwargs):
        try:
            value = super().call(target, **kwargs)
        except BaseException:
            self.retain_inspection(target, {"status": "unavailable", "code": "inspection_raised"})
            raise
        self.retain_inspection(target, value)
        return value

    def retain_inspection(self, target, value):
        try:
            if target.get("kind") != "worker":
                return
            require(diagnostic_uuid(target.get("operation")) and diagnostic_uuid(target.get("attempt")),
                    "Invalid inspection identity")
            row = {"operation_sha256": fingerprint(target["operation"]),
                   "attempt_sha256": fingerprint(target["attempt"]),
                   "observation": diagnostic_worker_observation(value)}
            self.diagnostic_observation_count += 1
            self.diagnostic_observations.append(row)
            self.diagnostic_observations = self.diagnostic_observations[-8:]
        except BaseException:
            # Diagnostics must not change the existing observer call's result.
            pass


def capture_fault_diagnostics(install, observer):
    """Failure-only attribution to the original request; no status/manager call."""
    result = {"schema": 1, "status": "unknown", "consistency": "bounded_best_effort_local_reads",
              "additional_native_or_http_calls": 0}
    try:
        request = observer.diagnostic_request_id
        check = observer.diagnostic_check_id
        require(diagnostic_uuid(request) and diagnostic_uuid(check), "No original apply identity")
        result["request_id"] = request
        home_metadata = install.home.lstat()
        require(stat.S_ISDIR(home_metadata.st_mode)
                and not getattr(home_metadata, "st_file_attributes", 0) & 0x400, "Diagnostic home is not ordinary")
        config_status, config, config_hash = diagnostic_json(install.home / "config.json", 65536)
        ledger_path = install.home / "native-browser-update" / "control.json"
        ledger_status, ledger, ledger_hash = diagnostic_json(ledger_path, 4 * 1024 * 1024)
        result["files"] = {"config": config_status, "browser_ledger": ledger_status}
        require(config_status == ledger_status == "available", "Unavailable diagnostic binding")
        installation_id = config.get("id")
        require(diagnostic_uuid(installation_id) and type(ledger.get("schema")) is int and ledger["schema"] == 1
                and ledger.get("installation_id") == installation_id, "Installation binding changed")
        request_record = ledger["requests"][request]
        operation_id = request_record["operation_id"]
        attempt_id = request_record["attempt_id"]
        require(operation_id == request and attempt_id == request
                and ledger["current_operation"] == operation_id
                and request_record["payload"] == {"kind": "apply", "check_id": check},
                "Original request is not current")
        operation = ledger["operations"][operation_id]
        attempt = operation["attempt"]
        transaction = operation["transaction_id"]
        require(operation["id"] == operation_id and operation["request_id"] == request
                and operation["kind"] == "apply" and attempt["id"] == attempt_id
                and diagnostic_uuid(transaction) and type(attempt["claimed"]) is bool
                and operation["state"] in {"accepted", "running", "completed", "deferred", "failed", "unknown"}
                and operation["outcome"] in {None, "unchanged", "applied", "rolled_back"},
                "Operation binding or state changed")
        for attribute, expected in (("operation", operation_id), ("attempt", attempt_id), ("transaction", transaction)):
            captured = getattr(observer, attribute, None)
            require(captured is None or captured == expected, "Captured observer identity changed")
        identities = {"operation_sha256": fingerprint(operation_id), "attempt_sha256": fingerprint(attempt_id),
                      "transaction_sha256": fingerprint(transaction)}
        launch = attempt.get("launch")
        require(launch is None or type(launch) is dict, "Invalid launch profile")
        process = attempt.get("worker_process")
        require(process is None or type(process) is dict, "Invalid worker identity")
        browser = {**identities, "state": operation["state"], "outcome": operation["outcome"],
                   "claimed": attempt["claimed"], "launch_present": launch is not None,
                   "worker_process_present": process is not None}
        if process is not None:
            browser["worker_process_sha256"] = fingerprint(process)
        dispatch = {"status": "not_prepared"}
        if launch is not None:
            require(type(launch.get("schema")) is int and launch["schema"] == 1 and launch.get("installation_id") == installation_id
                    and launch.get("home") == str(install.home.resolve())
                    and launch.get("identity") == {"kind": "worker", "operation": operation_id, "attempt": attempt_id},
                    "Prepared launch identity changed")
            service = launch["service"]
            manager = service["manager"]
            name = service["name"]
            require(manager in {"systemd_user", "launch_agent", "scheduled_task"}
                    and isinstance(name, str) and re.fullmatch(r"agent-conversations-worker-[0-9a-f]{64}(?:\.service)?", name)
                    and name.endswith(".service") == (manager == "systemd_user")
                    and type(launch.get("profile")) is dict and launch["profile"].get("autostart") is False,
                    "Invalid fixed worker profile")
            browser["launch_sha256"] = fingerprint(launch)
            browser["profile_sha256"] = fingerprint(launch["profile"])
            marker_status, marker, marker_hash = diagnostic_json(
                install.home / ("native-browser-launch-" + name + ".json"), 1024 * 1024)
            dispatch = {"status": marker_status}
            if marker_status == "available":
                require(marker.get("prepared") == launch and type(marker.get("manager_attempted")) is bool,
                        "Dispatch binding changed or lacks authority")
                launch_receipt = diagnostic_launch_receipt(marker.get("receipt"))
                require(launch_receipt["status"] != "accepted" or marker["manager_attempted"],
                        "Accepted receipt contradicts dispatch authority")
                dispatch.update(sha256=marker_hash, manager_attempted=marker["manager_attempted"],
                                receipt=launch_receipt)
        observations = getattr(observer, "diagnostic_observations", [])
        count = getattr(observer, "diagnostic_observation_count", 0)
        require(type(observations) is list and len(observations) <= 8 and type(count) is int and count >= len(observations),
                "Invalid captured observations")
        require(all(row.get("operation_sha256") == identities["operation_sha256"]
                    and row.get("attempt_sha256") == identities["attempt_sha256"] for row in observations),
                "Captured inspection belongs to another attempt")
        # No liveness/health assertion: these are already observed native results.
        observed = {"count": count, "last": observations, "additional_calls": 0,
                    "last_worker": diagnostic_worker_observation(observer.worker_observation)
                    if getattr(observer, "worker_observation", None) is not None else {"status": "not_captured"}}
        again_status, _, again_hash = diagnostic_json(ledger_path, 4 * 1024 * 1024)
        config_again_status, _, config_again_hash = diagnostic_json(install.home / "config.json", 65536)
        require(again_status == config_again_status == "available" and config_hash == config_again_hash
                and again_hash == ledger_hash, "Binding changed during diagnostic observation")
        result.update(status="bound", installation_id_sha256=fingerprint(installation_id),
                      browser_ledger_sha256=ledger_hash, browser=browser, dispatch=dispatch, observer=observed)
    except BaseException:
        result.update(status="unknown", reason="missing_invalid_or_changed_original_binding")
    return result


def retain_fault_diagnostics(install, observer):
    try:
        install.result["failure_request_diagnostics"] = capture_fault_diagnostics(install, observer)
    except BaseException:
        install.result["failure_request_diagnostics"] = {"schema": 1, "status": "unknown", "reason": "capture_failed"}
    try:
        install.checkpoint()
    except BaseException:
        pass  # Preserve the original fixture exception, including interrupts.


def read_observation(home):
    path = home / OBSERVATION
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    require(stat.S_ISREG(metadata.st_mode) and not path.is_symlink()
            and not getattr(metadata, "st_file_attributes", 0) & 0x400
            and metadata.st_size <= MAX_OBSERVATION_BYTES, "Invalid pre-status fixture observation")
    value = read_json(path)
    require(value.get("schema") == 1 and value.get("boundary") == "after_appstate_before_listener"
            and value.get("listener_started") is False and value.get("health_proved") is False
            and value.get("service_lock_held") is True, "Unexpected fixture boundary")
    require(value.get("wait_expired") is False, "Fixture lifetime expired; no manager-stop success may be inferred")
    return value


def derive_fault_bundle(a, b, fixture):
    manifest = lifecycle.derive_bundle(a, b, "prestatus-fault")
    relative = manifest["entrypoints"]["service"]
    binary = fixture.read_bytes()
    destination = b / relative
    destination.write_bytes(binary)
    destination.chmod(0o755)
    rows = [row for row in manifest["files"] if row["path"] == relative]
    require(len(rows) == 1, "Candidate service has ambiguous manifest identity")
    digest = hashlib.sha256(binary).hexdigest()
    rows[0].update(sha256=digest, bytes=len(binary))
    lifecycle.write_json(b / "manifest.json", manifest)
    return digest


def matching_cleanup_record(journal, observed):
    records = [journal.get("launch_cleanup"), *journal.get("launch_cleanup_history", [])]
    matches = [row for row in records if isinstance(row, dict)
               and row.get("binding", {}).get("transaction_id") == observed["transaction_id"]
               and row.get("binding", {}).get("nonce") == observed["launch_nonce"]
               and row.get("binding", {}).get("package", {}).get("binary_sha256") == observed["binary_sha256"]]
    require(len(matches) == 1, "Missing or ambiguous candidate stop-only receipt")
    record = matches[0]
    require(record.get("schema") == 1 and record.get("start", {}).get("status") == "accepted"
            and record.get("revoked") is True and record.get("stop", {}).get("status") == "accepted",
            "Candidate Start/revocation/Stop acknowledgements are not accepted")
    authority = record.get("authority")
    require(isinstance(authority, dict) and authority.get("binding") == record["binding"]
            and authority.get("application") == observed["process_identity"],
            "Stop authority differs from actual pre-status application generation")
    exit_proof = record.get("exit_before_switch")
    require(isinstance(exit_proof, dict) and exit_proof.get("application_exited") is True
            and exit_proof.get("service_lock_acquired") is True and exit_proof.get("cleanup_is_health") is False
            and exit_proof.get("binary_sha256") == observed["binary_sha256"],
            "Actual candidate exit was not persisted before its binary switched")
    native = exit_proof.get("native_snapshot", {})
    require(native.get("active") is False and native.get("transitioning") is False
            and native.get("processes") == [], "Stopped manager settlement is unproved")
    return record


def candidate_target(managed, transaction, nonce):
    return {"kind": "original"} if managed else {"kind": "replacement", "transaction": transaction, "nonce": nonce}


def observe_fault(install, observer, request_id, fixture_id, fixture_sha, *, timeout=90):
    deadline = time.monotonic() + timeout
    observed, native = None, None
    while time.monotonic() < deadline:
        observer.sample()
        value = read_observation(install.home)
        if value is not None:
            require(value["fixture_id"] == fixture_id and value["binary_sha256"] == fixture_sha,
                    "Fault child differs from its immutable fixture binding")
            require(observer.transaction == value["transaction_id"], "Fault belongs to another update transaction")
            require(not install.service_lock_available(), "Pre-status candidate does not hold the real service lock")
            target = candidate_target(observer.managed, observer.transaction, value["launch_nonce"])
            native = observer.call(target, previous=value["process_identity"])
            require(native.get("status") == "observed" and native.get("profile_matches") is True
                    and native.get("previous_process_alive") is True
                    and native.get("previous_process_manager_binding") in {"same_process", "descendant_of_task_engine"},
                    "Pre-status application generation is not proved under the actual manager")
            # Current listener absence is independent of the fixture's own flag.
            require(consumer_update.request_status(install, request_id, tolerate_unavailable=True) is None,
                    "Fault candidate unexpectedly serves authenticated update status")
            observed = value
            break
        time.sleep(0.1)
    require(observed is not None, "Real pre-status process boundary was not observed")
    install.result["fault_boundary"] = {
        "transaction_id": observed["transaction_id"], "fixture_id": fixture_id,
        "process_identity_sha256": fingerprint(observed["process_identity"]),
        "nonce_sha256": fingerprint(observed["launch_nonce"]), "binary_sha256": fixture_sha,
        "actual_service_lock_contended": True, "authenticated_status_unavailable": True,
        "manager": consumer_update.public_observation(native),
        "actual_application_manager_binding": native["previous_process_manager_binding"],
        "AppState_opened": True, "cleanup_is_health": False,
    }
    install.checkpoint()
    return observed


def await_fault_terminal(install, observer, observed, *, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        observer.sample()
        try:
            journal = read_json(install.home / "native-update.json")
            ledger = read_json(install.home / "native-browser-update/control.json")
        except (FileNotFoundError, PermissionError):
            # Read-only observation may overlap protected journal publication.
            # Stay within this original clock; never repeat the mutation.
            time.sleep(0.1)
            continue
        require(journal["transaction_id"] == observed["transaction_id"], "Fault transaction changed")
        operation = ledger["operations"][observer.operation]
        # Durable state is necessary, but only native observation proves that
        # this worker's application generation has actually stopped executing.
        worker = observer.worker_observation
        worker_exited = (worker and worker.get("actual_worker_alive") is False
                         and worker.get("processes") == [] and worker.get("transitioning") is False)
        if worker_exited and operation.get("state") in {"failed", "completed", "unknown"}:
            return journal, operation
        time.sleep(0.1)
    raise TimeoutError("Fault worker terminal remains unobserved; no retry or implicit recovery")


def teardown_candidate_after_red(install, observer, observed):
    """Post-proof disposal only, never part of the claimed rollback outcome."""
    target = candidate_target(observer.managed, observed["transaction_id"], observed["launch_nonce"])
    before = observer.call(target, previous=observed["process_identity"])
    require(before.get("previous_process_alive") is True
            and before.get("previous_process_manager_binding") in {"same_process", "descendant_of_task_engine"},
            "Cannot dispose an unknown/changed fault generation")
    stopped = observer.call(target, action="stop", previous=observed["process_identity"])
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        current = observer.call(target, previous=observed["process_identity"])
        if (current.get("previous_process_alive") is False and current.get("active") is False
                and current.get("transitioning") is False and current.get("processes") == []
                and install.service_lock_available()):
            return {"status": "exact_fault_child_stopped_after_red_proof", "dispatch": stopped["dispatch"],
                    "registration_retained": True, "transaction_not_restored": True}
        time.sleep(0.1)
    raise TimeoutError("Post-proof exact-generation cleanup is unknown; no repeated Stop")


def teardown_restored_after_green(install, observer, journal):
    """Stop the independently verified previous listener after recording proof.

    This is not recovery authority or a retry. Failed one-shot registrations are
    retained, and no restoration or removal is inferred from this final Stop.
    """
    receipt = journal.get("launch_cleanup", {})
    authority = receipt.get("authority", {})
    application = authority.get("application")
    require(isinstance(application, dict) and receipt.get("start", {}).get("status") == "accepted",
            "Previous restored process lacks an exact accepted generation for disposal")
    target = candidate_target(observer.managed, journal["transaction_id"], receipt["binding"]["nonce"])
    before = observer.call(target, previous=application)
    require(before.get("profile_matches") is True and before.get("previous_process_alive") is True
            and before.get("previous_process_manager_binding") in {"same_process", "descendant_of_task_engine"},
            "Restored fixture generation changed before disposal")
    stopped = observer.call(target, action="stop", previous=application)
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        current = observer.call(target, previous=application)
        if (current.get("previous_process_alive") is False and current.get("active") is False
                and current.get("transitioning") is False and current.get("processes") == []
                and install.service_lock_available()):
            return {"status": "restored_fixture_stopped_after_green_proof", "dispatch": stopped["dispatch"],
                    "registrations_retained_for_checked_owner_cleanup": True,
                    "no_reset_failed_or_raw_removal": True, "healthy_test_listener_left_running": False}
        time.sleep(0.1)
    raise TimeoutError("Post-proof restored fixture Stop remains unobserved; no retry")


def exercise_fault(install, observer, a, b, fixture_sha, *, expect_gap):
    try:
        return exercise_fault_inner(install, observer, a, b, fixture_sha, expect_gap=expect_gap)
    except BaseException:
        try:
            retain_fault_diagnostics(install, observer)
        except BaseException:
            pass
        raise


def exercise_fault_inner(install, observer, a, b, fixture_sha, *, expect_gap):
    before = consumer_update.protected_receipt(install)
    checked = consumer_update.check_once(install, b)
    require(checked["check"]["available"] and checked["actions"]["apply"], "Fault candidate was not checked")
    fixture_id = str(uuid.uuid4())
    lifecycle.write_json(install.home / CONTROL, {"schema": 1, "fixture_id": fixture_id,
                                                "candidate_binary_sha256": fixture_sha})
    request_id = str(uuid.uuid4())
    observer.diagnostic_request_id = request_id
    observer.diagnostic_check_id = checked["check"]["id"]
    observer.begin("applied")
    consumer_update.submit_once(install, "apply", {"request_id": request_id, "check_id": checked["check"]["id"]})
    observed = observe_fault(install, observer, request_id, fixture_id, fixture_sha)
    journal, operation = await_fault_terminal(install, observer, observed)
    target = candidate_target(observer.managed, observed["transaction_id"], observed["launch_nonce"])
    candidate_after = observer.call(target, previous=observed["process_identity"])
    if expect_gap:
        # The old rollback publishes selected=previous/switching BEFORE it
        # attempts Stop. That is intent, not evidence of a completed exchange.
        # Prove the actual installed bytes/receipt remain candidate instead.
        installed = install.installed()
        installed_binary = install.home / "bin" / install.worker.name
        installed_sha = hashlib.sha256(installed_binary.read_bytes()).hexdigest()
        require(journal["phase"] != "restored" and installed_sha == fixture_sha
                and installed["package"]["binary_sha256"] == fixture_sha
                and installed["package"]["bundle"]["manifest_sha256"] == hashlib.sha256((b / "manifest.json").read_bytes()).hexdigest()
                and candidate_after.get("previous_process_alive") is True
                and not install.service_lock_available(), "Old source did not reproduce the claimed pre-status recovery gap")
        require(not journal.get("launch_cleanup"), "Red baseline unexpectedly has the successor cleanup implementation")
        require(install.snapshot() == install.baseline, "Baseline failure changed protected work/account state")
        install.result["red_proof"] = {"status": "gap_reproduced_not_recovery_passed", "journal_sha256": fingerprint(journal),
                                       "phase": journal["phase"], "selection_intent": journal["selected"],
                                       "actual_installed_binary_sha256": installed_sha,
                                       "actual_installed_receipt_sha256": fingerprint(installed),
                                       "worker_terminal": operation["state"], "candidate_still_alive": True}
        install.checkpoint()
        install.result["post_proof_cleanup"] = teardown_candidate_after_red(install, observer, observed)
        return "gap_reproduced"

    receipt = matching_cleanup_record(journal, observed)
    require(journal["phase"] == "restored" and journal["selected"] == "previous"
            and journal.get("restore_outcome") == "rolled_back", "Previous bundle was not separately restored")
    require(candidate_after.get("previous_process_alive") is False,
            "The same fault application generation is still alive after rollback")
    running = install.await_running()
    install.selected(a)
    require(running["binary_sha256"] == journal["previous"]["binary_sha256"]
            and running["admission"]["closed"] is False, "Previous restoration did not independently prove health and open admission")
    require(install.snapshot() == install.baseline, "Rollback changed saved work/account/contact/unknown evidence")
    require(consumer_update.protected_receipt(install) == before, "Protected continuity differs after separately verified restoration")
    original = observer.call({"kind": "original"})
    require(observer.preference(original) == observer.baseline_startup, "Original startup policy changed")
    install.result["recovery_proof"] = {
        "status": "passed", "receipt_sha256": fingerprint(receipt),
        "candidate_application_sha256": fingerprint(receipt["authority"]["application"]),
        "native_instance_sha256": fingerprint(receipt["authority"]["native_snapshot"]),
        "receipt_target_kind": receipt["target"]["kind"], "start_accepted": True, "revoked": True,
        "stop_accepted": True, "actual_exit_before_switch": True, "candidate_binary_at_exit": fixture_sha,
        "cleanup_is_health": False, "previous_health_separately_verified": True,
        "previous_binary_sha256": running["binary_sha256"], "protected_continuity": True,
        "worker_original_apply_failed": operation["state"] == "failed", "mutation_count": 1, "mutation_retries": 0,
    }
    # Native failed registrations are deliberately not reset or removed here.
    # Keep restoration and teardown verdicts separate; the owner can inspect the
    # exact inactive registration through the checked retirement interface.
    install.checkpoint()  # Recovery proof precedes this independent disposal.
    install.result["teardown"] = teardown_restored_after_green(install, observer, journal)
    return "recovery_passed"


def remove_original_once(install, observer):
    """Test-only desktop removal, with durable intent and no lost-ack retry.

    The production CLI correctly rejects startup changes in a still-switching
    red transaction. This controller uses the same accepted desktop primitive
    only after exact child Stop and proof; it never handles a one-shot target.
    """
    started = time.monotonic()
    deadline = started + 30
    receipt_path = install.root / "original-startup-disposal.json"
    require(not receipt_path.exists(), "Original removal was already attempted; observation only, never retry")
    payload = {"home": str(install.home.resolve()), "action": "remove_original",
               "target": {"kind": "original"}, "previous_process": None, "setup_bundle": None,
               "expected_original": observer.baseline_startup}
    receipt = {"schema": 1, "status": "intent", "target": "original_desktop_startup_only",
               "captured_preference_sha256": fingerprint(observer.baseline_startup), "mutation_retries": 0,
               "timeout_seconds": 30}
    with receipt_path.open("x", encoding="utf-8") as stream:
        json.dump(receipt, stream, sort_keys=True, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    child = None
    try:
        # The accepted desktop API uses fixed OS tool lookup. This is the same
        # constrained fixture controller environment as its original install;
        # the product environment and global manager environment stay unchanged.
        require(time.monotonic() < deadline,
                "Original-removal budget expired before dispatch; preserve intent without retry")
        child = subprocess.Popen([str(observer.controller)], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 cwd=install.root, env=install.controller_environment)
        stdout, _ = child.communicate(json.dumps(payload).encode(), timeout=max(0, deadline - time.monotonic()))
        require(len(stdout) <= 65536, "Original-removal controller output exceeded bound")
        value = json.loads(stdout)
        require(isinstance(value, dict), "Original removal returned no structured acknowledgement")
        receipt.update(status="observed", return_code=child.returncode, controller=value,
                       elapsed_seconds=time.monotonic() - started)
        lifecycle.write_json(receipt_path, receipt)
        install.result["original_startup_disposal"] = receipt
        install.checkpoint()
        require(child.returncode == 0 and value.get("status") == "original_removed"
                and value.get("dispatch", {}).get("status") == "accepted"
                and value.get("original_definition_absent") is True and value.get("inactive_after") is True,
                "Original removal refused or remains unknown; preserve its receipt without retry")
    except BaseException as error:
        if child is not None and child.poll() is None:
            child.kill()
            try:
                child.communicate(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                install.unsettled_commands.append(child)
        receipt.update(status="unknown_or_refused", error_type=type(error).__name__, retry_permitted=False,
                       elapsed_seconds=time.monotonic() - started)
        lifecycle.write_json(receipt_path, receipt)
        install.result["original_startup_disposal"] = receipt
        install.checkpoint()
        raise


def finish_disposal(install, observer):
    """Remove only the separately captured, stopped original startup profile.

    Failed/unknown one-shot registrations and journals remain for inspection;
    none is reset, retired by inference, or silently redispatched. Retaining an
    enabled disposable original startup would launch it at the next login, so
    its exact checked removal is a separate, recorded post-proof operation.
    """
    require(install.service_lock_available(), "Fixture still owns its service lock")
    require(all(child.poll() is not None for child in install.unsettled_commands),
            "An unknown controller command remains live; retain its registration")
    ledger = consumer_update.retained_operations(install.home)
    journal_bytes = (install.home / "native-update.json").read_bytes()
    removed = False
    if install.registration is not None:
        original = observer.call({"kind": "original"})
        require(observer.preference(original) == observer.baseline_startup
                and original.get("profile_matches") is True
                and original.get("active") is False and original.get("transitioning") is False
                and original.get("processes") == [], "Original startup changed or remains active before disposal")
        install.registration.inspect()
        remove_original_once(install, observer)
        require(not install.registration.exists(), "Exact original fixture startup removal is unconfirmed")
        removed = True
    require(consumer_update.retained_operations(install.home) == ledger
            and (install.home / "native-update.json").read_bytes() == journal_bytes,
            "Post-proof startup removal changed retained operation evidence")
    require(all(child.poll() is not None for child in install.children),
            "A controller-owned original child remains live after disposal")
    for port in install.ports:
        with socket.socket() as connection:
            connection.settimeout(0.2)
            require(connection.connect_ex(("127.0.0.1", port)) != 0, "An owned fixture listener remains open")
    for stream in install.streams:
        stream.close()
    install.result["post_proof_disposal"] = {
        "original_startup_removed": removed, "owned_listeners_closed": True,
        "controller_children_reaped": True, "service_lock_released": True,
        "failed_or_unknown_one_shot_registrations_retained": True,
        "journal_and_operations_unchanged": True, "directory_retained_for_evidence": True,
        "no_reset_failed_or_raw_registration_removal": True,
    }


def qualify(archive, receipt, output, controller, fixture, *, topology="both", expect_gap=False):
    require(os.environ.get("CONSTELLATION_NATIVE_STARTUP_TEST") == "1", "Explicit disposable manager opt-in required")
    require(topology in {"both", "managed", "manual"}, "Unknown fixture topology")
    for path in (archive, receipt, output, controller, fixture):
        require(path.is_absolute(), "Fixture paths must be absolute")
    require(not output.exists(), "Use a fresh result path; do not retry an old operation")
    for executable in (controller, fixture):
        require(executable.is_file() and os.access(executable, os.X_OK), "Missing exact native fixture/controller")
    provenance = read_json(receipt)
    package = lifecycle.clean.package_module()
    manifest, contents = package.inspect_archive(archive, provenance["archive_sha256"])
    require(manifest["target"] == lifecycle.clean.native_target(), "Package targets another native host")
    require(lifecycle.digest(contents["manifest.json"]) == provenance["manifest_sha256"], "Package manifest receipt differs")
    root = Path(tempfile.mkdtemp(prefix="Companion prestatus café ")).resolve()
    result = {"schema": 1, "status": "running", "target": manifest["target"], "source_revision": manifest["source_revision"],
              "archive_sha256": provenance["archive_sha256"], "fixture_sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(),
              "controller_sha256": hashlib.sha256(controller.read_bytes()).hexdigest(), "expect_gap": expect_gap,
              "model_requests": 0, "provider_requests": 0, "voice_calls": 0, "cases": {},
              "speech_release": "unchanged_paused", "fixture_directory": str(root),
              "limits": ["Actual disposable user-manager application path; no clean-OS or ordinary-desktop claim",
                         "B uses a test-only installed app linked to production startup primitives; not a released product binary",
                         "Barrier follows actual AppState initialization but precedes listener/status; no synthetic health success",
                         "Default production30s health timeout is forced by the deliberately absent listener",
                         "Failed native registrations and uncertain effects are retained; no reset-failed, broad Stop or automatic retry",
                         "No production installation, real provider, game, physical media or publication"]}
    installations = []
    try:
        a, b = root / "Release A", root / "Release B"
        lifecycle.extract(contents, manifest, a)
        fixture_sha = derive_fault_bundle(a, b, fixture)
        scenarios = [("managed", True), ("manual", False)] if topology == "both" else [(topology, topology == "managed")]
        for name, managed in scenarios:
            case_root = root / name
            case_root.mkdir()
            case = {"status": "running", "checks": [], "topology": "original_managed_startup" if managed else "independent_manual_replacement"}
            result["cases"][name] = case
            install = consumer_update.UpdateInstallation(case_root, a, manifest, case, outer_result=result,
                checkpoint_output=output, controller=controller, managed=managed)
            installations.append((name, install))
            install.controller_environment = consumer_update.manager_environment(install, consumer_update.startup_module())
            prepared = consumer.prepare_consumer_installation(install, managed_parent=managed)
            consumer_update.seed_retained_work(install, prepared["conversation_id"])
            observer = FaultObserver(install, controller, managed=managed)
            case["status"] = exercise_fault(install, observer, a, b, fixture_sha, expect_gap=expect_gap)
            finish_disposal(install, observer)
            case["retained_directory"] = str(case_root)
            install.checkpoint()
        result["status"] = "gap_reproduced" if expect_gap else "recovery_passed_teardown_retained"
    except BaseException as error:
        result.update(status="failed_or_interrupted", error_type=type(error).__name__)
        try:
            result["failure_installation_snapshots"] = lifecycle.capture_failure_snapshots(installations)
        except BaseException:
            result["failure_installation_snapshots"] = {"status": "unavailable"}
        try:
            lifecycle.persist_checkpoint(output, result)
        except BaseException:
            pass  # A diagnostic checkpoint cannot replace the original failure.
        raise
    lifecycle.persist_checkpoint(output, result)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("archive", "receipt", "output", "controller", "fixture"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--topology", choices=("both", "managed", "manual"), default="both")
    parser.add_argument("--expect-gap", action="store_true")
    args = parser.parse_args()
    value = qualify(*(getattr(args, name).resolve() for name in ("archive", "receipt", "output", "controller", "fixture")),
                    topology=args.topology, expect_gap=args.expect_gap)
    print(json.dumps({"status": value["status"], "cases": list(value["cases"]),
                      "teardown_retained": True, "model_requests": 0}))
