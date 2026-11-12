"""Controller failure boundaries; these do not claim native lifetime proof."""
import importlib.util
import io
import json
from pathlib import Path
import queue
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import urllib.error

spec = importlib.util.spec_from_file_location("native_consumer_update", Path(__file__).with_name("native_consumer_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


class ConsumerUpdateControllerTests(unittest.TestCase):
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
        install = SimpleNamespace(browser_request=Mock(return_value={"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}))
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
