"""Controller-clock regression only; never invokes a native manager/process."""
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

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


if __name__ == "__main__":
    unittest.main()
