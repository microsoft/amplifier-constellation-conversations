"""Controller invariants; real executable behavior is tested by the CI harness."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("native_update_lifecycle", Path(__file__).with_name("native_update_lifecycle.py"))
lifecycle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lifecycle)


class LifecycleControllerTests(unittest.TestCase):
    def checkpoint_fixture(self, root):
        install = object.__new__(lifecycle.Installation)
        install.root, install.home, install.worker = root, root / "home", root / "app"
        install.env = {}
        install.result = {"status": "running", "checks": []}
        install.checkpoint_output = root / "progress.json"
        install.unsettled_commands = []
        return install

    def test_progress_checkpoint_precedes_command_and_omits_sensitive_data(self):
        with tempfile.TemporaryDirectory() as folder:
            install = self.checkpoint_fixture(Path(folder))
            def operation(argv, **kwargs):
                before = lifecycle.read_json(install.checkpoint_output)
                self.assertEqual(before["status"], "running")
                self.assertEqual(before["current_operation"]["phase"], "running")
                self.assertEqual(before["current_operation"]["operation"],
                    {"kind": "native-update-protocol", "mode": "participant", "action": "restore"})
                process = Mock(returncode=0)
                process.communicate.return_value = ('{"token":"secret-output"}', "secret-stderr")
                return process
            with patch.object(lifecycle.subprocess, "Popen", side_effect=operation):
                self.assertEqual(install.command(["native-update-protocol", "--config", "secret-path",
                    "--mode", "participant", "--action", "restore", "secret-nonce"],
                    extra_env={"PRIVATE_KEY": "secret-env"}, input_text="secret-input", timeout=30), {"token": "secret-output"})
            install.check("completed fixed check")
            text = install.checkpoint_output.read_text()
            self.assertNotIn("secret", text)
            after = json.loads(text)
            self.assertEqual(after["current_operation"]["outcome"], "succeeded")
            self.assertEqual(after["current_operation"]["return_code"], 0)
            self.assertEqual(len(after["checks"]), 1)
            self.assertIn("elapsed_seconds", after["native_operation_timings"][0])

    def test_timeout_checkpoint_and_exception_do_not_expose_full_command(self):
        with tempfile.TemporaryDirectory() as folder:
            install = self.checkpoint_fixture(Path(folder))
            timeout = lifecycle.subprocess.TimeoutExpired(["app", "secret-nonce"], 30)
            process = Mock(returncode=-9)
            process.poll.side_effect = [None, -9]
            process.communicate.side_effect = [timeout, ("secret-output", "secret-stderr")]
            with patch.object(lifecycle.subprocess, "Popen", return_value=process):
                with self.assertRaises(TimeoutError) as raised:
                    install.command(["native-update", "recover", "secret-nonce"], timeout=30)
            process.kill.assert_called_once_with()
            self.assertEqual(process.communicate.call_args_list[0].kwargs, {"input": None, "timeout": 30})
            self.assertEqual(process.communicate.call_args_list[1].kwargs, {"timeout": 10})
            process.wait.assert_not_called()
            self.assertNotIn("secret", str(raised.exception))
            result = lifecycle.read_json(install.checkpoint_output)
            self.assertEqual(result["current_operation"]["outcome"], "timeout")
            self.assertEqual(result["current_operation"]["timeout_seconds"], 30)
            diagnostic = result["current_operation"]["timeout_diagnostic"]
            self.assertTrue(diagnostic["capture_settled"])
            self.assertEqual(diagnostic["helper_return_code_after_drain"], -9)
            self.assertEqual(diagnostic["operation_outcome"], "unknown")
            self.assertNotIn("secret", install.checkpoint_output.read_text())

    def test_exited_helper_with_unsettled_capture_is_not_killed_or_reported_successful(self):
        with tempfile.TemporaryDirectory() as folder:
            install = self.checkpoint_fixture(Path(folder))
            process = Mock(returncode=0)
            process.poll.return_value = 0
            process.communicate.side_effect = [
                lifecycle.subprocess.TimeoutExpired(["secret-command"], 120),
                lifecycle.subprocess.TimeoutExpired(["secret-command"], 10, output=b"secret-output" * 1000)]
            with patch.object(lifecycle.subprocess, "Popen", return_value=process), \
                    patch.object(lifecycle, "UNSETTLED_CAPTURES", []):
                with self.assertRaises(TimeoutError):
                    install.command(["native-update", "run", "secret-path"])
                self.assertEqual(lifecycle.UNSETTLED_CAPTURES, [process])
            process.kill.assert_not_called()
            process.wait.assert_not_called()
            process.stdout.close.assert_not_called()
            process.stderr.close.assert_not_called()
            self.assertEqual(install.unsettled_commands, [process])
            diagnostic = lifecycle.read_json(install.checkpoint_output)["current_operation"]["timeout_diagnostic"]
            self.assertEqual(diagnostic["helper_return_code_at_timeout"], 0)
            self.assertFalse(diagnostic["helper_alive_at_timeout"])
            self.assertFalse(diagnostic["capture_settled"])
            self.assertTrue(diagnostic["helper_exit_observed"])
            self.assertEqual(diagnostic["stdout"]["sample_bytes"], 4096)
            self.assertTrue(diagnostic["stdout"]["sample_truncated"])
            self.assertNotIn("secret", install.checkpoint_output.read_text())

    def test_unsettled_live_helper_blocks_mutating_cleanup(self):
        with tempfile.TemporaryDirectory() as folder:
            install = self.checkpoint_fixture(Path(folder))
            process = Mock(returncode=None)
            process.poll.return_value = None
            process.communicate.side_effect = [
                lifecycle.subprocess.TimeoutExpired([], 120), lifecycle.subprocess.TimeoutExpired([], 10)]
            with patch.object(lifecycle.subprocess, "Popen", return_value=process), \
                    patch.object(lifecycle, "UNSETTLED_CAPTURES", []):
                with self.assertRaises(TimeoutError):
                    install.command(["native-update", "run", "candidate"])
            process.kill.assert_called_once_with()
            diagnostic = lifecycle.read_json(install.checkpoint_output)["current_operation"]["timeout_diagnostic"]
            self.assertIsNone(diagnostic["helper_return_code_after_drain"])
            self.assertFalse(diagnostic["helper_exit_observed"])
            with patch.object(install, "running") as running:
                with self.assertRaisesRegex(AssertionError, "helper exit remains unknown"):
                    install.cleanup(Path(folder) / "B")
                running.assert_not_called()

    def test_database_transactions_close_on_success_and_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            install = object.__new__(lifecycle.Installation)
            install.home = Path(folder)
            with install.db() as committed:
                committed.execute("CREATE TABLE retained(value INTEGER)")
                committed.execute("INSERT INTO retained VALUES(1)")
            with self.assertRaises(lifecycle.sqlite3.ProgrammingError):
                committed.execute("SELECT 1")
            with self.assertRaisesRegex(RuntimeError, "fixture failure"):
                with install.db() as aborted:
                    aborted.execute("INSERT INTO retained VALUES(2)")
                    raise RuntimeError("fixture failure")
            with self.assertRaises(lifecycle.sqlite3.ProgrammingError):
                aborted.execute("SELECT 1")
            with install.db() as checked:
                self.assertEqual(checked.execute("SELECT value FROM retained").fetchall(), [(1,)])

    def fixture(self, root):
        source = root / "A"
        source.mkdir()
        (source / "service").write_bytes(b"original service")
        (source / "node").write_bytes(b"original private node")
        manifest = {"version": "0.1.2", "entrypoints": {"service": "service", "node": "node"},
                    "files": [{"path": name, "sha256": lifecycle.digest((source / name).read_bytes()),
                               "bytes": (source / name).stat().st_size, "executable": True}
                              for name in ["service", "node"]]}
        lifecycle.write_json(source / "manifest.json", manifest)
        return source

    def test_candidate_b_changes_only_manifest_and_new_document(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            source = self.fixture(root)
            before = {path.name: path.read_bytes() for path in source.iterdir()}
            manifest = lifecycle.derive_bundle(source, root / "B", "b")
            self.assertEqual(before, {path.name: path.read_bytes() for path in source.iterdir()})
            self.assertEqual((root / "B/service").read_bytes(), before["service"])
            self.assertEqual(manifest["version"], "0.1.2+lifecycle-b")
            self.assertEqual(len(manifest["files"]), 3)
            for row in manifest["files"]:
                payload = (root / "B" / row["path"]).read_bytes()
                self.assertEqual(row["sha256"], lifecycle.digest(payload))
                self.assertEqual(row["bytes"], len(payload))

    def test_deliberately_incompatible_candidate_is_independent_copy(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            source = self.fixture(root)
            before = (source / "service").read_bytes()
            manifest = lifecycle.derive_bundle(source, root / "C", "failed", broken=True)
            self.assertEqual((source / "service").read_bytes(), before)
            self.assertEqual((root / "C/service").read_bytes(), (source / "node").read_bytes())
            service = next(row for row in manifest["files"] if row["path"] == "service")
            self.assertEqual(service["sha256"], lifecycle.digest((root / "C/service").read_bytes()))
            self.assertTrue(service["executable"])
            (root / "C/service").write_bytes(b"changed candidate")
            self.assertEqual((source / "service").read_bytes(), before)

    def test_context_binds_exact_plan_and_transaction(self):
        transaction = "c0ed61c0-741d-4f55-9912-f6f3eb27ca26"
        context = lifecycle.participant_context(transaction, ["instance"], plan_id="frozen-plan")
        self.assertEqual(context["id"], "frozen-plan")
        self.assertEqual(context["plan"]["transaction_id"], transaction)
        self.assertEqual(context["plan"]["components"], ["instance"])
        self.assertEqual(context["plan"]["restart_scope"], {"services": [], "instance": False, "slack_homes": []})
        self.assertEqual(context["participant"], "companion")

    def test_utf8_configuration_is_private_and_preserves_paths(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "日本語/config.json"
            lifecycle.write_json(path, {"path": "installation café 日本語"})
            self.assertEqual(lifecycle.read_json(path)["path"], "installation café 日本語")
            if lifecycle.os.name != "nt":
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
