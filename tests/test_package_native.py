"""Tiny synthetic artifacts test packaging boundaries, not native executability."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import shutil
import stat
import struct
import tarfile
import tempfile
import unittest
import zipfile


MODULE = Path(__file__).resolve().parents[1] / "scripts" / "package-native.py"
SPEC = importlib.util.spec_from_file_location("package_native", MODULE)
package_native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_native)


def synthetic_header(target):
    operating_system, arch, _ = package_native.TARGETS[target]
    data = bytearray(256)
    if operating_system == "linux":
        data[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<HH", data, 16, 3, 62 if arch == "x64" else 183)
    elif operating_system == "macos":
        data[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<III", data, 4, 0x01000007 if arch == "x64" else 0x0100000C, 0, 2)
    else:
        data[:2] = b"MZ"
        struct.pack_into("<I", data, 60, 64)
        data[64:68] = b"PE\0\0"
        struct.pack_into("<H", data, 68, 0x8664 if arch == "x64" else 0xAA64)
        struct.pack_into("<H", data, 86, 2)
        struct.pack_into("<H", data, 88, 0x20B)
    return bytes(data)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bridge = self.root / "bridge"
        self.modules = self.root / "installed modules"
        self.bridge.mkdir()
        self.modules.mkdir()
        self.sources = {"live.mjs": "import 'sdk';\n", "companion-mcp.mjs": "export {};\n",
                        "minecraft-observe.py": "# Optional connector source; no interpreter bundled.\n",
                        "package.json": json.dumps({"name": "fixture-bridge", "version": "1.0.0", "dependencies": {"sdk": "2.3.4"}}),
                        "package-lock.json": json.dumps({"lockfileVersion": 3, "packages": {
                            "": {"dependencies": {"sdk": "2.3.4"}},
                            "node_modules/sdk": {"version": "2.3.4", "resolved": "https://registry.example/sdk.tgz", "integrity": "sha512-fixture"},
                            "node_modules/dev-tool": {"version": "1.0.0", "dev": True},
                        }})}
        for name, content in self.sources.items():
            (self.bridge / name).write_text(content)
        for name, version in [("sdk", "2.3.4"), ("dev-tool", "1.0.0")]:
            p = self.modules / name
            p.mkdir()
            (p / "package.json").write_text(json.dumps({"name": name, "version": version, "license": "MIT", "main": "index.js"}))
            (p / "index.js").write_text("export const fixture = true;\n")
            (p / "LICENSE").write_text("Fixture license preserved\n")
        # npm-generated .bin links are not runtime dependencies or bundle inputs.
        (self.modules / ".bin").mkdir()
        try:
            (self.modules / ".bin" / "dev-tool").symlink_to(self.modules / "dev-tool" / "index.js")
        except OSError:
            (self.modules / ".bin" / "dev-tool").write_text("fixture shim")

    def pinned(self, name, contents):
        p = self.root / name
        p.write_bytes(contents)
        return {"path": str(p), "sha256": package_native.sha(contents)}

    def spec(self, target="linux-x64"):
        return {"schema_version": 1, "target": target, "version": "0.12.8", "source_revision": "a" * 40,
                "service": self.pinned("service", synthetic_header(target)),
                "node": {"executable": self.pinned("node", synthetic_header(target)), "version": "22.23.5",
                         "license": self.pinned("node-license", b"Node license\n")},
                "bridge": {"path": str(self.bridge), "source_sha256": package_native.tree_digest(package_native.bridge_files(self.bridge)),
                           "node_modules": str(self.modules), "node_modules_sha256": package_native.tree_digest(package_native.tree_files(self.modules))},
                "license": self.pinned("license", b"Microsoft MIT\n"),
                "third_party_notices": self.pinned("notices", b"Rust dependencies and license texts\n")}

    def build(self, spec=None, name="dist"):
        output = self.root / name
        receipt = package_native.build_bundle(spec or self.spec(), output)
        return output / receipt["archive"], receipt

    def repin_bridge(self, spec):
        spec["bridge"]["source_sha256"] = package_native.tree_digest(package_native.bridge_files(self.bridge))
        spec["bridge"]["node_modules_sha256"] = package_native.tree_digest(package_native.tree_files(self.modules))

    def test_all_six_headers_and_bundle_entrypoints(self):
        for target in package_native.TARGETS:
            with self.subTest(target=target):
                archive, receipt = self.build(self.spec(target), target)
                manifest, files = package_native.inspect_archive(archive, receipt["archive_sha256"])
                self.assertEqual(target, manifest["target"])
                self.assertEqual(package_native.sha(files["manifest.json"]), receipt["manifest_sha256"])
                self.assertIn(manifest["entrypoints"]["node"], files)
                self.assertEqual("not_established_by_packaging", manifest["qualification"]["clean_machine_launch"])
                self.assertFalse(any("dev-tool" in p or "/.bin/" in p for p in files))
                self.assertIn("bridge/node_modules/sdk/LICENSE", files)
                self.assertIn("licenses/Node-LICENSE", files)
                inventory = json.loads(files["licenses/npm-dependencies.json"])
                self.assertEqual("2.3.4", inventory[0]["version"])
                self.assertEqual("sha512-fixture", inventory[0]["integrity"])

    def test_relocation_has_no_build_paths_and_retains_executable_modes(self):
        archive, receipt = self.build()
        manifest, files = package_native.inspect_archive(archive, receipt["archive_sha256"])
        relocated = self.root / "relocated space é" / "bundle"
        relocated.mkdir(parents=True)
        # Validate every member first; never use archive.extractall on untrusted data.
        for name, data in files.items():
            path = relocated / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        for entrypoint in manifest["entrypoints"].values():
            self.assertTrue((relocated / entrypoint).is_file())
        self.assertNotIn(str(self.root), files["manifest.json"].decode())
        with tarfile.open(archive) as reader:
            self.assertEqual(0o755, reader.getmember("agent-conversations").mode)
            self.assertEqual(0o755, reader.getmember("runtime/node").mode)
            self.assertTrue(all(item.isfile() for item in reader))

    def test_same_inputs_produce_identical_archives(self):
        spec = self.spec()
        first, one = self.build(spec, "one")
        second, two = self.build(spec, "two")
        self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(one, two)

    def test_input_hash_mismatch_refuses_output(self):
        spec = self.spec()
        Path(spec["service"]["path"]).write_bytes(b"replaced")
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            self.build(spec)
        self.assertFalse((self.root / "dist").exists())

    def test_node_architecture_mismatch_refused(self):
        spec = self.spec()
        spec["node"]["executable"] = self.pinned("node", synthetic_header("linux-arm64"))
        with self.assertRaisesRegex(ValueError, "Node architecture"):
            self.build(spec)

    def test_same_cpu_wrong_os_refused(self):
        spec = self.spec()
        spec["service"] = self.pinned("service", synthetic_header("windows-x64"))
        with self.assertRaisesRegex(ValueError, "service architecture"):
            self.build(spec)

    def test_unknown_truncated_fat_and_library_headers_refused(self):
        for value in [b"#!/bin/sh\n", b"MZ", b"\xca\xfe\xba\xbe" + bytes(80), b"\x7fELF\x01\x01" + bytes(70)]:
            with self.subTest(value=value[:6]), self.assertRaises(ValueError):
                package_native.executable_identity(value)
        dll = bytearray(synthetic_header("windows-x64"))
        struct.pack_into("<H", dll, 86, 0x2002)
        with self.assertRaises(ValueError):
            package_native.executable_identity(dll)

    def test_missing_or_changed_production_dependency_refused(self):
        spec = self.spec()
        shutil.rmtree(self.modules / "sdk")
        self.repin_bridge(spec)
        with self.assertRaisesRegex(ValueError, "missing production dependency"):
            self.build(spec)

    def test_wrong_installed_version_refused(self):
        spec = self.spec()
        (self.modules / "sdk" / "package.json").write_text('{"name":"sdk","version":"9.0.0"}')
        self.repin_bridge(spec)
        with self.assertRaisesRegex(ValueError, "version mismatch"):
            self.build(spec)

    def test_missing_declared_runtime_entrypoint_refused(self):
        spec = self.spec()
        (self.modules / "sdk" / "index.js").unlink()
        self.repin_bridge(spec)
        with self.assertRaisesRegex(ValueError, "missing package entrypoint"):
            self.build(spec)

    def test_conditional_exports_and_cache_exclusion(self):
        spec = self.spec()
        p = self.modules / "sdk"
        descriptor = json.loads((p / "package.json").read_text())
        descriptor["exports"] = {".": {"import": "./index.js", "default": "./index.js"}, "./blocked": None}
        (p / "package.json").write_text(json.dumps(descriptor))
        (p / ".cache").mkdir()
        (p / ".cache" / "stale").write_text("generated artifact")
        self.repin_bridge(spec)
        archive, receipt = self.build(spec)
        _, files = package_native.inspect_archive(archive, receipt["archive_sha256"])
        self.assertFalse(any(".cache" in path for path in files))
        descriptor["exports"]["./missing"] = "./missing.js"
        (p / "package.json").write_text(json.dumps(descriptor))
        self.repin_bridge(spec)
        with self.assertRaisesRegex(ValueError, "missing package entrypoint"):
            self.build(spec, "invalid")

    def test_stale_dependency_tree_digest_refused(self):
        spec = self.spec()
        (self.modules / "sdk" / "index.js").write_text("changed")
        with self.assertRaisesRegex(ValueError, "tree digest mismatch"):
            self.build(spec)

    def test_dependency_and_lock_disagreement_refused(self):
        spec = self.spec()
        p = json.loads((self.bridge / "package.json").read_text())
        p["dependencies"]["sdk"] = "2.3.5"
        (self.bridge / "package.json").write_text(json.dumps(p))
        self.repin_bridge(spec)
        with self.assertRaisesRegex(ValueError, "dependencies differ"):
            self.build(spec)

    def test_incomplete_bridge_refused(self):
        (self.bridge / "companion-mcp.mjs").unlink()
        with self.assertRaisesRegex(ValueError, "incomplete bridge"):
            self.spec()

    def test_linked_dependency_or_native_addon_refused(self):
        spec = self.spec()
        try:
            (self.modules / "sdk" / "linked").symlink_to(self.root / "license")
        except OSError:
            self.skipTest("symlink creation not permitted on this test host")
        with self.assertRaisesRegex(ValueError, "regular input file"):
            self.repin_bridge(spec)
        (self.modules / "sdk" / "linked").unlink()
        (self.modules / "sdk" / "addon.node").write_bytes(b"unqualified native addon")
        self.repin_bridge(spec)
        with self.assertRaisesRegex(ValueError, "native npm dependency"):
            self.build(spec)

    def test_unsafe_relative_paths_rejected(self):
        for path in ["../escape", "/absolute", "x/../escape", "x\\escape", "C:escape", "CON", "x/evil.", "x//y"]:
            with self.subTest(path=path), self.assertRaises(ValueError):
                package_native.relative(path)

    def test_archive_links_and_traversal_rejected_before_extraction(self):
        for name, kind in [("../escape", tarfile.REGTYPE), ("safe", tarfile.SYMTYPE), ("safe", tarfile.LNKTYPE)]:
            path = self.root / "bad.tar.gz"
            with tarfile.open(path, "w:gz") as archive:
                item = tarfile.TarInfo(name)
                item.type = kind
                item.linkname = "../escape"
                item.size = 1 if kind == tarfile.REGTYPE else 0
                archive.addfile(item, io.BytesIO(b"x") if item.size else None)
            with self.subTest(name=name, kind=kind), self.assertRaises(ValueError):
                package_native.inspect_archive(path, package_native.sha(path.read_bytes()))
        path = self.root / "bad.zip"
        with zipfile.ZipFile(path, "w") as archive:
            info = zipfile.ZipInfo("link")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(info, "../escape")
        with self.assertRaisesRegex(ValueError, "nonregular"):
            package_native.inspect_archive(path, package_native.sha(path.read_bytes()))

    def test_changed_unlisted_or_duplicate_archive_member_refused(self):
        original, receipt = self.build()
        _, files = package_native.inspect_archive(original, receipt["archive_sha256"])
        for mode in ["changed", "extra", "duplicate"]:
            path = self.root / (mode + ".zip")
            payload = copy.deepcopy(files)
            if mode == "changed":
                payload["bridge/live.mjs"] = b"changed"
            if mode == "extra":
                payload["extra"] = b"unlisted"
            with zipfile.ZipFile(path, "w") as archive:
                for name, data in payload.items():
                    archive.writestr(name, data)
                if mode == "duplicate":
                    archive.writestr("MANIFEST.JSON", payload["manifest.json"])
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                package_native.inspect_archive(path, package_native.sha(path.read_bytes()))

    def test_output_cannot_overwrite_or_mutate_inputs(self):
        spec = self.spec()
        self.build(spec)
        with self.assertRaisesRegex(ValueError, "replace an existing"):
            self.build(spec)
        with self.assertRaisesRegex(ValueError, "mutate an input"):
            package_native.build_bundle(spec, self.bridge / "dist")


if __name__ == "__main__":
    unittest.main()
