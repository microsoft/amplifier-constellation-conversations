"""Offline worker-observation diagnostics; no controller, manager or app runs."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("worker_observation_controller", Path(__file__).with_name("native_consumer_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


class WorkerObservationTests(unittest.TestCase):
    def fixture(self, directory):
        home = Path(directory)
        ledger = home / "native-browser-update/control.json"
        ledger.parent.mkdir()
        ledger.write_text(json.dumps({"current_operation": "private-operation", "operations": {"private-operation": {
            "id": "private-operation", "transaction_id": "private-transaction", "attempt": {"id": "private-attempt", "claimed": True}}}}))
        install = SimpleNamespace(home=home, result={}, checkpoint=Mock())
        observer = object.__new__(controller.NativeObserver)
        observer.install = install
        observer.previous_operation = observer.operation = observer.attempt = observer.transaction = None
        observer.worker_generation = observer.actual_worker_generation = observer.worker_observation = None
        observer.old_identity = {"pid": 1, "birth": "private-old-birth"}
        observer.old_exit = observer.survived_old_exit = False
        observer.observations = []
        worker = {"status": "observed", "profile_matches": True, "active": True, "transitioning": False,
                  "processes": [{"identity": {"pid": 2, "birth": "private-engine-birth"}}], "previous_process_alive": False,
                  "actual_worker_identity": {"pid": 3, "birth": "private-worker-birth"},
                  "actual_worker_alive": True, "actual_worker_manager_binding": "descendant_of_task_engine"}
        observer.call = Mock(return_value=worker)
        return observer, ledger, worker

    def timed_failure(self, observer):
        with patch.object(controller.time, "monotonic", side_effect=[0, 0, 40]), \
             patch.object(controller.time, "sleep"), patch.object(controller.subprocess, "Popen") as spawn:
            with self.assertRaisesRegex(TimeoutError, "Actual worker exit is unobserved") as caught:
                observer.settled({"id": "private-operation", "transaction_id": "private-transaction"}, {})
            spawn.assert_not_called()
        return caught.exception

    def test_unavailable_receipt_preserves_fixed_failure_stage_before_original_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, _ = self.fixture(directory)
            observer.call.return_value = {"status": "unavailable", "code": "native_controller_observation_or_dispatch_failed",
                                          "diagnostic_stage": "runtime_binding", "error": "private error path"}
            self.timed_failure(observer)
            row = observer.install.result["worker_observation_failure"]
            self.assertFalse(row["proof_complete"])
            self.assertEqual((row["settlement_timeout_seconds"], row["controller_timeout_seconds"]), (40, 45))
            self.assertEqual(row["counts"], {"controller_unavailable": 1})
            self.assertEqual(row["history"][-1]["worker"], {"status": "unavailable", "code": "native_controller_observation_or_dispatch_failed", "diagnostic_stage": "runtime_binding"})
            self.assertGreaterEqual(row["history"][-1]["elapsed_seconds"], 0)
            observer.install.checkpoint.assert_called_once()
            observer.call.assert_called_once_with({"kind": "worker", "operation": "private-operation", "attempt": "private-attempt"}, previous=observer.old_identity)
            self.assertIsNone(observer.worker_observation)

    def test_live_pending_worker_does_not_become_exit_and_saves_only_identity_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, _ = self.fixture(directory)
            self.timed_failure(observer)
            row = observer.install.result["worker_observation_failure"]
            worker = row["history"][-1]["worker"]
            self.assertTrue(worker["active"] and worker["actual_worker_alive"])
            self.assertEqual(worker["process_count"], 1)
            self.assertEqual(len(worker["actual_worker_identity_sha256"]), 64)
            self.assertEqual(row["counts"], {"observed": 1})
            self.assertNotIn("private-", json.dumps(row))
            self.assertNotIn('"pid"', json.dumps(row))
            self.assertEqual(len(observer.observations), 1)  # Existing success history is retained.
            self.assertTrue(observer.old_exit and observer.survived_old_exit)

    def test_profile_mismatch_and_unavailable_ledger_are_recorded_without_proof(self):
        for kind in ["profile", "ledger"]:
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                observer, _, worker = self.fixture(directory)
                if kind == "profile":
                    observer.call.return_value = {**worker, "profile_matches": False, "active": False}
                    self.timed_failure(observer)
                else:
                    with patch.object(controller, "read_json", side_effect=PermissionError("private path")):
                        self.timed_failure(observer)
                    observer.call.assert_not_called()
                row = observer.install.result["worker_observation_failure"]
                self.assertEqual(row["counts"], {"profile_mismatch" if kind == "profile" else "ledger_unavailable": 1})
                self.assertIsNone(observer.worker_observation)
                self.assertEqual(observer.observations, [])

    def test_summary_rejects_raw_fields_and_unrecognized_stage_or_code(self):
        for stage in [*controller.WORKER_OBSERVATION_STAGES, None, "private arbitrary path"]:
            with self.subTest(stage=stage):
                value = {"status": "unavailable", "code": "private error", "diagnostic_stage": stage,
                         "service_name": "private service", "launch_receipt": {"path": "private path"},
                         "active": "private text", "actual_worker_identity": {"secret": "private contents"},
                         "processes": [{"identity": {"secret": "private identity"}}]}
                row = controller._worker_observation_summary(value)
                self.assertEqual(row["diagnostic_stage"], stage if stage in controller.WORKER_OBSERVATION_STAGES else "unrecognized")
                self.assertEqual(row["code"], "unrecognized")
                self.assertIsNone(row["active"])
                self.assertNotIn("private", json.dumps(row))
                self.assertNotIn("service_name", row)
        self.assertNotIn("diagnostic_stage", controller._worker_observation_summary({"status": "unavailable"}))

    def test_history_is_bounded_in_memory_with_no_sample_checkpoint(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, _ = self.fixture(directory)
            observer.call.return_value = {"status": "unavailable", "code": "controller_observation_timeout"}
            for _ in range(30):
                self.assertFalse(observer.sample())
            state = observer.worker_diagnostics
            self.assertEqual(state["samples"], 30)
            self.assertEqual(len(state["history"]), controller.WORKER_OBSERVATION_HISTORY)
            self.assertEqual(state["omitted_samples"], 22)
            self.assertLess(len(json.dumps(state).encode()), 32768)
            observer.install.checkpoint.assert_not_called()
            self.timed_failure(observer)
            observer.install.checkpoint.assert_called_once()
            self.assertEqual(observer.install.result["worker_observation_failure"]["samples"], 31)

    def test_checkpoint_failure_or_interrupt_preserves_original_timeout(self):
        for error in [OSError("private checkpoint"), KeyboardInterrupt(), SystemExit(9)]:
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as directory:
                observer, _, _ = self.fixture(directory)
                observer.call.return_value = {"status": "unavailable"}
                observer.install.checkpoint.side_effect = error
                self.timed_failure(observer)
                observer.install.checkpoint.assert_called_once()
                self.assertFalse(observer.install.result["worker_observation_failure"]["proof_complete"])

    def test_original_cancellation_before_controller_is_not_swallowed(self):
        for cancellation in [KeyboardInterrupt(), SystemExit(7)]:
            with self.subTest(error=type(cancellation).__name__), tempfile.TemporaryDirectory() as directory:
                observer, _, _ = self.fixture(directory)
                observer.install.checkpoint.side_effect = SystemExit(99)
                with patch.object(controller, "read_json", side_effect=cancellation):
                    with self.assertRaises(type(cancellation)) as caught:
                        observer.settled({}, {})
                self.assertIs(caught.exception, cancellation)
                observer.call.assert_not_called()
                self.assertEqual(observer.install.result["worker_observation_failure"]["history"][-1]["sample_stage"], "ledger")

    def test_post_sample_cancellation_propagates_before_replacement_or_other_action(self):
        for cancellation in [KeyboardInterrupt(), SystemExit(7)]:
            with self.subTest(error=type(cancellation).__name__), tempfile.TemporaryDirectory() as directory:
                observer, _, worker = self.fixture(directory)
                observer.call.return_value = {**worker, "active": False, "processes": [], "actual_worker_alive": False}
                with patch.object(controller, "_worker_observation_summary", side_effect=cancellation):
                    with self.assertRaises(type(cancellation)) as caught:
                        observer.settled({}, {})
                self.assertIs(caught.exception, cancellation)
                observer.call.assert_called_once()  # No replacement or mutation follows.
                observer.install.checkpoint.assert_called_once()

    def test_optional_projection_error_does_not_change_normal_sample(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, worker = self.fixture(directory)
            with patch.object(controller, "_worker_observation_summary", side_effect=ValueError("private")):
                self.assertTrue(observer.sample())
            self.assertEqual(observer.worker_observation, worker)
            self.assertEqual(observer.observations, [controller.public_observation(worker)])
            observer.install.checkpoint.assert_not_called()

    def test_settled_success_returns_original_receipt_without_failure_write(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, _ = self.fixture(directory)
            expected = {"worker_observations": [{"original": "unchanged"}], "worker_exit_observed": True}
            observer._settled = Mock(return_value=expected)
            self.assertIs(observer.settled({"operation": "fixed"}, {"running": "fixed"}), expected)
            observer._settled.assert_called_once_with({"operation": "fixed"}, {"running": "fixed"})
            observer.install.checkpoint.assert_not_called()
            self.assertNotIn("worker_observation_failure", observer.install.result)


if __name__ == "__main__":
    unittest.main()
