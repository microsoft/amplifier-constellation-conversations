"""Deterministic controller boundaries; no app, manager, browser or server runs."""
import errno
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("native_single_update", Path(__file__).with_name("native_single_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


class AssistantClosureTests(unittest.TestCase):
    def fixture(self, home):
        saved = {"schema": 1, "installation_id": "disposable-installation", "port": 31471,
                 "nonce": "private-assistant-nonce", "generation": {"pid": 123, "birth": "exact"}}
        (home / "native-browser-update").mkdir()
        (home / "native-browser-update/assistant-control.json").write_text(json.dumps(saved))
        (home / "native-update-assistant.lock").write_bytes(b"unchanged-assistant-lock")
        (home / "service.lock").write_bytes(b"unchanged-installed-service-lock")
        install = SimpleNamespace(home=home, result={"assistant": {"cleanup": {}}}, checkpoint=Mock(),
            service_lock_available=Mock(side_effect=AssertionError("Do not acquire the running application's lock")))
        install.assistant_lock_identity = controller.ordinary_identity(home / "native-update-assistant.lock")
        install.assistant_home_identity = controller.ordinary_identity(home, directory=True)
        return install, saved

    def connection(self, code, result=0):
        value = Mock()
        value.connect_ex.return_value = code
        value.getsockopt.return_value = result
        return value

    def row(self, install):
        return install.result["assistant"]["cleanup"]["listener_closure"]

    def test_explicit_posix_or_windows_refusal_requires_actual_own_lock_and_leaves_service_lock(self):
        for code in [errno.ECONNREFUSED, 10061]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                install, saved = self.fixture(Path(directory))
                connection = self.connection(code)
                with patch.object(controller.socket, "socket", return_value=connection):
                    result = controller.await_assistant_stopped(install, saved)
                self.assertEqual(result["status"], "stopped")
                self.assertTrue(result["listener_refused"] and result["assistant_lock_released"])
                self.assertEqual(result["connect_errno"], code)
                self.assertEqual(result["authenticated_requests"], 0)
                self.assertEqual((install.home / "native-update-assistant.lock").read_bytes(), b"unchanged-assistant-lock")
                self.assertEqual((install.home / "service.lock").read_bytes(), b"unchanged-installed-service-lock")
                install.service_lock_available.assert_not_called()
                connection.connect_ex.assert_called_once_with(("127.0.0.1", saved["port"]))
                connection.setblocking.assert_called_once_with(False)

    def test_pending_windows_connect_is_retained_until_delayed_native_refusal(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            connection, clock, polls = self.connection(10035, 10061), [0.0], []
            def ready(readable, writable, exceptional, timeout):
                self.assertEqual((readable, writable, exceptional), ([], [connection], [connection]))
                self.assertTrue(0 < timeout <= 0.2)
                connection.close.assert_not_called()
                clock[0] += timeout
                polls.append(timeout)
                return ([], [], [connection]) if len(polls) == 3 else ([], [], [])
            with patch.object(controller.socket, "socket", return_value=connection) as create, \
                 patch.object(controller.select, "select", side_effect=ready), \
                 patch.object(controller.time, "monotonic", side_effect=lambda: clock[0]):
                result = controller.await_assistant_stopped(install, saved)
            self.assertEqual(result["status"], "stopped")
            self.assertEqual(result["connection_attempts"], 1)
            self.assertEqual(result["poll_slices"], 3)
            create.assert_called_once()
            connection.connect_ex.assert_called_once()
            connection.getsockopt.assert_called_once_with(controller.socket.SOL_SOCKET, controller.socket.SO_ERROR)
            connection.close.assert_called_once()

    def test_persistent_pending_retains_unknown_at_original_fifteen_seconds(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            connection, clock = self.connection(10035), [0.0]
            def pending(_read, _write, _exception, timeout):
                self.assertTrue(0 < timeout <= min(0.2, 15 - clock[0]))
                clock[0] += timeout
                return [], [], []
            with patch.object(controller.socket, "socket", return_value=connection) as create, \
                 patch.object(controller.select, "select", side_effect=pending), \
                 patch.object(controller.time, "monotonic", side_effect=lambda: clock[0]), \
                 self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                controller.await_assistant_stopped(install, saved)
            row = self.row(install)
            self.assertEqual((row["status"], row["listener"], row["elapsed_seconds"]), ("unknown", "pending", 15))
            self.assertNotIn("listener_refused", row)
            self.assertNotIn("assistant_lock_released", row)
            create.assert_called_once()
            connection.connect_ex.assert_called_once()
            connection.getsockopt.assert_not_called()
            connection.close.assert_called_once()

    def test_completed_open_gets_new_probe_but_pending_does_not(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            first, second = self.connection(10035, 0), self.connection(10061)
            with patch.object(controller.socket, "socket", side_effect=[first, second]), \
                 patch.object(controller.select, "select", return_value=([], [first], [])), \
                 patch.object(controller.time, "sleep"):
                result = controller.await_assistant_stopped(install, saved)
            self.assertEqual(result["connection_attempts"], 2)
            first.connect_ex.assert_called_once()
            second.connect_ex.assert_called_once()
            first.close.assert_called_once()
            second.close.assert_called_once()

    def test_refused_socket_cannot_reuse_old_sample_after_lock_becomes_free(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            first, second = self.connection(10061), self.connection(10061)
            with patch.object(controller.socket, "socket", side_effect=[first, second]), \
                 patch.object(controller, "assistant_lock_available", side_effect=[False, True]), \
                 patch.object(controller.time, "sleep"):
                result = controller.await_assistant_stopped(install, saved)
            self.assertEqual(result["connection_attempts"], 2)
            first.connect_ex.assert_called_once()
            second.connect_ex.assert_called_once()

    def test_socket_error_or_ambiguous_readiness_is_unknown_not_absence(self):
        cases = [(10013, None, 0), (10060, None, 0), (10035, "exception", 0),
                 (10035, "write", 10054), (10035, "exception", 10035)]
        for initial, ready, result in cases:
            with self.subTest(initial=initial, ready=ready, result=result), tempfile.TemporaryDirectory() as directory:
                install, saved = self.fixture(Path(directory))
                connection = self.connection(initial, result)
                with patch.object(controller.socket, "socket", return_value=connection), \
                     patch.object(controller.select, "select", return_value=([], [connection] if ready == "write" else [], [connection] if ready == "exception" else [])), \
                     self.assertRaisesRegex(AssertionError, "unknown after socket"):
                    controller.await_assistant_stopped(install, saved)
                self.assertEqual(self.row(install)["status"], "unknown")
                self.assertNotIn("listener_refused", self.row(install))
                connection.close.assert_called_once()

    def test_changed_receipt_or_lock_during_pending_observation_refuses(self):
        for changed in ["receipt", "lock"]:
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory:
                install, saved = self.fixture(Path(directory))
                connection = self.connection(10035)
                def drift(*_args):
                    if changed == "receipt":
                        (install.home / "native-browser-update/assistant-control.json").write_text(json.dumps({**saved, "nonce": "different"}))
                    else:
                        path = install.home / "native-update-assistant.lock"
                        path.rename(install.home / "old-lock")
                        path.write_bytes(b"changed")
                    return [], [], []
                with patch.object(controller.socket, "socket", return_value=connection), \
                     patch.object(controller.select, "select", side_effect=drift), \
                     self.assertRaisesRegex(AssertionError, "identity changed"):
                    controller.await_assistant_stopped(install, saved)
                self.assertEqual(self.row(install)["status"], "unknown")
                self.assertNotIn("listener_refused", self.row(install))

    def test_missing_lock_is_not_free_and_never_created(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            path = install.home / "native-update-assistant.lock"
            path.unlink()  # Disposable test input only; controller never does this.
            with patch.object(controller.socket, "socket") as create, self.assertRaises(FileNotFoundError):
                controller.await_assistant_stopped(install, saved)
            create.assert_not_called()
            self.assertFalse(path.exists())
            self.assertEqual(self.row(install)["status"], "unknown")

    def test_lock_opened_handle_must_match_saved_path_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            install, _saved = self.fixture(Path(directory))
            path = install.home / "native-update-assistant.lock"
            replacement = install.home / "different-lock"
            replacement.write_bytes(b"different")
            original_open = Path.open
            def redirected(target, *args, **kwargs):
                return original_open(replacement if target == path else target, *args, **kwargs)
            with patch.object(Path, "open", redirected), self.assertRaisesRegex(AssertionError, "changed before acquisition"):
                controller.assistant_lock_available(install.home, install.assistant_lock_identity)
            self.assertEqual(path.read_bytes(), b"unchanged-assistant-lock")
            self.assertEqual(replacement.read_bytes(), b"different")

    def test_generation_unknown_prevents_socket_success_from_manufacturing_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            child, observer = Mock(returncode=0), Mock()
            observer.call.return_value = {"previous_process_alive": None}
            with patch.object(controller.socket, "socket") as create, self.assertRaisesRegex(AssertionError, "generation absence"):
                controller.verify_assistant_exit(install, child, saved, observer)
            create.assert_not_called()
            self.assertEqual(install.result["assistant"]["cleanup"]["status"], "unknown")
            observer.call.assert_called_once_with({"kind": "original"}, previous=saved["generation"])

    def test_supported_close_normal_return_generation_and_listener_lock_all_required(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            child, observer = Mock(returncode=0), Mock()
            observer.call.return_value = {"previous_process_alive": False}
            with patch.object(controller.socket, "socket", return_value=self.connection(10061)):
                controller.verify_assistant_exit(install, child, saved, observer)
            row = install.result["assistant"]["cleanup"]
            self.assertEqual(row["status"], "stopped")
            self.assertTrue(all(row[k] for k in ["normal_exit", "exact_generation_absent", "listener_refused", "assistant_lock_released"]))
            child.wait.assert_called_once_with(timeout=12)
            child.kill.assert_not_called()
            child.terminate.assert_not_called()

    def test_interrupt_during_observation_is_preserved_as_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            install, saved = self.fixture(Path(directory))
            connection = self.connection(10035)
            original = KeyboardInterrupt()
            with patch.object(controller.socket, "socket", return_value=connection), \
                 patch.object(controller.select, "select", side_effect=original), self.assertRaises(KeyboardInterrupt) as caught:
                controller.await_assistant_stopped(install, saved)
            self.assertIs(caught.exception, original)
            self.assertEqual(self.row(install)["status"], "unknown")
            connection.close.assert_called_once()


class ArtifactPreflightTests(unittest.TestCase):
    def test_fresh_exclusive_directory_is_bound_existing_captures_are_never_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve() / "captures"
            identity = controller.fresh_browser_artifacts(path)
            self.assertEqual(controller.artifact_identity(path), identity)
            sentinel = path / "original.png"
            sentinel.write_bytes(b"original")
            with self.assertRaises(FileExistsError):
                controller.fresh_browser_artifacts(path)
            self.assertEqual(sentinel.read_bytes(), b"original")

    def test_relative_path_and_symlink_ancestor_refuse_without_creating_captures(self):
        with self.assertRaisesRegex(AssertionError, "absolute"):
            controller.fresh_browser_artifacts(Path("relative-captures"))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            actual, alias = root / "actual", root / "alias"
            actual.mkdir()
            try:
                alias.symlink_to(actual, target_is_directory=True)
            except OSError:
                self.skipTest("Native symlink creation unavailable for this unit fixture")
            with self.assertRaisesRegex(AssertionError, "ordinary directory"):
                controller.fresh_browser_artifacts(alias / "captures")
            self.assertFalse((actual / "captures").exists())

    def test_existing_artifacts_refuse_before_package_inspection_or_manager_setup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            artifacts = root / "captures"
            artifacts.mkdir()
            executable = Path(sys.executable).resolve()
            with patch.dict(os.environ, {"CONSTELLATION_NATIVE_STARTUP_TEST": "1"}), \
                 patch.object(controller.lifecycle.clean, "package_module") as package, \
                 patch.object(controller.consumer, "prepare_consumer_installation") as prepare, \
                 self.assertRaises(FileExistsError):
                controller.qualify(root / "old.zip", root / "old.json", root / "new.zip", root / "new.json",
                    executable, root / "result.json", browser_node=executable, browser_artifacts=artifacts)
            package.assert_not_called()
            prepare.assert_not_called()

    def test_replaced_reserved_directory_refuses_before_browser_spawn(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "captures"
            identity = controller.fresh_browser_artifacts(path)
            path.rename(root / "original")
            path.mkdir()
            with patch.object(controller.subprocess, "Popen") as spawn, \
                 self.assertRaisesRegex(AssertionError, "directory changed"):
                controller.exercise_browser(Mock(), None, None, None, None, None, None, path, identity)
            spawn.assert_not_called()


if __name__ == "__main__":
    unittest.main()
