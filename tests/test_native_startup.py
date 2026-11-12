"""Unit checks for the disposable native startup controller; no OS-pass claims."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("native_startup", ROOT / "scripts/test-native-startup.py")
startup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(startup)


class NativeStartupHarnessTests(unittest.TestCase):
    def test_explicit_disposable_account_opt_in_is_required(self):
        # Keep OS identity variables (including Windows processor metadata); only
        # the explicit registration permission is absent in this refusal case.
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, {startup.OPT_IN: ""}):
            out = Path(temp) / "result.json"
            with self.assertRaisesRegex(AssertionError, "opt-in"):
                startup.qualify(Path("absent.tar.gz"), Path("absent.json"), out)
            value = json.loads(out.read_text())
            self.assertEqual(value["status"], "not_tested")
            self.assertEqual(value["model_requests"], 0)
            self.assertEqual(value["fresh_consumer_os"], "not_tested")

    def test_windows_startup_path_excludes_system32_developer_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            system = root / "Windows"
            def which(tool, *, path):
                return str(system / "System32/wsl.exe") if tool == "wsl" and "System32" in path else None
            with patch.object(startup, "os", SimpleNamespace(name="nt", environ={"SYSTEMROOT": str(system)})), \
                    patch.object(startup.shutil, "which", side_effect=which):
                env = startup.startup_environment(root)
            self.assertEqual(env["PATH"], str(root / "OS tools only"))
            self.assertEqual(env["SYSTEMROOT"], str(system))
            self.assertEqual(list((root / "OS tools only").iterdir()), [])

    def test_windows_start_uses_absolute_os_command_outside_application_path(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            with patch.object(startup.platform, "system", return_value="Windows"):
                manager = startup.Startup(root / "home", "owned-task", root / "app.exe",
                    {"HOME": str(root), "USERPROFILE": str(root), "SYSTEMROOT": str(root / "Windows"), "PATH": "empty"})
            with patch.object(manager, "inspect"), patch.object(startup, "command") as command:
                manager.start()
            self.assertEqual(command.call_args.args[0],
                [root / "Windows/System32/schtasks.exe", "/Run", "/TN", "owned-task"])

    def test_windows_task_identity_uses_actual_executable_and_unicode_argv(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            exe = root / "native café 子.exe"
            exe.write_bytes(b"test")
            home = root / "user's home café 子"
            ns = "http://schemas.microsoft.com/windows/2004/02/mit/task"
            document = ET.Element("Task", xmlns=ns)
            action = ET.SubElement(ET.SubElement(document, "Actions"), "Exec")
            ET.SubElement(action, "Command").text = str(exe)
            args = ET.SubElement(action, "Arguments")
            args.text = f'--home "{home}" serve'
            text = lambda: ET.tostring(document, encoding="unicode")
            self.assertTrue(startup.windows_task_matches(text(), exe, home))
            args.text = f'--home "{home}" other'
            self.assertFalse(startup.windows_task_matches(text(), exe, home))
            args.text = f'--home "{home}" serve'
            ET.SubElement(document.find("Actions"), "ComHandler")
            self.assertFalse(startup.windows_task_matches(text(), exe, home))

    def test_uncertain_stop_does_not_remove_or_delete_owned_installation(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manager = Mock()
            manager.exists.return_value = True
            manager.stop.side_effect = RuntimeError("manager status unknown")
            result = {}
            with patch.object(startup, "command") as command:
                with self.assertRaisesRegex(RuntimeError, "unknown"):
                    startup.cleanup_owned(manager, root / "app", root / "home", {}, None, root, result)
                command.assert_not_called()
            self.assertTrue((root / startup.RETAIN).is_file())
            self.assertEqual(result["retained_temporary_installation"], str(root))
            self.assertNotIn("cleanup", result)

    def test_successful_cleanup_stops_before_removal_and_confirms_absence(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manager = Mock()
            manager.exists.side_effect = [True, False]
            events = []
            manager.stop.side_effect = lambda: events.append("stop")
            result = {}
            with patch.object(startup, "command", side_effect=lambda *a, **k: events.append("remove")), \
                    patch.object(startup, "await_closed", side_effect=lambda *a: events.append("closed")):
                startup.cleanup_owned(manager, root / "app", root / "home", {}, ("https://fixture", object()), root, result)
            self.assertEqual(events, ["stop", "closed", "remove"])
            self.assertNotIn("retained_temporary_installation", result)

    def test_lost_registration_ack_still_cleans_only_owned_registration(self):
        # An absent entry is not created during cleanup, and needs no task stop.
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manager = Mock()
            manager.exists.return_value = False
            with patch.object(startup, "command") as command:
                startup.cleanup_owned(manager, root / "app", root / "home", {}, None, root, {})
            command.assert_not_called()
            manager.stop.assert_not_called()

    def test_temporary_files_preserved_only_for_uncertain_cleanup(self):
        with startup.temporary_installation() as root:
            (root / "fixture").write_text("owned")
        self.assertFalse(root.exists())
        with startup.temporary_installation() as retained:
            (retained / startup.RETAIN).write_text("unknown")
        self.assertTrue(retained.exists())
        # Test itself owns the retained demonstration directory.
        import shutil
        shutil.rmtree(retained)

    def test_healthy_app_does_not_substitute_for_manager_process_identity(self):
        manager = Mock()
        manager.state.return_value = {"active": True, "pid": 54321}
        with self.assertRaisesRegex(AssertionError, "Manager's process"):
            startup.Startup.await_active(manager, 12345)

    def test_windows_definition_change_is_rejected_even_with_same_fixed_command(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            executable = root / "app.exe"
            executable.write_bytes(b"fixture")
            home = root / "home"
            ns = "http://schemas.microsoft.com/windows/2004/02/mit/task"
            document = ET.Element("Task", xmlns=ns)
            actions = ET.SubElement(document, "Actions")
            action = ET.SubElement(actions, "Exec")
            ET.SubElement(action, "Command").text = str(executable)
            ET.SubElement(action, "Arguments").text = f'--home "{home}" serve'
            with patch.object(startup.platform, "system", return_value="Windows"):
                manager = startup.Startup(home, "unique-task", executable, {"HOME": str(root), "USERPROFILE": str(root)})
            before = ET.tostring(document, encoding="unicode")
            ET.SubElement(document, "Description").text = "Unexpected registration change"
            after = ET.tostring(document, encoding="unicode")
            with patch.object(manager, "task", side_effect=[{"xml": before, "enabled": True}, {"xml": after, "enabled": True}]):
                manager.inspect()
                with self.assertRaisesRegex(AssertionError, "definition changed"):
                    manager.inspect()


if __name__ == "__main__":
    unittest.main()
