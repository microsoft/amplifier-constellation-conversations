"""Bounded checks for the build/qualification harness; no native-pass claims."""
import hashlib
import importlib.util
import io
import json
import os
import ssl
import urllib.error
from pathlib import Path
import tarfile
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ci = load("native_ci", ROOT / "scripts/native-ci-inputs.py")
clean = load("native_clean", ROOT / "tests/native_clean_install.py")
imports = load("native_imports", ROOT / "scripts/check-native-imports.py")
rust_tests = load("native_rust_tests", ROOT / "scripts/run-native-rust-tests.py")


class NativeBuildHarnessTests(unittest.TestCase):
    def test_frozen_node_inputs_cover_exact_six_targets(self):
        data = json.loads((ROOT / "scripts/native-node-lock.json").read_text())
        self.assertEqual(set(data["targets"]), {"linux-x64", "linux-arm64", "macos-x64",
                                               "macos-arm64", "windows-x64", "windows-arm64"})
        self.assertTrue(data["source"].startswith("https://nodejs.org/dist/v"))
        for row in data["targets"].values():
            self.assertEqual(len(row["sha256"]), 64)
            self.assertIn("v" + data["version"], row["file"])

    def test_child_environment_has_no_provider_or_tool_configuration(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home, empty = root / "User", root / "Nothing"
            home.mkdir()
            empty.mkdir()
            with patch.dict(os.environ, {"OPENAI_API_KEY": "not-a-real-key", "NODE_PATH": "/host/modules",
                                         "PYTHONPATH": "/host/packages", "CARGO_HOME": "/host/cargo"}):
                env = clean.isolated_environment(home, empty)
            self.assertTrue(all(name not in env for name in ("OPENAI_API_KEY", "NODE_PATH", "PYTHONPATH", "CARGO_HOME")))
            self.assertEqual(env["PATH"], str(empty))
            self.assertEqual(env["HOME"], str(home))

    def test_architecture_identity_does_not_claim_x64_as_arm(self):
        with patch.object(clean.platform, "system", return_value="Windows"), patch.object(clean.platform, "machine", return_value="AMD64"):
            self.assertEqual(clean.native_target(), "windows-x64")
        with patch.object(clean.platform, "system", return_value="Windows"), patch.object(clean.platform, "machine", return_value="ARM64"):
            self.assertEqual(clean.native_target(), "windows-arm64")

    def test_node_extraction_keeps_only_exact_top_level_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts, output = root / "scripts", root / "output"
            scripts.mkdir()
            output.mkdir()
            name = "node-v22.23.5-linux-x64.tar.xz"
            archive = output / name
            with tarfile.open(archive, "w:xz") as stream:
                for path, contents in {"node-v22.23.5-linux-x64/bin/node": b"exact-node",
                                       "node-v22.23.5-linux-x64/LICENSE": b"exact-license",
                                       "node-v22.23.5-linux-x64/lib/node_modules/npm/LICENSE": b"unrelated-license",
                                       "../../outside": b"unrelated-bad-path"}.items():
                    info = tarfile.TarInfo(path)
                    info.size = len(contents)
                    stream.addfile(info, io.BytesIO(contents))
            lock = {"version": "22.23.5", "source": "https://nodejs.org/dist/v22.23.5/SHASUMS256.txt",
                    "targets": {"linux-x64": {"file": name, "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()}}}
            (scripts / "native-node-lock.json").write_text(json.dumps(lock))
            with patch.object(ci, "ROOT", root):
                result = ci.node_inputs("linux-x64", output)
            self.assertEqual(Path(result["executable"]["path"]).read_bytes(), b"exact-node")
            self.assertEqual(Path(result["license"]["path"]).read_bytes(), b"exact-license")
            self.assertFalse((root / "outside").exists())

    def test_corrupt_cached_node_cannot_be_executed_or_packaged(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            (root / "scripts/native-node-lock.json").write_text(json.dumps({"version": "22.23.5", "source": "https://nodejs.org/",
                "targets": {"linux-x64": {"file": "node.tar.xz", "sha256": "0" * 64}}}))
            (root / "node.tar.xz").write_bytes(b"corrupt")
            with patch.object(ci, "ROOT", root):
                with self.assertRaisesRegex(ValueError, "checksum"):
                    ci.node_inputs("linux-x64", root)
            self.assertFalse((root / "node").exists())

    def test_windows_developer_runtimes_are_not_assumed_to_be_os_files(self):
        for name in ["VCRUNTIME140.dll", "msvcp140.dll", "concrt140.dll", "ucrtbased.dll"]:
            self.assertEqual(imports.classify(name), "separate_redistributable_or_developer_runtime")
        self.assertEqual(imports.classify("api-ms-win-crt-runtime-l1-1-0.dll"), "windows_system")
        self.assertEqual(imports.classify("KERNEL32.dll"), "windows_system")
        for name in ["other-library.dll", "../kernel32.dll", "folder\\user32.dll"]:
            self.assertEqual(imports.classify(name), "unreviewed_import")

    def test_delay_imports_and_warnings_are_retained(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "fixture.exe"
            path.write_bytes(b"fixture-bytes")
            image = SimpleNamespace(FILE_HEADER=SimpleNamespace(Machine=0xAA64),
                                    OPTIONAL_HEADER=SimpleNamespace(Magic=0x20B),
                                    DIRECTORY_ENTRY_IMPORT=[SimpleNamespace(dll=b"KERNEL32.dll")],
                                    DIRECTORY_ENTRY_DELAY_IMPORT=[SimpleNamespace(dll=b"VCRUNTIME140.dll")],
                                    parse_data_directories=lambda directories: None,
                                    close=lambda: None, get_warnings=lambda: ["preserved warning"])
            reader = SimpleNamespace(PE=lambda *args, **kwargs: image,
                                     DIRECTORY_ENTRY={"IMAGE_DIRECTORY_ENTRY_IMPORT": 1, "IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT": 13})
            result = imports.inspect(path, hashlib.sha256(path.read_bytes()).hexdigest(), "arm64", reader)
            self.assertEqual(result["warnings"], ["preserved warning"])
            self.assertEqual(len(result["unresolved_dependencies"]), 1)
            self.assertTrue(result["unresolved_dependencies"][0]["delayed"])
            with self.assertRaisesRegex(ValueError, "architecture"):
                imports.inspect(path, result["sha256"], "x64", reader)
            with self.assertRaisesRegex(ValueError, "changed"):
                imports.inspect(path, "0" * 64, "arm64", reader)

    def test_empty_native_test_selection_cannot_be_reported_as_passing(self):
        with self.assertRaisesRegex(ValueError, "no passing"):
            rust_tests.passed_counts("test result: ok. 0 passed; 0 failed; 0 ignored; 137 filtered out;")
        self.assertEqual(rust_tests.passed_counts("test result: ok. 1 passed; 0 failed; 0 ignored; 136 filtered out;"),
                         {"passed": 1, "failed": 0, "ignored": 0})
        with self.assertRaisesRegex(ValueError, "successful"):
            rust_tests.passed_counts("test result: FAILED. 0 passed; 1 failed; 0 ignored;")

    def test_certificate_failures_are_not_retried_as_startup_delays(self):
        error = ssl.SSLCertVerificationError(1, "Missing Authority Key Identifier")
        self.assertTrue(clean.certificate_failure(error))
        self.assertTrue(clean.certificate_failure(urllib.error.URLError(error)))
        self.assertFalse(clean.certificate_failure(urllib.error.URLError(ConnectionRefusedError())))


if __name__ == "__main__":
    unittest.main()
