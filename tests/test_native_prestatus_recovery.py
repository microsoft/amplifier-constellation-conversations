"""Controller-clock regression only; never invokes a native manager/process."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import uuid

SPEC = importlib.util.spec_from_file_location(
    "prestatus_fixture_under_test", Path(__file__).with_name("native_prestatus_recovery.py"))
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


class OriginalDisposalBudget(unittest.TestCase):
    def test_intent_flush_expiring_budget_never_dispatches_or_retries(self):
        with tempfile.TemporaryDirectory(prefix="prestatus-clock-only-") as directory:
            root = Path(directory)
            install = SimpleNamespace(root=root, home=root / "home", result={},
                                      controller_environment={}, unsettled_commands=[], checkpoint=Mock())
            observer = SimpleNamespace(controller=root / "never-executed-controller",
                                       baseline_startup={"registered": True})
            clock = [0.0]

            def slow_intent_flush(_fd):
                clock[0] = 31.0

            with patch.object(FIXTURE.time, "monotonic", side_effect=lambda: clock[0]), \
                 patch.object(FIXTURE.os, "fsync", side_effect=slow_intent_flush), \
                 patch.object(FIXTURE.subprocess, "Popen") as spawn:
                with self.assertRaisesRegex(Exception, "budget expired before dispatch"):
                    FIXTURE.remove_original_once(install, observer)
                spawn.assert_not_called()
                path = root / "original-startup-disposal.json"
                retained = path.read_bytes()
                receipt = json.loads(retained)
                self.assertEqual(receipt["status"], "unknown_or_refused")
                self.assertFalse(receipt["retry_permitted"])
                self.assertEqual(receipt["timeout_seconds"], 30)
                self.assertEqual(receipt["mutation_retries"], 0)
                self.assertEqual(receipt["elapsed_seconds"], 31.0)
                with self.assertRaisesRegex(Exception, "already attempted"):
                    FIXTURE.remove_original_once(install, observer)
                spawn.assert_not_called()
                self.assertEqual(path.read_bytes(), retained)


class FailureDiagnosticRetention(unittest.TestCase):
    SECRET = "private-value-must-not-be-exported"

    def fixture(self, directory):
        home = Path(directory).resolve()
        installation, request, check, transaction = (str(uuid.uuid4()) for _ in range(4))
        install = SimpleNamespace(home=home, result={}, checkpoint=Mock())
        observer = FIXTURE.FaultObserver.__new__(FIXTURE.FaultObserver)
        observer.diagnostic_request_id, observer.diagnostic_check_id = request, check
        observer.diagnostic_observations, observer.diagnostic_observation_count = [], 0
        observer.operation, observer.attempt, observer.transaction = request, request, transaction
        observer.worker_observation = None
        launch = {"schema": 1, "home": str(home), "installation_id": installation,
                  "identity": {"kind": "worker", "operation": request, "attempt": request},
                  "service": {"manager": "systemd_user", "name": "agent-conversations-worker-" + "a" * 64 + ".service",
                              "definition_path": self.SECRET},
                  "profile": {"autostart": False, "executable": self.SECRET,
                              "args": [self.SECRET], "cwd": self.SECRET}}
        attempt = {"id": request, "claimed": False, "launch": launch,
                   "handoff_nonce": self.SECRET, "worker_process": None}
        ledger = {"schema": 1, "installation_id": installation, "current_operation": request,
                  "requests": {request: {"operation_id": request, "attempt_id": request,
                                         "payload": {"kind": "apply", "check_id": check}}},
                  "operations": {request: {"id": request, "request_id": request,
                     "transaction_id": transaction, "kind": "apply", "state": "unknown", "outcome": None,
                     "attempt": attempt, "plan": {"private": self.SECRET}, "helper": self.SECRET}}}
        marker = {"prepared": launch, "manager_attempted": True,
                  "receipt": {"status": "unknown", "code": self.SECRET}}
        FIXTURE.lifecycle.write_json(home / "config.json", {"id": installation, "private": self.SECRET})
        FIXTURE.lifecycle.write_json(home / "native-browser-update/control.json", ledger)
        marker_path = home / ("native-browser-launch-" + launch["service"]["name"] + ".json")
        FIXTURE.lifecycle.write_json(marker_path, marker)
        return install, observer, ledger, marker_path

    def capture(self, install, observer):
        with patch.object(FIXTURE.consumer_update.NativeObserver, "call") as native, \
             patch.object(FIXTURE.consumer_update, "request_status") as http, \
             patch.object(FIXTURE.subprocess, "Popen") as spawn:
            value = FIXTURE.capture_fault_diagnostics(install, observer)
        native.assert_not_called()
        http.assert_not_called()
        spawn.assert_not_called()
        self.assertNotIn(self.SECRET, json.dumps(value))
        self.assertNotIn(str(install.home), json.dumps(value))
        return value

    def test_accepted_http_unknown_dispatch_before_journal_retains_no_effect_claim(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, _, _ = self.fixture(directory)
            install.result["request_dispatch"] = [{"http_status": 200, "mutation_retries": 0}]
            value = self.capture(install, observer)
            self.assertEqual(value["status"], "bound")
            self.assertEqual(value["browser"]["state"], "unknown")
            self.assertFalse(value["browser"]["claimed"])
            self.assertTrue(value["browser"]["launch_present"])
            self.assertTrue(value["dispatch"]["manager_attempted"])
            self.assertEqual(value["dispatch"]["receipt"]["status"], "unknown")
            self.assertEqual(value["dispatch"]["receipt"]["code"], "unknown")
            self.assertEqual(value["observer"]["last_worker"]["status"], "not_captured")
            self.assertFalse((install.home / "native-update.json").exists())
            self.assertNotIn("recovery_proof", install.result)

    def test_only_fixed_source_owned_launch_codes_are_retained_in_plain_text(self):
        for code in sorted(FIXTURE.DIAGNOSTIC_LAUNCH_CODES):
            result = FIXTURE.diagnostic_launch_receipt({"status": "unknown", "code": code})
            self.assertEqual(result["code"], code)
            self.assertEqual(result["code_sha256"], FIXTURE.fingerprint(code))
        result = FIXTURE.diagnostic_launch_receipt({"status": "refused", "code": self.SECRET})
        self.assertEqual(result["code"], "unknown")
        self.assertNotIn(self.SECRET, json.dumps(result))

    def test_claimed_running_is_not_health_or_effect_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, ledger, _ = self.fixture(directory)
            operation = ledger["operations"][observer.operation]
            operation.update(state="running")
            operation["attempt"].update(claimed=True, worker_process={"pid": 10, "birth": self.SECRET})
            FIXTURE.lifecycle.write_json(install.home / "native-browser-update/control.json", ledger)
            value = self.capture(install, observer)
            self.assertTrue(value["browser"]["claimed"])
            self.assertTrue(value["browser"]["worker_process_present"])
            self.assertEqual(value["browser"]["worker_process_sha256"],
                             FIXTURE.fingerprint(operation["attempt"]["worker_process"]))
            self.assertNotIn("health", value)

    def test_original_request_operation_attempt_and_observer_drift_are_unknown(self):
        for drift in ("installation", "current", "request", "attempt", "transaction", "observer", "check"):
            with self.subTest(drift=drift), tempfile.TemporaryDirectory() as directory:
                install, observer, ledger, _ = self.fixture(directory)
                different = str(uuid.uuid4())
                operation = ledger["operations"][observer.operation]
                if drift == "installation": ledger["installation_id"] = different
                if drift == "current": ledger["current_operation"] = different
                if drift == "request": operation["request_id"] = different
                if drift == "attempt": operation["attempt"]["id"] = different
                if drift == "transaction": operation["transaction_id"] = different
                if drift == "observer": observer.operation = different
                if drift == "check": ledger["requests"][observer.diagnostic_request_id]["payload"]["check_id"] = different
                FIXTURE.lifecycle.write_json(install.home / "native-browser-update/control.json", ledger)
                value = self.capture(install, observer)
                self.assertEqual(value["status"], "unknown")
                self.assertNotIn("browser", value)
                self.assertNotIn("dispatch", value)

    def test_marker_absence_is_distinct_from_unknown_ack(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, _, marker = self.fixture(directory)
            marker.unlink()
            value = self.capture(install, observer)
            self.assertEqual(value["status"], "bound")
            self.assertEqual(value["dispatch"], {"status": "absent"})
            self.assertNotIn("manager_attempted", value["dispatch"])

    def test_changed_or_malformed_dispatch_never_attributes_acceptance(self):
        for drift in ("prepared", "attempted", "receipt", "code", "contradiction"):
            with self.subTest(drift=drift), tempfile.TemporaryDirectory() as directory:
                install, observer, _, marker_path = self.fixture(directory)
                marker = FIXTURE.read_json(marker_path)
                if drift == "prepared": marker["prepared"]["installation_id"] = str(uuid.uuid4())
                if drift == "attempted": marker.pop("manager_attempted")
                if drift == "receipt": marker["receipt"] = {"status": "accepted", "message": self.SECRET}
                if drift == "code": marker["receipt"]["code"] = {"message": self.SECRET}
                if drift == "contradiction": marker.update(manager_attempted=False, receipt={"status": "accepted"})
                FIXTURE.lifecycle.write_json(marker_path, marker)
                value = self.capture(install, observer)
                self.assertEqual(value["status"], "unknown")
                self.assertNotIn("dispatch", value)

    def test_untrusted_worker_name_never_selects_another_file(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, ledger, _ = self.fixture(directory)
            ledger["operations"][observer.operation]["attempt"]["launch"]["service"]["name"] = "../../private"
            FIXTURE.lifecycle.write_json(install.home / "native-browser-update/control.json", ledger)
            with patch.object(FIXTURE, "diagnostic_json", wraps=FIXTURE.diagnostic_json) as reader:
                value = self.capture(install, observer)
            self.assertEqual(value["status"], "unknown")
            self.assertEqual([call.args[0].name for call in reader.call_args_list], ["config.json", "control.json"])

    def test_missing_malformed_oversized_and_permission_error_are_redacted(self):
        for mode in ("missing", "malformed", "oversized", "error"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                install, observer, _, _ = self.fixture(directory)
                ledger = install.home / "native-browser-update/control.json"
                if mode == "missing": ledger.unlink()
                if mode == "malformed": ledger.write_text(self.SECRET)
                if mode == "oversized": ledger.write_bytes(b"x" * (4 * 1024 * 1024 + 1))
                if mode == "error":
                    with patch.object(FIXTURE.os, "open", side_effect=PermissionError(self.SECRET)):
                        value = self.capture(install, observer)
                else:
                    value = self.capture(install, observer)
                self.assertEqual(value["status"], "unknown")
                self.assertNotIn("browser", value)

    def test_changed_binding_between_local_reads_retains_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, _, _ = self.fixture(directory)
            reader = FIXTURE.diagnostic_json
            reads = [0]
            def change(path, limit):
                status, value, digest = reader(path, limit)
                if path.name == "control.json":
                    reads[0] += 1
                    if reads[0] == 2: digest = "0" * 64
                return status, value, digest
            with patch.object(FIXTURE, "diagnostic_json", side_effect=change):
                value = self.capture(install, observer)
            self.assertEqual(value["status"], "unknown")
            self.assertNotIn("browser", value)

    def test_symlink_file_and_parent_are_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            target.write_text(json.dumps({"private": self.SECRET}))
            link = root / "link"
            try:
                link.symlink_to(target)
            except OSError:
                self.skipTest("This account cannot create a disposable symbolic link")
            self.assertEqual(FIXTURE.diagnostic_json(link, 1024)[0], "invalid")
            sub = root / "sub"
            sub.mkdir()
            (sub / "value").write_text("{}")
            parent_link = root / "parent-link"
            parent_link.symlink_to(sub, target_is_directory=True)
            self.assertEqual(FIXTURE.diagnostic_json(parent_link / "value", 1024)[0], "invalid")

    def test_fixture_observer_retains_unavailable_and_profile_mismatch_without_extra_calls(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, _, _ = self.fixture(directory)
            target = {"kind": "worker", "operation": observer.operation, "attempt": observer.attempt}
            for response in ({"status": "unavailable", "code": self.SECRET},
                             {"status": "observed", "profile_matches": False, "active": False,
                              "service_name": self.SECRET, "processes": []}):
                with patch.object(FIXTURE.consumer_update.NativeObserver, "call", return_value=response) as native:
                    self.assertIs(observer.call(target), response)
                native.assert_called_once_with(target)
            value = self.capture(install, observer)
            self.assertEqual(value["observer"]["count"], 2)
            self.assertEqual(value["observer"]["last"][0]["observation"]["status"], "unavailable")
            self.assertFalse(value["observer"]["last"][1]["observation"]["profile_matches"])
            self.assertEqual(value["observer"]["last_worker"]["status"], "not_captured")

    def test_native_not_dispatched_and_optional_null_are_distinct_from_false(self):
        self.assertEqual(FIXTURE.diagnostic_worker_observation({"status": "not_dispatched"}),
                         {"status": "not_dispatched"})
        result = FIXTURE.diagnostic_worker_observation({
            "status": "observed", "profile_matches": False, "active": False, "transitioning": False,
            "enabled": None, "masked": None, "definition_matches": None,
            "actual_worker_alive": None, "actual_worker_identity": None,
            "actual_worker_manager_binding": None, "processes": [], "launch_receipt": None})
        self.assertEqual(result["status"], "observed")
        for key in ("enabled", "masked", "definition_matches", "actual_worker_alive"):
            self.assertIsNone(result[key])
        self.assertEqual(result["launch_receipt"], {"status": "not_recorded"})

    def test_retained_observations_are_bounded_and_wrong_attempt_is_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer, _, _ = self.fixture(directory)
            target = {"kind": "worker", "operation": observer.operation, "attempt": observer.attempt}
            for _ in range(20): observer.retain_inspection(target, {"status": "unavailable", "code": self.SECRET})
            value = self.capture(install, observer)
            self.assertEqual(value["observer"]["count"], 20)
            self.assertEqual(len(value["observer"]["last"]), 8)
            observer.retain_inspection({**target, "attempt": str(uuid.uuid4())}, {"status": "unavailable"})
            self.assertEqual(self.capture(install, observer)["status"], "unknown")

    def test_native_inspection_exception_is_preserved_and_recorded(self):
        with tempfile.TemporaryDirectory() as directory:
            _, observer, _, _ = self.fixture(directory)
            original = AssertionError("original fixture observation failed")
            target = {"kind": "worker", "operation": observer.operation, "attempt": observer.attempt}
            with patch.object(FIXTURE.consumer_update.NativeObserver, "call", side_effect=original) as native:
                with self.assertRaises(AssertionError) as caught: observer.call(target)
            self.assertIs(caught.exception, original)
            self.assertEqual(observer.diagnostic_observation_count, 1)
            native.assert_called_once_with(target)

    def test_original_assertion_survives_diagnostic_and_checkpoint_failures(self):
        for failure in ("capture", "checkpoint", "retainer"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                install, observer, _, _ = self.fixture(directory)
                original = AssertionError("Real pre-status process boundary was not observed")
                if failure == "checkpoint": install.checkpoint.side_effect = RuntimeError(self.SECRET)
                target = "retain_fault_diagnostics" if failure == "retainer" else "capture_fault_diagnostics"
                kwargs = {"side_effect": RuntimeError(self.SECRET)} if failure != "checkpoint" else {
                    "return_value": {"schema": 1, "status": "unknown"}}
                with patch.object(FIXTURE, "exercise_fault_inner", side_effect=original), \
                     patch.object(FIXTURE, target, **kwargs):
                    with self.assertRaises(AssertionError) as caught:
                        FIXTURE.exercise_fault(install, observer, None, None, "hash", expect_gap=False)
                self.assertIs(caught.exception, original)
                self.assertNotIn(self.SECRET, json.dumps(install.result))

    def test_outer_snapshot_and_persistence_failure_do_not_replace_original(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            archive, receipt, output, controller, app = (home / name for name in ("archive", "receipt", "output", "controller", "app"))
            for executable in (controller, app):
                executable.write_bytes(b"not executed")
                executable.chmod(0o700)
            FIXTURE.lifecycle.write_json(receipt, {"archive_sha256": "unused", "manifest_sha256": FIXTURE.lifecycle.digest(b"{}")})
            manifest = {"target": "fixture", "source_revision": "a" * 40}
            package = SimpleNamespace(inspect_archive=Mock(return_value=(manifest, {"manifest.json": b"{}"})))
            root = home / "captured-fixture-root"
            root.mkdir()
            original = AssertionError("original extraction failure")
            with patch.dict(FIXTURE.os.environ, {"CONSTELLATION_NATIVE_STARTUP_TEST": "1"}), \
                 patch.object(FIXTURE.lifecycle.clean, "package_module", return_value=package), \
                 patch.object(FIXTURE.lifecycle.clean, "native_target", return_value="fixture"), \
                 patch.object(FIXTURE.tempfile, "mkdtemp", return_value=str(root)), \
                 patch.object(FIXTURE.lifecycle, "extract", side_effect=original), \
                 patch.object(FIXTURE.lifecycle, "capture_failure_snapshots", side_effect=RuntimeError(self.SECRET)), \
                 patch.object(FIXTURE.lifecycle, "persist_checkpoint", side_effect=RuntimeError(self.SECRET)), \
                 patch.object(FIXTURE.subprocess, "Popen") as spawn:
                with self.assertRaises(AssertionError) as caught:
                    FIXTURE.qualify(archive, receipt, output, controller, app)
            self.assertIs(caught.exception, original)
            spawn.assert_not_called()


if __name__ == "__main__":
    unittest.main()
