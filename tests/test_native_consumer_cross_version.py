"""Offline input/worker binding tests; synthetic headers are never executed."""
import copy
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import unittest


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fixture = load("cross_version_fixture", "native_consumer_update.py")
packaging = load("cross_version_synthetic_packages", "test_package_native.py")


class ExplicitCandidateInputs(unittest.TestCase):
    def setUp(self):
        self.package = packaging.PackageTests("test_all_six_headers_and_bundle_entrypoints")
        self.package.setUp()
        self.addCleanup(self.package.doCleanups)
        self.root = self.package.root
        old_spec = self.package.spec("linux-arm64")
        self.a, old_receipt = self.package.build(old_spec, "old")
        self.manifest, self.contents = packaging.package_native.inspect_archive(self.a, old_receipt["archive_sha256"])
        new_spec = copy.deepcopy(old_spec)
        new_spec["source_revision"] = "b" * 40
        new_spec["service"] = self.package.pinned("new-service", packaging.synthetic_header("linux-arm64") + b"new source")
        self.c, self.new_receipt = self.package.build(new_spec, "new")
        self.receipt = self.c.with_suffix(".receipt.json")
        self.write_receipt(self.new_receipt)

    def write_receipt(self, value):
        self.receipt.write_text(json.dumps(value))

    def inspect(self, archive=None, receipt=None):
        return fixture.explicit_candidate(packaging.package_native, self.manifest, self.contents,
                                          archive or self.c, receipt or self.receipt)

    def test_default_unchanged_and_valid_distinct_candidate_keeps_exact_source(self):
        self.assertIsNone(fixture.explicit_candidate(packaging.package_native, self.manifest, self.contents, None, None))
        manifest, contents, provenance = self.inspect()
        self.assertEqual(manifest["source_revision"], "b" * 40)
        self.assertEqual(provenance["archive_sha256"], self.new_receipt["archive_sha256"])
        self.assertFalse(provenance["synthetic_candidate"])
        self.assertNotEqual(contents[manifest["entrypoints"]["service"]], self.contents[self.manifest["entrypoints"]["service"]])

    def test_partial_relative_and_linked_inputs_refused(self):
        with self.assertRaisesRegex(Exception, "supplied together"):
            fixture.explicit_candidate(packaging.package_native, self.manifest, self.contents, self.c, None)
        with self.assertRaisesRegex(Exception, "absolute"):
            self.inspect(archive=Path(self.c.name))
        linked = self.root / "linked-receipt.json"
        try:
            linked.symlink_to(self.receipt)
        except OSError:
            self.skipTest("Symlink creation unavailable for this test account")
        with self.assertRaisesRegex(Exception, "ordinary non-linked"):
            self.inspect(receipt=linked)

    def test_archive_manifest_target_and_source_receipt_mismatches_refused(self):
        for field, wrong in [("archive_sha256", "0" * 64), ("manifest_sha256", "0" * 64),
                             ("target", "windows-x64"), ("source_revision", "c" * 40)]:
            with self.subTest(field=field):
                self.write_receipt({**self.new_receipt, field: wrong})
                with self.assertRaises(Exception):
                    self.inspect()
        self.write_receipt(self.new_receipt)

    def test_different_manifest_with_same_binary_is_not_cross_version_proof(self):
        same = self.package.spec("linux-arm64")
        same["source_revision"] = "c" * 40
        archive, receipt = self.package.build(same, "same-service")
        path = self.root / "same-service-receipt.json"
        path.write_text(json.dumps(receipt))
        with self.assertRaisesRegex(Exception, "different actual service binary"):
            self.inspect(archive=archive, receipt=path)

    def test_worker_binding_uses_old_protocol_fields_and_rejects_helper_or_generation_drift(self):
        expected = self.root / "expected-package"
        expected.mkdir()
        manifest = {"source_revision": "a" * 40, "version": "0.12.8", "entrypoints": {"service": "app"}}
        (expected / "manifest.json").write_text(json.dumps(manifest))
        (expected / "app").write_bytes(b"not executed")
        home = self.root / "fixture-home"
        (home / "native-browser-update").mkdir(parents=True)
        identity = {"pid": 123, "birth": "captured generation"}
        operation = {"id": "operation", "transaction_id": "transaction",
                     "attempt": {"id": "attempt", "claimed": True, "worker_process": identity},
                     "helper": {"binary_sha256": fixture.lifecycle.digest((expected / "app").read_bytes()),
                                "bundle": {"manifest_sha256": fixture.lifecycle.digest((expected / "manifest.json").read_bytes()),
                                           "version": "0.12.8", "entrypoints": {"service": str(expected / "app")}}}}
        observer = SimpleNamespace(operation="operation", transaction="transaction", attempt="attempt",
                                   actual_worker_generation=fixture.fingerprint(identity))
        path = home / "native-browser-update/control.json"

        def write(value):
            path.write_text(json.dumps({"operations": {"operation": value}}))

        write(operation)
        bound = fixture.actual_helper_receipt(SimpleNamespace(home=home), observer, expected)
        self.assertEqual(bound["source_revision"], "a" * 40)
        self.assertFalse(bound["helper_override"])
        self.assertNotIn("launch_cleanup", operation)  # Old A needs no newly invented authority field.
        for changed in ["binary", "manifest", "generation", "claim"]:
            with self.subTest(changed=changed):
                bad = copy.deepcopy(operation)
                if changed == "binary":
                    bad["helper"]["binary_sha256"] = "0" * 64
                elif changed == "manifest":
                    bad["helper"]["bundle"]["manifest_sha256"] = "0" * 64
                elif changed == "generation":
                    bad["attempt"]["worker_process"]["birth"] = "another generation"
                else:
                    bad["attempt"]["claimed"] = False
                write(bad)
                with self.assertRaises(Exception):
                    fixture.actual_helper_receipt(SimpleNamespace(home=home), observer, expected)

    def test_actual_authenticated_status_provenance_required_after_old_package_restoration(self):
        expected = self.root / "expected-old-package"
        fixture.lifecycle.extract(self.contents, self.manifest, expected)
        home = self.root / "restored-old-home"
        home.mkdir()
        binary = fixture.lifecycle.digest(self.contents[self.manifest["entrypoints"]["service"]])
        manifest_sha = fixture.lifecycle.digest(self.contents["manifest.json"])
        package = {"binary_sha256": binary, "bundle": {"manifest_sha256": manifest_sha}}
        binding = {"installation_id": "installation", "transaction_id": "rollback", "nonce": "launch-nonce", "package": package}
        status = {"id": "installation", "binary_sha256": binary, "transaction_id": "rollback", "launch_nonce": "launch-nonce",
                  "pid": 731, "process_nonce": "actual-application-nonce", "admission_closed": True, "process_birth": None}
        receipt = {"binding": binding, "authority": {"binding": binding, "application": {"pid": 731, "birth": "native-generation"}},
                   "authority_source": {"kind": "authenticated_status", "first_status": status,
                                        "confirmed_status": copy.deepcopy(status), "restore_started": False},
                   "start": {"status": "accepted"}, "revoked": False, "stop": None, "exit_before_switch": None}
        journal = {"transaction_id": "rollback", "installation_id": "installation", "phase": "restored", "restore_outcome": "rolled_back",
                   "launch_nonce": "launch-nonce", "selected": "candidate", "candidate": package, "launch_cleanup": receipt}
        phase = {"transaction_id": "rollback", "phase": "rolled_back", "selected_manifest_sha256": manifest_sha,
                 "process": {"pid": 731, "binary_sha256": binary, "process_nonce_sha256": fixture.fingerprint("actual-application-nonce")},
                 "lifetime": {"replacement_application_identity_sha256": fixture.fingerprint(receipt["authority"]["application"])}}
        observer, install = SimpleNamespace(transaction="rollback"), SimpleNamespace(home=home)
        path = home / "native-update.json"
        path.write_text(json.dumps(journal))
        proof = fixture.authenticated_previous_receipt(install, observer, expected, phase)
        self.assertEqual(proof["kind"], "authenticated_status")
        self.assertTrue(proof["previous_restoration_separately_verified"])
        for changed in ["origin", "nonce", "pid", "binary", "not_restored", "same_pid_different_birth"]:
            with self.subTest(changed=changed):
                bad = copy.deepcopy(journal)
                source = bad["launch_cleanup"]["authority_source"]
                if changed == "origin":
                    source["kind"] = "process_entry"
                elif changed == "nonce":
                    source["confirmed_status"]["launch_nonce"] = "another-launch"
                elif changed == "pid":
                    source["confirmed_status"]["pid"] = 732
                elif changed == "binary":
                    source["confirmed_status"]["binary_sha256"] = "0" * 64
                elif changed == "same_pid_different_birth":
                    bad["launch_cleanup"]["authority"]["application"]["birth"] = "another-native-generation"
                else:
                    bad["phase"] = "verified"
                path.write_text(json.dumps(bad))
                with self.assertRaises(Exception):
                    fixture.authenticated_previous_receipt(install, observer, expected, phase)
        path.write_text(json.dumps(journal))
        (home / "native-update-entry-unexpected.json").write_text(json.dumps({"binding": binding}))
        with self.assertRaisesRegex(Exception, "entry despite authenticated-status"):
            fixture.authenticated_previous_receipt(install, observer, expected, phase)


if __name__ == "__main__":
    unittest.main()
