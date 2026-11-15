"""Offline fixture-process seams only; no app, manager, HTTP or installation run."""
import importlib.util
import inspect
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("managed_setup_controller", Path(__file__).with_name("native_consumer_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


def phase(name="bootstrap_verify", state="started", **extra):
    return json.dumps({"event": "native_installation_phase", "phase": name, "state": state, **extra}).encode() + b"\n"


class ManagedSetupObservationTests(unittest.TestCase):
    def fixture(self, directory):
        install = SimpleNamespace(home=Path(directory) / "absent-home", root=Path(directory), env={},
                                  result={}, checkpoint=Mock(), unsettled_commands=[])
        observer = object.__new__(controller.NativeObserver)
        observer.install, observer.controller = install, Path(directory) / "unexecuted-controller"
        return install, observer

    def child(self, steps):
        child = Mock()
        child.pid, child.returncode = 321, None
        steps = iter(steps)
        def communicate(*_args, **_kwargs):
            code, value = next(steps)
            child.returncode = code
            if isinstance(value, BaseException):
                raise value
            return value
        child.communicate.side_effect = communicate
        return child

    def call(self, observer, action="prepare_managed"):
        return observer.call({"kind": "original"}, action=action, setup_bundle=observer.install.root / "bundle" if action == "prepare_managed" else None)

    def test_prepare_reuses_existing_120_installation_budget_other_actions_keep_45(self):
        self.assertEqual(inspect.signature(controller.lifecycle.Installation.command).parameters["timeout"].default, 120)
        for action in ["prepare_managed", "inspect", "observe_retirement", "stop"]:
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                install, observer = self.fixture(directory)
                reply = {"status": "prepared", "service_started": False} if action == "prepare_managed" else {"status": "observed", "dispatch": {"status": "accepted"}}
                child = self.child([(0, (json.dumps(reply).encode(), b""))])
                with patch.object(controller.subprocess, "Popen", return_value=child) as spawn:
                    self.assertEqual(self.call(observer, action), reply)
                self.assertEqual(child.communicate.call_args.kwargs["timeout"], 120 if action == "prepare_managed" else 45)
                spawn.assert_called_once()
                child.kill.assert_not_called()
                self.assertEqual(controller.CHECK_REPLY_SECONDS, 40)
                if action == "prepare_managed":
                    row = install.result["managed_setup_observation"]
                    self.assertEqual(row["previous_observer_timeout_seconds"], 45)
                    self.assertEqual(row["timeout_seconds"], 120)
                    self.assertEqual(row["outcome"], "returned")
                    self.assertTrue(row["exit_observed"])
                    self.assertFalse(row["pid_is_authority"])
                else:
                    self.assertNotIn("managed_setup_observation", install.result)

    def test_unavailable_is_retained_before_unchanged_desktop_assertion_without_start(self):
        with tempfile.TemporaryDirectory() as directory:
            install, _observer = self.fixture(directory)
            install.managed, install.bundle = True, Path(directory) / "bundle"
            install.controller = Path(directory) / "controller"
            reply = {"status": "unavailable", "code": "native_controller_observation_or_dispatch_failed"}
            child = self.child([(1, (json.dumps(reply).encode(), phase()))])
            with patch.object(controller.NativeObserver, "__init__", lambda own, current, executable, **_: (setattr(own, "install", current), setattr(own, "controller", executable))[-1]), \
                 patch.object(controller.subprocess, "Popen", return_value=child) as spawn, \
                 patch.object(controller, "managed_parent") as start, \
                 self.assertRaisesRegex(AssertionError, "Fresh managed setup was not confirmed; preserve fixture without retry"):
                controller.UpdateInstallation.desktop_start(install)
            start.assert_not_called()
            spawn.assert_called_once()
            row = install.result["managed_setup_observation"]
            self.assertEqual(row["returned"], reply)
            self.assertEqual(row["outcome"], "returned_unknown_or_refused")
            self.assertEqual(row["return_code"], 1)
            self.assertEqual(row["installation_phases"]["events"][0]["phase"], "bootstrap_verify")
            self.assertNotIn("setup_receipt", install.result)

    def test_timeout_keeps_partial_phase_and_reap_without_retry_or_late_success(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer = self.fixture(directory)
            progress = phase("bundle_copy")
            timeout = controller.subprocess.TimeoutExpired(["private-argv"], 120, stderr=progress)
            # Reaping can observe a late clean exit/prepared response. The
            # original timed-out action must still return unavailable.
            child = self.child([(None, timeout), (0, (b'{"status":"prepared","service_started":false}', progress))])
            with patch.object(controller.subprocess, "Popen", return_value=child) as spawn:
                reply = self.call(observer)
            self.assertEqual(reply, {"status": "unavailable", "code": "controller_observation_timeout"})
            self.assertEqual([call.kwargs["timeout"] for call in child.communicate.call_args_list], [120, 10])
            spawn.assert_called_once()
            child.kill.assert_called_once()
            self.assertEqual(install.unsettled_commands, [])
            row = install.result["managed_setup_observation"]
            self.assertEqual(row["outcome"], "timeout_unknown")
            self.assertEqual(row["kill"], "returned")
            self.assertEqual(row["reap"], "communicate_returned")
            self.assertEqual(row["return_code"], 0)
            self.assertEqual(row["installation_phases"]["events"][0]["phase"], "bundle_copy")
            self.assertEqual(row["mutation_retries"], 0)
            self.assertNotIn("private-argv", json.dumps(row))

    def test_unsettled_drain_retains_initial_partial_phase_and_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer = self.fixture(directory)
            first = controller.subprocess.TimeoutExpired("private-command", 120, stderr=phase())
            second = controller.subprocess.TimeoutExpired("private-command", 10)
            child = self.child([(None, first), (None, second)])
            with patch.object(controller.subprocess, "Popen", return_value=child):
                self.call(observer)
            self.assertEqual(install.unsettled_commands, [child])
            row = install.result["managed_setup_observation"]
            self.assertEqual(row["reap"], "unknown_after_drain")
            self.assertFalse(row["exit_observed"])
            self.assertEqual(len(row["installation_phases"]["events"]), 1)
            child.kill.assert_called_once()

    def test_kill_failure_is_retained_without_replacing_the_original_exception(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer = self.fixture(directory)
            original = OSError("private kill error")
            timeout = controller.subprocess.TimeoutExpired("private-command", 120, stderr=phase())
            child = self.child([(None, timeout)])
            child.kill.side_effect = original
            with patch.object(controller.subprocess, "Popen", return_value=child), self.assertRaises(OSError) as caught:
                self.call(observer)
            self.assertIs(caught.exception, original)
            self.assertEqual(install.unsettled_commands, [child])
            row = install.result["managed_setup_observation"]
            self.assertEqual(row["outcome"], "timeout_unknown")
            self.assertEqual(row["kill"], "failed_or_interrupted")
            self.assertNotIn("private", json.dumps(row))

    def test_projection_is_bounded_and_excludes_raw_errors_unknown_fields_and_invalid_values(self):
        private = b"credential-secret C:\\private\\home\n"
        malformed = [phase(phase=["private"]), phase("private-path"),
                     phase(private_field="credential-secret"), phase(state="returned", result="ok", elapsed_ms=True),
                     phase(state="returned", result=["private"], elapsed_ms=2)]
        valid = phase("bundle_copy", "returned", elapsed_ms=20, result="error")
        data = private + b"{" * 40000 + b"\n" + b"".join(malformed) + valid * 80
        projected = controller._managed_setup_phases(data)
        self.assertTrue(projected["input_truncated"])
        self.assertEqual(len(projected["events"]), 64)
        self.assertEqual(projected["events_omitted"], 16)
        self.assertTrue(all(row["result"] == "error" for row in projected["events"]))
        self.assertNotIn("private", json.dumps(projected))
        self.assertNotIn("credential-secret", json.dumps(projected))
        self.assertEqual(controller._managed_setup_return({"status": ["secret"], "code": {"secret": 1}}),
                         {"status": "unrecognized", "code": "unrecognized"})

    def test_diagnostic_failures_do_not_change_success_unavailable_or_pending_error(self):
        for reply in [{"status": "prepared", "service_started": False}, {"status": "unavailable", "code": "native_controller_observation_or_dispatch_failed"}]:
            with self.subTest(reply=reply), tempfile.TemporaryDirectory() as directory:
                install, observer = self.fixture(directory)
                install.checkpoint.side_effect = RuntimeError("private diagnostic error")
                child = self.child([(0, (json.dumps(reply).encode(), phase()))])
                with patch.object(controller.subprocess, "Popen", return_value=child):
                    self.assertEqual(self.call(observer), reply)
        with tempfile.TemporaryDirectory() as directory:
            install, observer = self.fixture(directory)
            original = OSError("original transport error")
            child = self.child([(None, original)])
            with patch.object(controller.subprocess, "Popen", return_value=child), \
                 patch.object(controller, "_managed_setup_phases", side_effect=[{"events": []}, KeyboardInterrupt()]), \
                 self.assertRaises(OSError) as caught:
                self.call(observer)
            self.assertIs(caught.exception, original)
            self.assertEqual(install.unsettled_commands, [child])
        with tempfile.TemporaryDirectory() as directory:
            install, observer = self.fixture(directory)
            original = OSError("original transport error before checkpoint failure")
            install.checkpoint.side_effect = [None, RuntimeError("private checkpoint error")]
            child = self.child([(None, original)])
            with patch.object(controller.subprocess, "Popen", return_value=child), self.assertRaises(OSError) as caught:
                self.call(observer)
            self.assertIs(caught.exception, original)
            self.assertEqual(install.unsettled_commands, [child])
            self.assertEqual(install.result["managed_setup_observation"]["outcome"], "interrupted_or_failed")

    def test_cancellation_before_spawn_or_after_success_propagates_without_another_action(self):
        for interruption in [KeyboardInterrupt(), SystemExit(7)]:
            for after in [False, True]:
                with self.subTest(kind=type(interruption).__name__, after=after), tempfile.TemporaryDirectory() as directory:
                    install, observer = self.fixture(directory)
                    install.checkpoint.side_effect = [None, interruption] if after else interruption
                    child = self.child([(0, (b'{"status":"prepared","service_started":false}', phase()))])
                    with patch.object(controller.subprocess, "Popen", return_value=child) as spawn, self.assertRaises(type(interruption)) as caught:
                        self.call(observer)
                    self.assertIs(caught.exception, interruption)
                    self.assertEqual(spawn.call_count, 1 if after else 0)
                    child.kill.assert_not_called()

    def test_cancellation_during_communicate_preserves_unknown_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            install, observer = self.fixture(directory)
            interruption = KeyboardInterrupt()
            child = self.child([(None, interruption)])
            with patch.object(controller.subprocess, "Popen", return_value=child) as spawn, self.assertRaises(KeyboardInterrupt) as caught:
                self.call(observer)
            self.assertIs(caught.exception, interruption)
            self.assertEqual(install.unsettled_commands, [child])
            self.assertEqual(install.result["managed_setup_observation"]["outcome"], "interrupted_or_failed")
            spawn.assert_called_once()
            child.kill.assert_not_called()


if __name__ == "__main__":
    unittest.main()
