"""Controller boundaries; actual native execution belongs to the package lane."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("native_consumer_install", Path(__file__).with_name("native_consumer_install.py"))
consumer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(consumer)


class ConsumerControllerTests(unittest.TestCase):
    def readiness_fixture(self, root, *, after=None, response_change=None, config_change=None):
        install = self.fixture(root)
        config = {"id": "owned-installation", "public_url": "http://127.0.0.1:31471", "port": 31471}
        before = {"url": config["public_url"], "port": config["port"], "pid": 101, "process_nonce": "fixture-generation-a"}
        current = {**before, **(after or {})}
        consumer.write_json(install.home / "config.json", config)
        consumer.write_json(install.home / "runtime.json", before)

        def health(*_args, **_kwargs):
            # Publish the next generation after running() has read the old
            # runtime, exactly when the new listener can answer /health.
            replacement = install.home / "next-runtime.json"
            consumer.write_json(replacement, current)
            replacement.replace(install.home / "runtime.json")
            if config_change:
                consumer.write_json(install.home / "config.json", {**config, **config_change})
            response = io.BytesIO(b'{"ok":true}')
            response.status = 200
            return response

        install.health = Mock(open=Mock(side_effect=health))
        install.ports = set()
        value = {"id": config["id"], "pid": current["pid"], "process_nonce": current["process_nonce"], **(response_change or {})}
        install.api = Mock(return_value=value)
        return install, value

    def test_runtime_generation_publication_is_pending_then_stable_without_relaunch(self):
        for after in [{"pid": 202, "process_nonce": "fixture-generation-b"},
                      {"process_nonce": "fixture-generation-b"}]:
            with self.subTest(after=after), tempfile.TemporaryDirectory() as folder:
                install, value = self.readiness_fixture(Path(folder), after=after)
                with patch.object(consumer.subprocess, "Popen") as spawn:
                    self.assertIsNone(install.running())
                    self.assertIs(install.running(), value)
                    spawn.assert_not_called()
                self.assertEqual(install.api.call_count, 2)
                self.assertTrue(all(not call.args and not call.kwargs for call in install.api.call_args_list))

    def test_stable_runtime_identity_mismatch_still_refuses(self):
        for changed in [{"id": "other-installation"}, {"pid": 303}, {"process_nonce": "fixture-wrong-generation"}]:
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as folder:
                install, _ = self.readiness_fixture(Path(folder), response_change=changed)
                with self.assertRaisesRegex(AssertionError, "identity differs"):
                    install.running()
                install.api.assert_called_once_with()

    def test_generation_change_cannot_hide_wrong_installation_origin_port_or_config(self):
        cases = [({"response_change": {"id": "other-installation"}}, "installation identity differs"),
                 ({"after": {"url": "http://127.0.0.1:31472"}}, "saved literal loopback origin"),
                 ({"after": {"port": 31472}}, "saved literal loopback origin"),
                 ({"config_change": {"public_url": "http://127.0.0.1:31472"}}, "configuration identity changed"),
                 ({"config_change": {"id": "other-installation"}}, "configuration identity changed"),
                 ({"config_change": {"port": 31472}}, "configuration identity changed")]
        for changes, message in cases:
            with self.subTest(changes=changes), tempfile.TemporaryDirectory() as folder:
                after = {"pid": 202, "process_nonce": "fixture-generation-b", **changes.get("after", {})}
                install, _ = self.readiness_fixture(Path(folder), **{**changes, "after": after})
                with self.assertRaisesRegex(AssertionError, message):
                    install.running()
                install.api.assert_called_once_with()

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
