"""Controller invariants; real executable behavior is tested by the CI harness."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_update_lifecycle", Path(__file__).with_name("native_update_lifecycle.py"))
lifecycle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lifecycle)


class LifecycleControllerTests(unittest.TestCase):
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
