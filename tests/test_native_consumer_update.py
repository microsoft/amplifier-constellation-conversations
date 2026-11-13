"""Controller failure boundaries; these do not claim native lifetime proof."""
import importlib.util
import http.server
import io
import json
from pathlib import Path
import queue
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import urllib.error
import urllib.request

spec = importlib.util.spec_from_file_location("native_consumer_update", Path(__file__).with_name("native_consumer_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


class ConsumerUpdateControllerTests(unittest.TestCase):
    def cleanup_fixture(self, home):
        install = controller.consumer.ConsumerInstallation.__new__(controller.consumer.ConsumerInstallation)
        install.home, install.ports, install.result = home, set(), {"cleanup": {}}
        install.checkpoint = Mock()
        install.api = Mock(side_effect=AssertionError("retained read-only API connection reset"))
        install.running = Mock(side_effect=AssertionError("Cleanup must not request authenticated status"))
        install.command = Mock(side_effect=AssertionError("Cleanup must not dispatch a command"))
        config = {"id": "owned-installation", "transport": "local_http", "port": 31471,
                  "public_url": "http://127.0.0.1:31471"}
        runtime = {"url": config["public_url"], "public_url": config["public_url"], "port": config["port"],
                   "provider_id": "web:" + config["id"], "pid": 101, "process_nonce": "retained-generation"}
        controller.consumer.write_json(home / "config.json", config)
        controller.consumer.write_json(home / "runtime.json", runtime)
        (home / "service.lock").write_bytes(b"0")
        return install

    def cleanup_socket(self, codes, *, during=None):
        connection = Mock()
        connection.__enter__ = Mock(return_value=connection)
        connection.__exit__ = Mock(return_value=False)
        values = iter(codes)
        def connect(_endpoint):
            if during:
                during()
            return next(values)
        connection.connect_ex.side_effect = connect
        return connection

    def test_cleanup_after_health_to_api_reset_uses_closed_socket_and_real_lock_only(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            connection = self.cleanup_socket([controller.errno.ECONNREFUSED])
            # Use the real fixture's filesystem lock acquisition. The socket is
            # deterministic; no manager, service, or authenticated API runs.
            with patch.object(controller.socket, "socket", return_value=connection):
                result = controller.await_cleanup_stopped(install, stage="manager_stop")
            self.assertEqual(result["status"], "stopped")
            self.assertEqual(result["samples"], 1)
            self.assertTrue(result["listener_closed"] and result["service_lock_released"])
            self.assertEqual(result["authenticated_requests"], 0)
            install.running.assert_not_called()
            install.api.assert_not_called()
            install.command.assert_not_called()
            connection.connect_ex.assert_called_once_with(("127.0.0.1", 31471))

    def test_cleanup_waits_for_both_socket_refusal_and_lock_release(self):
        scenarios = [([0, controller.errno.ECONNREFUSED], [True, True]),
                     ([controller.errno.ECONNREFUSED] * 2, [False, True]),
                     ([controller.errno.ETIMEDOUT, 10061], [True, True]),
                     ([controller.errno.EWOULDBLOCK, controller.errno.ECONNREFUSED], [True, True]),
                     ([10035, 10061], [True, True])]
        for codes, locks in scenarios:
            with self.subTest(codes=codes, locks=locks), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                install.service_lock_available = Mock(side_effect=locks)
                connection = self.cleanup_socket(codes)
                with patch.object(controller.socket, "socket", return_value=connection), patch.object(controller.time, "sleep"):
                    value = controller.await_cleanup_stopped(install, stage="maintenance_exit")
                self.assertEqual(value["status"], "stopped")
                self.assertEqual(value["samples"], 2)
                self.assertEqual(install.service_lock_available.call_count, 2)
                install.running.assert_not_called()
                self.assertTrue(all(call.args[0] <= 0.2 for call in connection.settimeout.call_args_list))

    def test_cleanup_open_or_locked_or_timed_out_never_becomes_absence(self):
        for code, free in [(0, True), (controller.errno.ECONNREFUSED, False),
                           (controller.errno.ETIMEDOUT, True), (controller.errno.EWOULDBLOCK, True), (10035, True)]:
            with self.subTest(code=code, free=free), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                install.service_lock_available = Mock(return_value=free)
                connection = self.cleanup_socket([code])
                with patch.object(controller.socket, "socket", return_value=connection), \
                     patch.object(controller.time, "monotonic", side_effect=[0, 0, 0, 15, 15]), \
                     self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                row = install.result["cleanup"]["listener_closure"][0]
                self.assertEqual(row["status"], "unknown")
                self.assertNotIn("listener_closed", row)
                self.assertEqual(row["timeout_seconds"], 15)

    def test_cleanup_pending_then_open_cannot_prove_absence_before_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            install.service_lock_available = Mock(return_value=True)
            connection = self.cleanup_socket([10035, 0])
            with patch.object(controller.socket, "socket", return_value=connection), \
                 patch.object(controller.time, "monotonic", side_effect=[0, 0, 0, 1, 2, 2, 15, 15]), \
                 patch.object(controller.time, "sleep"), \
                 self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                controller.await_cleanup_stopped(install, stage="manager_stop")
            row = install.result["cleanup"]["listener_closure"][0]
            self.assertEqual(row["status"], "unknown")
            self.assertEqual(row["listener"], "open")
            self.assertEqual(row["samples"], 2)
            self.assertNotIn("listener_closed", row)
            install.api.assert_not_called()
            self.assertEqual(connection.connect_ex.call_count, 2)

    def test_cleanup_unknown_socket_error_is_not_swallowed_or_retried(self):
        for code in [controller.errno.EACCES, controller.errno.ECONNRESET, 10013, 10054]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                install.service_lock_available = Mock()
                connection = self.cleanup_socket([code])
                with patch.object(controller.socket, "socket", return_value=connection), \
                     self.assertRaisesRegex(AssertionError, "unknown after socket error"):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                row = install.result["cleanup"]["listener_closure"][0]
                self.assertEqual(row["status"], "unknown")
                self.assertEqual(row["socket_error"], code)
                self.assertEqual(connection.connect_ex.call_count, 1)
                install.service_lock_available.assert_not_called()
                install.api.assert_not_called()

    def test_cleanup_missing_malformed_or_drifted_identity_cannot_prove_stopped(self):
        cases = [("config.json", {"id": "another-installation"}),
                 ("config.json", {"public_url": "http://example.invalid:31471"}),
                 ("config.json", {"transport": "https"}),
                 ("runtime.json", {"url": "http://localhost:31471"}),
                 ("runtime.json", {"port": "31471"}),
                 ("runtime.json", {"provider_id": "web:other"}),
                 ("runtime.json", {"pid": 202}),
                 ("runtime.json", {"process_nonce": "different-generation"}),
                 ("runtime.json", None), ("service.lock", None)]
        for name, changes in cases:
            with self.subTest(name=name, changes=changes), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                def change():
                    path = install.home / name
                    if changes is None:
                        path.unlink()
                    else:
                        value = controller.read_json(path)
                        controller.consumer.write_json(path, {**value, **changes})
                connection = self.cleanup_socket([controller.errno.ECONNREFUSED], during=change)
                with patch.object(controller.socket, "socket", return_value=connection), \
                     self.assertRaises((AssertionError, OSError)):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                self.assertEqual(install.result["cleanup"]["listener_closure"][0]["status"], "unknown")
                install.running.assert_not_called()

    def test_cleanup_deadline_cannot_dispatch_socket_after_slow_identity_read(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            with patch.object(controller.socket, "socket") as connection, \
                 patch.object(controller.time, "monotonic", side_effect=[0, 0, 15, 15]), \
                 self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                controller.await_cleanup_stopped(install, stage="manager_stop")
            connection.assert_not_called()
            self.assertEqual(install.result["cleanup"]["listener_closure"][0]["status"], "unknown")

    def test_check_accepts_one_reply_after_ordinary_http_limit_within_ui_allowance(self):
        requests = []
        checked = {"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}

        class DelayedCheck(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                requests.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                # Exercise a real delayed HTTP status, beyond the former 15s
                # fixture limit but within the update page's existing 40s.
                time.sleep(16)
                body = json.dumps(checked).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        with tempfile.TemporaryDirectory() as directory, http.server.HTTPServer(("127.0.0.1", 0), DelayedCheck) as server:
            server.timeout = 20
            home = Path(directory)
            (home / "config.json").write_text(json.dumps({"public_url": f"http://127.0.0.1:{server.server_port}"}))
            install = controller.consumer.ConsumerInstallation.__new__(controller.consumer.ConsumerInstallation)
            install.home, install.csrf, install.result, install.checkpoint = home, None, {}, Mock()
            install.browser = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            thread = threading.Thread(target=server.handle_request)
            thread.start()
            try:
                self.assertEqual(controller.check_once(install, Path("candidate")), checked)
            finally:
                thread.join(timeout=20)
                self.assertFalse(thread.is_alive())
            self.assertEqual(requests, [(controller.API + "/check", {"package_path": "candidate"})])
            self.assertEqual(install.result["check_request"]["response"], "received")
            self.assertGreaterEqual(install.result["check_request"]["elapsed_seconds"], 15)
            self.assertLess(install.result["check_request"]["elapsed_seconds"], 40)

    def test_unknown_check_retains_only_bounded_ledger_evidence_without_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / "native-browser-update").mkdir()
            (home / "config.json").write_text(json.dumps({"id": "installation"}))
            check_id = "f9b9dd53-f007-4d83-b4e8-90d56046e943"
            ledger = home / "native-browser-update/control.json"
            ledger.write_text(json.dumps({"schema": 1, "installation_id": "installation",
                "current_check": check_id, "checks": {check_id: {"plan": "private-package-path"}},
                "current_operation": None, "operations": {}, "private": "secret-nonce"}))
            install = SimpleNamespace(home=home, result={}, checkpoint=Mock(),
                browser_request=Mock(side_effect=TimeoutError("private socket error")))
            with patch.object(controller, "request_status", return_value={"owner": "standalone", "mode": "normal", "actions": {"check": True}}), patch.object(controller, "submit_once") as submit:
                with self.assertRaises(TimeoutError):
                    controller.exercise_api(install, Path("a"), Path("b"), Mock())
                submit.assert_not_called()
            install.browser_request.assert_called_once()
            row = install.result["check_request"]
            self.assertEqual(row["response"], "transport_unknown")
            self.assertEqual(row["mutation_retries"], 0)
            self.assertEqual(row["ledger_after_failure"]["current_check_id"], check_id)
            self.assertTrue(row["ledger_after_failure"]["current_check_record_present"])
            self.assertNotIn("private", json.dumps(row))
            self.assertNotIn("secret-nonce", json.dumps(row))
            ledger.write_text(json.dumps({"schema": 1, "installation_id": "installation",
                "current_check": "private-malformed-id", "current_operation": None}))
            self.assertEqual(controller.check_failure_ledger(home),
                {"status": "unavailable", "code": "browser_ledger_observation_failed"})

    def test_task_engine_without_actual_worker_cannot_prove_survival(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            ledger = home / "native-browser-update/control.json"
            ledger.parent.mkdir()
            ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
                "transaction_id": "transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
            observer = controller.NativeObserver.__new__(controller.NativeObserver)
            observer.install = SimpleNamespace(home=home)
            observer.previous_operation = observer.operation = observer.attempt = observer.transaction = None
            observer.worker_generation = observer.actual_worker_generation = observer.worker_observation = None
            observer.old_identity = {"pid": 1, "birth": "old"}
            observer.old_exit = observer.survived_old_exit = False
            observer.observations = []
            engine = {"status": "observed", "profile_matches": True, "active": True, "transitioning": False,
                      "processes": [{"identity": {"pid": 2, "birth": "engine"}}], "previous_process_alive": False}
            observer.call = Mock(return_value=engine)
            observer.sample()
            self.assertFalse(observer.survived_old_exit)
            self.assertIsNone(observer.actual_worker_generation)
            observer.call.return_value = {**engine, "actual_worker_identity": {"pid": 3, "birth": "worker"},
                                          "actual_worker_alive": True, "actual_worker_manager_binding": "descendant_of_task_engine"}
            observer.sample()
            self.assertTrue(observer.survived_old_exit)
            self.assertIsNotNone(observer.actual_worker_generation)

    def observer_read_fixture(self, home):
        ledger = home / "native-browser-update/control.json"
        ledger.parent.mkdir()
        ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
            "transaction_id": "transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
        observer = controller.NativeObserver.__new__(controller.NativeObserver)
        observer.install = SimpleNamespace(home=home)
        observer.previous_operation = observer.operation = observer.attempt = observer.transaction = None
        observer.worker_generation = observer.actual_worker_generation = observer.worker_observation = None
        observer.old_identity = {"pid": 1, "birth": "old"}
        observer.old_exit = observer.survived_old_exit = False
        observer.observations = []
        worker = {"status": "observed", "profile_matches": True, "active": True, "transitioning": False,
                  "processes": [{"identity": {"pid": 2, "birth": "engine"}}], "previous_process_alive": False,
                  "actual_worker_identity": {"pid": 3, "birth": "worker"}, "actual_worker_alive": True,
                  "actual_worker_manager_binding": "descendant_of_task_engine"}
        observer.call = Mock(return_value=worker)
        return observer, ledger, worker

    @staticmethod
    def observer_proof(observer):
        fields = ["previous_operation", "operation", "attempt", "transaction", "worker_generation",
                  "actual_worker_generation", "worker_observation", "old_identity", "old_exit",
                  "survived_old_exit", "observations"]
        return json.dumps({key: getattr(observer, key) for key in fields}, sort_keys=True)

    def test_unavailable_ledger_sample_advances_no_proof_and_next_sample_is_fresh(self):
        for error in [PermissionError(13, "fixture private reader unavailable"), FileNotFoundError("fixture replaced")]:
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as directory:
                observer, _, _ = self.observer_read_fixture(Path(directory))
                before = self.observer_proof(observer)
                with patch.object(controller, "read_json", side_effect=error):
                    self.assertFalse(observer.sample())
                self.assertEqual(self.observer_proof(observer), before)
                observer.call.assert_not_called()
                self.assertTrue(observer.sample())
                self.assertTrue(observer.old_exit and observer.survived_old_exit)
                self.assertIsNotNone(observer.actual_worker_generation)
                observer.call.assert_called_once()

    def test_persistent_unavailable_ledger_cannot_reuse_stale_exit_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, worker = self.observer_read_fixture(Path(directory))
            self.assertTrue(observer.sample())
            observer.worker_observation = {**worker, "active": False, "processes": [], "actual_worker_alive": False}
            before = self.observer_proof(observer)
            observer.call.reset_mock()
            with patch.object(controller, "read_json", side_effect=PermissionError(13, "unavailable")), \
                 patch.object(controller.time, "monotonic", side_effect=[0, 0, 1, 40]), \
                 patch.object(controller.time, "sleep"), patch.object(controller, "submit_once") as submit:
                with self.assertRaisesRegex(TimeoutError, "Actual worker exit is unobserved"):
                    observer.settled({"id": "operation", "transaction_id": "transaction"}, {})
                submit.assert_not_called()
            self.assertEqual(self.observer_proof(observer), before)
            observer.call.assert_not_called()

    def test_terminal_journal_cannot_supply_missed_worker_lifetime_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, worker = self.observer_read_fixture(Path(directory))
            with patch.object(controller, "read_json", side_effect=PermissionError(13, "unavailable")):
                self.assertFalse(observer.sample())
            observer.call.return_value = {**worker, "active": False, "processes": [], "actual_worker_alive": False}
            with self.assertRaisesRegex(AssertionError, "survival after old-parent exit"):
                observer.settled({"id": "operation", "transaction_id": "transaction", "phase": "completed", "outcome": "applied"}, {})
            self.assertFalse(observer.survived_old_exit)
            self.assertIsNone(observer.worker_generation)
            observer.call.assert_called_once()

    def test_malformed_or_rebound_ledger_still_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, ledger, _ = self.observer_read_fixture(Path(directory))
            self.assertTrue(observer.sample())
            before = self.observer_proof(observer)
            observer.call.reset_mock()
            ledger.write_text("{invalid-json")
            with self.assertRaises(json.JSONDecodeError):
                observer.sample()
            ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
                "transaction_id": "another-transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
            with self.assertRaisesRegex(AssertionError, "Operation or launch attempt changed"):
                observer.sample()
            self.assertEqual(self.observer_proof(observer), before)
            observer.call.assert_not_called()

    def test_legacy_startup_success_does_not_require_json_output(self):
        install = SimpleNamespace(worker=Path("app"), home=Path("owned-home"),
                                  begin_operation=Mock(return_value={}), end_operation=Mock())
        native = SimpleNamespace(command=Mock(return_value=SimpleNamespace(returncode=0, stdout="", stderr="")))
        with patch.object(controller, "startup_module", return_value=native):
            controller.startup_command(install, {"PATH": "fixed-os-tools"}, "install")
        native.command.assert_called_once_with([Path("app"), "--home", Path("owned-home"), "startup", "install"],
            env={"PATH": "fixed-os-tools"}, check=False, timeout=30)
        self.assertEqual(install.end_operation.call_args.args[2:], ("succeeded", 0))

    def test_protected_receipt_keeps_only_hashes_and_rejects_changed_work(self):
        baseline = {"files": {"secrets/local-password.json": "digest"},
                    "tables": {"browser_sessions": [["private-session-row"]],
                               "companion_actions": [["retained-work", "completed"]]},
                    "companion_policy": {"enabled": False}}
        install = SimpleNamespace(baseline=baseline, snapshot=Mock(return_value=baseline))
        result = controller.protected_receipt(install)
        self.assertEqual(result["tables"]["browser_sessions"]["rows"], 1)
        self.assertNotIn("private-session-row", json.dumps(result))
        install.snapshot.return_value = {**baseline, "tables": {"companion_actions": []}}
        with self.assertRaisesRegex(AssertionError, "protected"):
            controller.protected_receipt(install)

    def test_frame_limit_discards_oversized_private_line_before_capture(self):
        events, capture = queue.Queue(), controller.PrivateCapture()
        controller.read_frames(io.BytesIO(b'"private-' + b'x' * 40 + b'"\n'), events, capture, frame_limit=16)
        self.assertEqual(events.get()[0], "error")
        self.assertEqual(capture.value(), b"")

    def test_raw_capture_overflow_drops_private_content_instead_of_partial_truncation(self):
        capture = controller.PrivateCapture()
        capture.append(b"private-token")
        capture.append(b"x" * (1024 * 1024))
        capture.append(b"more-private")
        self.assertTrue(capture.overflow)
        self.assertNotIn(b"private-token", capture.value())
        self.assertNotIn(b"more-private", capture.value())

    def test_request_reconciliation_never_mutates_or_accepts_a_rebound_transaction(self):
        request = "request"
        first = {"request": {"id": request, "state": "recorded", "operation_id": "operation"},
                 "operation": {"id": "operation", "transaction_id": "original", "phase": "running"}}
        rebound = {**first, "operation": {**first["operation"], "transaction_id": "different", "phase": "completed", "outcome": "applied"}}
        install = SimpleNamespace()
        with patch.object(controller, "request_status", side_effect=[first, rebound]) as status, patch.object(controller.time, "sleep"), self.assertRaisesRegex(AssertionError, "original transaction"):
            controller.await_operation(install, request, "applied", Mock())
        self.assertTrue(all(call.args == (install, request) and call.kwargs == {"tolerate_unavailable": True} for call in status.call_args_list))

    def test_apply_transport_failure_is_retained_without_another_mutation(self):
        install = SimpleNamespace(home=Path("private-home"), csrf="private-csrf", result={}, checkpoint=Mock(),
                                  browser=SimpleNamespace(open=Mock(side_effect=urllib.error.URLError("lost response"))))
        with patch.object(controller, "read_json", return_value={"public_url": "http://127.0.0.1:12345"}):
            controller.submit_once(install, "apply", {"request_id": "original", "check_id": "checked"})
        install.browser.open.assert_called_once()
        self.assertEqual(install.result["request_dispatch"], [{"action": "apply", "request_id": "original",
                          "response": "transport_unknown", "mutation_retries": 0}])

    def test_server_error_is_uncertain_but_client_refusal_fails_clearly(self):
        for status in [503, 409]:
            with self.subTest(status=status):
                response = Mock(status=status)
                response.__enter__ = Mock(return_value=response)
                response.__exit__ = Mock(return_value=False)
                install = SimpleNamespace(home=Path("private-home"), csrf="private-csrf", result={}, checkpoint=Mock(),
                                          browser=SimpleNamespace(open=Mock(return_value=response)))
                with patch.object(controller, "read_json", return_value={"public_url": "http://127.0.0.1:12345"}):
                    if status == 503:
                        controller.submit_once(install, "recover", {"request_id": "original"})
                        self.assertEqual(install.result["request_dispatch"][0]["response"], "server_error_unknown")
                    else:
                        with self.assertRaisesRegex(AssertionError, "HTTP 409"):
                            controller.submit_once(install, "recover", {"request_id": "original"})
                        self.assertEqual(install.result["request_dispatch"][0]["response"], "refused")
                install.browser.open.assert_called_once()

    def test_apply_uncertainty_enters_original_uuid_reconciliation_before_any_rollback(self):
        install = SimpleNamespace(result={}, checkpoint=Mock(), browser_request=Mock(return_value={"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}))
        with patch.object(controller, "request_status", return_value={"owner": "standalone", "mode": "normal", "actions": {"check": True}}), patch.object(controller, "submit_once") as submit, patch.object(controller, "await_operation", side_effect=TimeoutError("outcome unknown")) as reconcile:
            with self.assertRaises(TimeoutError):
                controller.exercise_api(install, Path("a"), Path("b"), Mock())
        submit.assert_called_once()
        reconcile.assert_called_once()
        self.assertEqual(submit.call_args.args[1], "apply")
        self.assertEqual(reconcile.call_args.args[1], submit.call_args.args[2]["request_id"])

    def test_marker_only_lifetime_evidence_cannot_pass_a_checkpoint(self):
        baseline = {"files": {}, "tables": {}, "companion_policy": {}}
        install = SimpleNamespace(baseline=baseline, snapshot=lambda: baseline, selected=Mock(),
            browser_request=Mock(return_value={"authenticated": True}), home=Path("private-home"),
            await_running=Mock(return_value={"id": "installation"}))
        observer = Mock()
        observer.settled.return_value = {"marker_present": True, "worker_exit_observed": False}
        snapshot = {"operation": {"id": "operation", "transaction_id": "transaction", "phase": "completed", "outcome": "applied"}}
        with patch.object(controller, "read_json", return_value={"id": "installation"}), self.assertRaisesRegex(AssertionError, "OS lifetime"):
            controller.phase_checkpoint(install, Path("bundle"), "applied", snapshot, observer)


if __name__ == "__main__":
    unittest.main()
