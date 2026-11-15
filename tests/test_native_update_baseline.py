"""Offline qualification-input checks; synthetic native headers are never run."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


baseline = load("native_baseline", ROOT / "scripts/native-update-baseline.py")
packaging = load("baseline_synthetic_packages", ROOT / "tests/test_package_native.py")


class Response(io.BytesIO):
    def __init__(self, data, url):
        super().__init__(data)
        self.url = url

    def geturl(self):
        return self.url


class NativeUpdateBaselineTests(unittest.TestCase):
    def setUp(self):
        self.package = packaging.PackageTests("test_all_six_headers_and_bundle_entrypoints")
        self.package.setUp()
        self.addCleanup(self.package.doCleanups)
        self.root = self.package.root
        self.lock = baseline.load_lock()
        self.target = "linux-arm64"
        self.assets, self.requests = {}, []
        self.output = self.root / "published baseline"
        self.lock_path = self.root / "baseline-lock.json"
        self.add_target(self.target)

    def add_target(self, target, *, version="0.13.4", source=None):
        spec = self.package.spec(target)
        spec.update(version=version, source_revision=source or self.lock["source_revision"])
        archive, receipt = self.package.build(spec, "synthetic-" + target)
        row = self.lock["targets"][target]
        # Public names stay fixed even in mismatch fixtures: content is verified.
        receipt["archive"] = row["archive"]["file"]
        raw = (json.dumps(receipt) + "\n").encode()
        row["archive"]["sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
        row["receipt"]["sha256"] = hashlib.sha256(raw).hexdigest()
        row["manifest_sha256"] = receipt["manifest_sha256"]
        self.assets[row["archive"]["file"]] = archive.read_bytes()
        self.assets[row["receipt"]["file"]] = raw

    def opener(self, url, *, timeout):
        self.assertEqual(timeout, 90)
        self.assertTrue(url.startswith(baseline.RELEASE))
        name = url.removeprefix(baseline.RELEASE)
        self.requests.append(name)
        return Response(self.assets[name], url)

    def acquire(self, target=None, *, opener=None, output=None):
        self.lock_path.write_text(json.dumps(self.lock), encoding="utf-8")
        with patch.object(baseline, "LOCK", self.lock_path):
            return baseline.acquire(target or self.target, output or self.output, opener=opener or self.opener)

    def test_complete_archive_and_receipt_closure_for_all_six_targets(self):
        for target in self.lock["targets"]:
            if target != self.target:
                self.add_target(target)
            with self.subTest(target=target):
                output = self.root / ("download-" + target)
                result = self.acquire(target, output=output)
                row = self.lock["targets"][target]
                self.assertEqual(self.requests[-2:], [row["receipt"]["file"], row["archive"]["file"]])
                self.assertEqual(result["source_revision"], self.lock["source_revision"])
                self.assertEqual(result["version"], "0.13.4")
                self.assertFalse(result["launched"])
                self.assertEqual(json.loads((output / "baseline.json").read_text()), result)
                self.assertEqual((output / row["archive"]["file"]).read_bytes(), self.assets[row["archive"]["file"]])
                self.assertEqual(len(list(output.iterdir())), 3)

    def test_existing_output_and_unknown_target_fail_before_network(self):
        self.output.mkdir()
        marker = self.output / "retained"
        marker.write_bytes(b"keep")
        with self.assertRaisesRegex(ValueError, "fresh absolute"):
            self.acquire()
        with self.assertRaisesRegex(ValueError, "Unknown native"):
            self.acquire("not-a-target", output=self.root / "other")
        self.assertEqual(self.requests, [])
        self.assertEqual(marker.read_bytes(), b"keep")

    def test_receipt_and_archive_checksum_failures_leave_no_success_receipt(self):
        for key in ["receipt", "archive"]:
            with self.subTest(key=key):
                row = self.lock["targets"][self.target]
                name = row[key]["file"]
                original = self.assets[name]
                self.assets[name] = original + b"changed"
                output = self.root / ("changed-" + key)
                with self.assertRaisesRegex(ValueError, "checksum"):
                    self.acquire(output=output)
                self.assertFalse((output / "baseline.json").exists())
                self.assertFalse((output / name).exists())
                self.assertFalse(list(output.glob(".download-*")))
                self.assets[name] = original

    def test_correct_receipt_hash_cannot_hide_wrong_source_or_target(self):
        row = self.lock["targets"][self.target]
        name = row["receipt"]["file"]
        original = json.loads(self.assets[name])
        for field, wrong in [("source_revision", "b" * 40), ("target", "linux-x64"),
                             ("manifest_sha256", "0" * 64), ("archive", "outside.tar.gz")]:
            with self.subTest(field=field):
                raw = json.dumps({**original, field: wrong}).encode()
                self.assets[name] = raw
                row["receipt"]["sha256"] = hashlib.sha256(raw).hexdigest()
                before = len(self.requests)
                with self.assertRaisesRegex(ValueError, "receipt differs"):
                    self.acquire(output=self.root / ("wrong-" + field))
                self.assertEqual(len(self.requests), before + 1)

    def test_valid_manifest_must_be_actual_predecessor_version(self):
        other = "linux-x64"
        self.add_target(other, version="0.13.3")
        with self.assertRaisesRegex(ValueError, "manifest differs"):
            self.acquire(other)
        self.assertFalse((self.output / "baseline.json").exists())

    def test_download_size_and_https_guards_prevent_publication(self):
        with patch.object(baseline, "MAX_RECEIPT", 4):
            with self.assertRaisesRegex(ValueError, "exceeds"):
                self.acquire()
        self.assertEqual(list(self.output.iterdir()), [])
        def downgraded(url, *, timeout):
            return Response(b"untrusted", "http://example.invalid/predecessor")
        output = self.root / "downgraded"
        with self.assertRaisesRegex(ValueError, "HTTPS"):
            self.acquire(output=output, opener=downgraded)
        self.assertEqual(list(output.iterdir()), [])

    def test_lock_refuses_arbitrary_urls_filenames_and_partial_targets(self):
        original = copy.deepcopy(self.lock)
        cases = [lambda x: x.update(release_url="https://example.invalid/latest/"),
                 lambda x: x["targets"][self.target]["archive"].update(file="../other.tar.gz"),
                 lambda x: x["targets"].pop("windows-arm64")]
        for index, mutate in enumerate(cases):
            self.lock = copy.deepcopy(original)
            mutate(self.lock)
            with self.subTest(index=index), self.assertRaises(ValueError):
                self.acquire(output=self.root / ("invalid-lock-" + str(index)))
        self.assertEqual(self.requests, [])


if __name__ == "__main__":
    unittest.main()
