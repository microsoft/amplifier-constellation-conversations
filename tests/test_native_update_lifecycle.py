"""Controller invariants; real executable behavior is tested by the CI harness."""
import importlib.util
import json
from pathlib import Path, PureWindowsPath
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

    def test_nonzero_diagnostics_retain_only_static_categories_and_numeric_codes(self):
        with tempfile.TemporaryDirectory() as folder:
            install = self.checkpoint_fixture(Path(folder))
            process = Mock(returncode=1)
            process.communicate.return_value = ('{"credential":"secret-output"}',
                'Error: Cannot restrict application storage to this Windows user: secret-path (os error 206)\n'
                'secret-nonce secret-token; Existing executable preserved; Frozen package changed; '
                'Private files must be ordinary files; Companion node was customized; (os error 5)')
            with patch.object(lifecycle.subprocess, "Popen", return_value=process):
                with self.assertRaises(AssertionError) as raised:
                    install.command(["native-update", "bootstrap", "secret-path"], input_text="secret-input")
            self.assertNotIn("secret", str(raised.exception))
            text = install.checkpoint_output.read_text()
            self.assertNotIn("secret", text)
            diagnostic = json.loads(text)["current_operation"]["failure_diagnostic"]
            self.assertEqual(diagnostic["os_error_codes"], [5, 206])
            self.assertEqual(diagnostic["reason_tags"], ["customized_binding", "existing_executable_preserved",
                "frozen_package_changed", "invalid_private_path", "private_storage_permissions"])
            self.assertFalse(diagnostic["raw_content_retained"])

    def test_failure_reason_scan_and_error_codes_are_bounded(self):
        diagnostic = lifecycle.failure_summary(None, " ".join(f"(os error {code})" for code in range(20))
            + "x" * 8192 + "Cannot create private storage policy (os error 999)")
        self.assertEqual(diagnostic["os_error_codes"], list(range(8)))
        self.assertTrue(diagnostic["os_error_codes_truncated"])
        self.assertEqual(diagnostic["reason_tags"], [])
        self.assertTrue(diagnostic["stderr"]["sample_truncated"])

    def test_atomic_write_and_rollback_contexts_have_fixed_reason_tags(self):
        contexts = {
            "Cannot commit private state": "cannot_commit_private_state",
            "Cannot publish native update journal": "native_update_journal_commit",
            "Cannot commit installed bundle receipt": "installed_bundle_receipt_commit",
            "Cannot persist native installation receipt": "native_installation_receipt_commit",
            "Cannot replace native service executable": "native_service_executable_replace",
            "Cannot commit companion bundle bindings": "companion_bundle_bindings_commit",
            "Cannot stage immutable native bundle": "immutable_native_bundle_stage",
            "Update failed; rollback bundle switch also failed": "rollback_bundle_switch_failed",
            "Update failed; previous bundle verification also failed": "previous_bundle_verification_failed",
            "Update failed; previous availability restoration also failed": "previous_availability_restoration_failed",
        }
        for context, tag in contexts.items():
            with self.subTest(tag=tag):
                diagnostic = lifecycle.failure_summary(None, context + ": secret-path secret-nonce (os error 5)")
                self.assertEqual(diagnostic["reason_tags"], [tag])
                self.assertEqual(diagnostic["os_error_codes"], [5])
                self.assertNotIn("secret", json.dumps(diagnostic))

    def test_failure_snapshot_retains_only_allowlisted_state_and_digest_comparisons(self):
        with tempfile.TemporaryDirectory() as folder:
            home = Path(folder)
            binary = home / "bin/app"
            binary.parent.mkdir()
            binary.write_bytes(b"secret-executable-bytes")
            previous = lifecycle.digest(binary.read_bytes())
            candidate = lifecycle.digest(b"candidate-secret")
            journal = {"phase": "switching", "direction": "rolled_back", "selected": "previous",
                "restore_started": False, "rollback_aborted": True, "restore_outcome": "unchanged",
                "child_pid": 19790212, "previous": {"binary_sha256": previous, "bundle": "secret-previous-path"},
                "candidate": {"binary_sha256": candidate, "bundle": "secret-candidate-path"},
                "context": {"token": "secret-token"}, "launch_nonce": "secret-nonce"}
            lifecycle.write_json(home / "native-update.json", journal)
            lifecycle.write_json(home / "native-update-process-exit.json", {
                "exit_confirmed": False, "pid": 19790212, "process_birth": "secret-birth", "process_nonce": "secret-nonce"})
            database = lifecycle.sqlite3.connect(home / "state.sqlite3")
            try:
                database.execute("PRAGMA journal_mode=WAL")
                database.execute("CREATE TABLE native_update_admission(transaction_id TEXT, phase TEXT)")
                database.execute("INSERT INTO native_update_admission VALUES('secret-transaction', 'prepared')")
                database.commit()
                before = (home / "native-update.json").read_bytes()
                with patch.object(lifecycle.subprocess, "Popen", side_effect=AssertionError("No subprocess allowed")), \
                        patch.object(lifecycle.urllib.request, "urlopen", side_effect=AssertionError("No service query allowed")):
                    snapshot = lifecycle.failure_state_snapshot(home, "app")
                self.assertEqual((home / "native-update.json").read_bytes(), before)
                self.assertEqual(database.execute("SELECT phase FROM native_update_admission").fetchall(), [("prepared",)])
            finally:
                database.close()
            self.assertEqual(snapshot["journal"], {"status": "available", "phase": "switching",
                "direction": "rolled_back", "selected": "previous", "restore_outcome": "unchanged",
                "restore_started": False, "rollback_aborted": True, "child_pid_present": True})
            self.assertEqual(snapshot["admission"], {"status": "available", "closed": True})
            self.assertEqual(snapshot["installed_binary"], {"status": "available", "matches_previous": True,
                "matches_candidate": False})
            self.assertEqual(snapshot["process_exit_receipt"], {"status": "available", "exit_confirmed": False})
            text = json.dumps(snapshot)
            for sensitive in ["secret", previous, candidate, "19790212", str(home)]:
                self.assertNotIn(sensitive, text)

    def test_failure_snapshot_absent_state_does_not_create_installation(self):
        with tempfile.TemporaryDirectory() as folder:
            home = Path(folder) / "absent-secret-home"
            snapshot = lifecycle.failure_state_snapshot(home, "app")
            self.assertFalse(home.exists())
            for name in ["journal", "admission", "installed_binary", "process_exit_receipt"]:
                self.assertEqual(snapshot[name]["status"], "absent")
                self.assertTrue(all(value is None for key, value in snapshot[name].items() if key != "status"))
            self.assertNotIn("secret", json.dumps(snapshot))

    def test_failure_snapshot_rejects_malformed_values_without_coercion(self):
        with tempfile.TemporaryDirectory() as folder:
            home = Path(folder)
            lifecycle.write_json(home / "native-update.json", {"phase": ["secret-phase"],
                "direction": {"secret-direction": True}, "selected": "secret-path", "restore_outcome": 1,
                "restore_started": 1, "rollback_aborted": "true", "child_pid": True,
                "previous": {"binary_sha256": "secret-digest"}, "candidate": ["secret"]})
            lifecycle.write_json(home / "native-update-process-exit.json", {"exit_confirmed": "secret-value"})
            (home / "state.sqlite3").write_bytes(b"secret malformed database")
            (home / "bin").mkdir()
            (home / "bin/app").write_bytes(b"secret")
            snapshot = lifecycle.failure_state_snapshot(home, "app")
            self.assertTrue(all(value is None for key, value in snapshot["journal"].items() if key != "status"))
            self.assertEqual(snapshot["admission"], {"status": "unavailable", "closed": None})
            self.assertEqual(snapshot["installed_binary"], {"status": "available", "matches_previous": None,
                "matches_candidate": None})
            self.assertIsNone(snapshot["process_exit_receipt"]["exit_confirmed"])
            self.assertNotIn("secret", json.dumps(snapshot))
            for malformed in [b'{"secret":', b'["secret"]', b'\xffsecret']:
                (home / "native-update.json").write_bytes(malformed)
                self.assertEqual(lifecycle.failure_state_snapshot(home, "app")["journal"]["status"], "invalid")

    def test_failure_snapshot_bounds_files_and_rejects_admission_views(self):
        with tempfile.TemporaryDirectory() as folder:
            home = Path(folder)
            (home / "native-update.json").write_bytes(b"x" * (1024 * 1024 + 1))
            (home / "bin").mkdir()
            with (home / "bin/app").open("wb") as stream:
                stream.truncate(512 * 1024 * 1024 + 1)
            database = lifecycle.sqlite3.connect(home / "state.sqlite3")
            try:
                database.execute("CREATE VIEW native_update_admission AS SELECT 'prepared' AS phase")
                database.commit()
            finally:
                database.close()
            snapshot = lifecycle.failure_state_snapshot(home, "app")
            self.assertEqual(snapshot["journal"]["status"], "too_large")
            self.assertEqual(snapshot["installed_binary"]["status"], "too_large")
            self.assertEqual(snapshot["admission"], {"status": "invalid", "closed": None})

    def test_precleanup_snapshot_errors_do_not_skip_either_owned_cleanup(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            manifest = {"target": "fixture-target", "source_revision": "fixture-revision", "files": []}
            manifest_bytes = json.dumps(manifest).encode()
            receipt, output = root / "receipt.json", root / "result.json"
            lifecycle.write_json(receipt, {"archive_sha256": "fixture-archive",
                "manifest_sha256": lifecycle.digest(manifest_bytes)})
            primary, secondary = Mock(), Mock()
            primary.failure_snapshot.return_value = {"journal": {"phase": "restored"}}
            secondary.failure_snapshot.side_effect = RuntimeError("secret-diagnostic-error")
            cleanup_order = []
            def cleanup(name):
                snapshots = lifecycle.read_json(output)["failure_installation_snapshots"]
                self.assertEqual(snapshots["primary"], {"journal": {"phase": "restored"}})
                self.assertEqual(snapshots["explicit_rollback_failure"], {"status": "unavailable"})
                cleanup_order.append(name)
                return {"owned_children_reaped": True}
            primary.cleanup.side_effect = lambda candidate: cleanup("primary")
            secondary.cleanup.side_effect = lambda candidate: cleanup("secondary")
            def derive(source, destination, label, **kwargs):
                destination.mkdir()
                (destination / "manifest.json").write_bytes(manifest_bytes)
            package = Mock()
            package.inspect_archive.return_value = manifest, {"manifest.json": manifest_bytes}
            with patch.object(lifecycle.clean, "package_module", return_value=package), \
                    patch.object(lifecycle.clean, "native_target", return_value="fixture-target"), \
                    patch.object(lifecycle, "derive_bundle", side_effect=derive), \
                    patch.object(lifecycle, "Installation", side_effect=[primary, secondary]), \
                    patch.object(lifecycle, "exercise"), \
                    patch.object(lifecycle, "exercise_failed_explicit_rollback", side_effect=RuntimeError("fixture failed")):
                with self.assertRaisesRegex(RuntimeError, "fixture failed"):
                    lifecycle.qualify(root / "archive", receipt, output)
            self.assertEqual(cleanup_order, ["secondary", "primary"])
            result = lifecycle.read_json(output)
            self.assertTrue(result["temporary_installation_removed"])
            self.assertEqual(result["status"], "failed")
            self.assertNotIn("secret", output.read_text())

    def test_fixture_path_evidence_counts_unicode_without_retaining_paths(self):
        root = PureWindowsPath(r"C:\secret-user\fixture café 🧪")
        home = root / "Companion installation 日本語"
        manifest = {"files": [{"path": "bridge/node_modules/openai/src/resources/admin/organization/workload-identity-providers/index.js"}]}
        first = lifecycle.fixture_path_lengths(root, home, manifest)
        secondary_root = root / "Explicit rollback failure installation"
        second = lifecycle.fixture_path_lengths(secondary_root, secondary_root / home.name, manifest)
        self.assertEqual(first["root_utf16_units"], len(str(root).encode("utf-16-le")) // 2)
        self.assertEqual(second["deepest_protected_staging_parent_utf16_units"]
                         - first["deepest_protected_staging_parent_utf16_units"], 39)
        self.assertNotIn("secret", json.dumps(first))
        self.assertNotIn("Companion installation", json.dumps(second))

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
