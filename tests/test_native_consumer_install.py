"""Controller boundaries; actual native execution belongs to the package lane."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("native_consumer_install", Path(__file__).with_name("native_consumer_install.py"))
consumer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(consumer)


class ConsumerControllerTests(unittest.TestCase):
    def fixture(self, root):
        install = object.__new__(consumer.ConsumerInstallation)
        install.root, install.home = root, root / "home"
        install.unsettled_commands = []
        consumer.write_json(install.home / "config.json", {"public_url": "http://127.0.0.1:31471"})
        return install

    def test_authenticated_controller_requests_use_native_proof_client_not_http_bearer(self):
        with tempfile.TemporaryDirectory() as folder:
            install = self.fixture(Path(folder))
            install.command = Mock(return_value={"ok": True})
            with patch.object(consumer.urllib.request, "urlopen", side_effect=AssertionError("No raw HTTP bearer")):
                self.assertEqual(install.api("POST", "/api/v1/native-update/stop", {"transaction_id": "fixture"}), {"ok": True})
            arguments = install.command.call_args.args[0]
            self.assertEqual(arguments[:3], ["api", "POST", "/api/v1/native-update/stop"])
            self.assertEqual(arguments[3], "--body-file")
            self.assertEqual(consumer.read_json(arguments[4]), {"transaction_id": "fixture"})
            self.assertEqual(install.command.call_args.kwargs["timeout"], 30)

    def test_browser_inputs_use_stdin_and_private_values_never_enter_command_arguments(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            install = self.fixture(root)
            child = Mock(returncode=0)
            child.communicate.return_value = ('{"status":"passed","checks":3}', "")
            with patch.object(consumer.subprocess, "Popen", return_value=child) as popen:
                value = consumer.run_browser(install, root / "test.mjs", root / "node", root / "artifacts", "secret-setup-token")
            self.assertEqual(value["status"], "passed")
            self.assertEqual(popen.call_args.args[0], [str(root / "node"), str(root / "test.mjs")])
            data = json.loads(child.communicate.call_args.args[0])
            self.assertEqual(data["setup_token"], "secret-setup-token")
            self.assertEqual(data["fixture_key"], consumer.FIXTURE_KEY)
            self.assertEqual(child.communicate.call_args.kwargs["timeout"], 120)
            self.assertNotIn("secret", str(popen.call_args))

    def test_browser_failure_and_timeout_omit_raw_output_and_keep_bounded_drain(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            install = self.fixture(root)
            child = Mock(returncode=1)
            child.communicate.return_value = ("secret-output", "secret-stderr")
            with patch.object(consumer.subprocess, "Popen", return_value=child):
                with self.assertRaises(AssertionError) as raised:
                    consumer.run_browser(install, root / "test", root / "node", root / "artifacts", "secret-token")
                self.assertNotIn("secret", str(raised.exception))
            child.communicate.side_effect = [consumer.subprocess.TimeoutExpired(["secret-command"], 120), ("secret", "secret")]
            with patch.object(consumer.subprocess, "Popen", return_value=child):
                with self.assertRaises(TimeoutError) as raised:
                    consumer.run_browser(install, root / "test", root / "node", root / "artifacts", "secret-token")
                self.assertNotIn("secret", str(raised.exception))
            child.kill.assert_called_once_with()
            self.assertEqual(child.communicate.call_args.kwargs, {"timeout": 10})

    def test_account_requests_never_follow_redirects(self):
        self.assertIsNone(consumer.NoRedirect().redirect_request(None, None, 307, "redirect", {}, "http://attacker.invalid"))

    def test_browser_controller_preflight_refuses_missing_or_invalid_node_before_installation(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            node = root / "node"
            node.write_bytes(b"controller test fixture")
            node.chmod(0o700)
            cases = [(None, root / "artifacts"), (node, None),
                     (Path("relative-node"), root / "artifacts"),
                     (root / "absent-node", root / "artifacts"),
                     (root, root / "artifacts")]
            with patch.object(consumer, "read_json") as read, patch.object(consumer.tempfile, "mkdtemp") as allocate:
                for controller, artifacts in cases:
                    with self.subTest(controller=controller, artifacts=artifacts), self.assertRaises(AssertionError):
                        consumer.qualify(root / "archive", root / "receipt", root / "result",
                                         browser_test=root / "browser.mjs", browser_node=controller,
                                         browser_artifacts=artifacts)
                with patch.object(consumer.os, "access", return_value=False), self.assertRaises(AssertionError):
                    consumer.qualify(root / "archive", root / "receipt", root / "result",
                                     browser_test=root / "browser.mjs", browser_node=node,
                                     browser_artifacts=root / "artifacts")
                read.assert_not_called()
                allocate.assert_not_called()

    def test_browser_failure_artifact_retains_reason_but_redacts_private_inputs(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            install = self.fixture(root)
            child = Mock(returncode=1)
            child.communicate.return_value = ("setup=secret-token", "assertion: contact missing " + consumer.PASSWORD + " " + consumer.FIXTURE_KEY)
            with patch.object(consumer.subprocess, "Popen", return_value=child):
                with self.assertRaises(AssertionError):
                    consumer.run_browser(install, root / "test", root / "node", root / "artifacts", "secret-token")
            artifact = consumer.read_json(root / "artifacts/controller-browser-output.json")
            self.assertIn("assertion: contact missing", artifact["stderr"]["text"])
            self.assertEqual(artifact["return_code"], 1)
            for secret in ["secret-token", consumer.PASSWORD, consumer.FIXTURE_KEY]:
                self.assertNotIn(secret, json.dumps(artifact))

    def test_browser_diagnostic_redacts_before_bounding_and_handles_absent_or_byte_capture(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            secret = "private-value-spanning-boundary"
            consumer.retain_browser_output(root, "x" * 40000 + secret + "y" * 32760,
                                           b"static failure " + secret.encode(), [secret], returncode=None, timed_out=True)
            artifact = consumer.read_json(root / "controller-browser-output.json")
            self.assertTrue(artifact["stdout"]["truncated"])
            self.assertLessEqual(len(artifact["stdout"]["text"].encode()), 32768)
            self.assertNotIn("boundary", artifact["stdout"]["text"])
            self.assertEqual(artifact["stderr"]["text"], "static failure [redacted]")
            consumer.retain_browser_output(root, None, None, [secret], returncode=None, timed_out=True)
            self.assertEqual(consumer.read_json(root / "controller-browser-output.json")["stdout"]["text"], "")
            consumer.retain_browser_output(root, "日" * 40000, None, [secret], returncode=1, timed_out=False)
            self.assertLessEqual(len(consumer.read_json(root / "controller-browser-output.json")["stdout"]["text"].encode()), 32768)


if __name__ == "__main__":
    unittest.main()
