"""Controller failure boundaries; these do not claim native lifetime proof."""
import importlib.util
import hashlib
import http.server
import io
import json
from pathlib import Path
import queue
import stat
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import urllib.error
import urllib.request

spec = importlib.util.spec_from_file_location("native_consumer_update", Path(__file__).with_name("native_consumer_update.py"))
controller = importlib.util.module_from_spec(spec)
spec.loader.exec_module(controller)


class CheckObservationTests(unittest.TestCase):
    OLD_ID = "1c97701c-955f-4d8e-ae6a-ccf563e6687a"
    NEW_ID = "057c2e79-cc3d-4852-ae80-66b494de6a21"
    NONCE = "private-test-runtime-nonce"
    BUNDLE_VERIFICATION_PHASES = ("bundle_manifest", "bundle_inventory", "bundle_content", "bundle_entrypoints")

    def runtime(self, home, *, nonce=None, pid=101):
        (home / "runtime.json").write_text(json.dumps({"pid": pid, "process_nonce": nonce or self.NONCE,
            "private_extra": "runtime-private-extension"}))

    def snapshot(self, *, identity=None, nonce=None, pid=101):
        return {"schema": 1, "observation_only": True, "id": identity or self.NEW_ID, "pid": pid,
            "process_nonce_sha256": hashlib.sha256((nonce or self.NONCE).encode()).hexdigest(),
            "started_at_ms": 1794570000000, "queued_ms": 2, "elapsed_ms": 39000,
            "prior_observation_io_ms": 3, "state": "running", "result": None, "truncated": False,
            "events": [{"sequence": 1, "phase": "bundle_copy", "state": "started",
                        "at_ms": 25, "elapsed_ms": None, "result": None}]}

    def trace(self, home, value):
        path = home / "native-browser-update/check-observation.json"
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps(value))
        return path

    def installation(self, home, request):
        return SimpleNamespace(home=home, result={}, checkpoint=Mock(), browser_request=Mock(side_effect=request))

    def test_check_observation_timeout_retains_progress_without_authority(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            error = TimeoutError("original CHECK timeout")
            def request(*_args, **_kwargs):
                self.trace(home, self.snapshot())
                raise error
            install = self.installation(home, request)
            with patch.object(controller, "request_status", return_value={"owner": "standalone", "mode": "normal", "actions": {"check": True}}), \
                 patch.object(controller, "submit_once") as submit, self.assertRaises(TimeoutError) as caught:
                controller.exercise_api(install, Path("a"), Path("b"), Mock())
            self.assertIs(caught.exception, error)
            submit.assert_not_called()
            install.browser_request.assert_called_once_with("POST", controller.API + "/check", {"package_path": "b"}, timeout=40)
            row = install.result["check_request"]
            self.assertEqual(row["response"], "transport_unknown")
            self.assertEqual(row["mutation_retries"], 0)
            self.assertEqual(row["observation_before"]["status"], "absent")
            after = row["observation_after"]
            self.assertEqual(after["attribution"], "new_id_same_generation")
            self.assertEqual(after["snapshot"]["events"][0]["phase"], "bundle_copy")
            self.assertFalse(after["request_identity_bound"])
            self.assertEqual(install.checkpoint.call_count, 2)

    def test_check_observation_returned_ok_never_turns_timeout_into_success(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            value = {**self.snapshot(), "state": "returned", "result": "ok"}
            value["events"] = [{"sequence": index, "phase": phase, "state": "returned",
                                "at_ms": 10, "elapsed_ms": 2, "result": "ok"}
                               for index, phase in enumerate(self.BUNDLE_VERIFICATION_PHASES, 1)]
            def request(*_args, **_kwargs):
                self.trace(home, value)
                raise TimeoutError("no HTTP reply")
            install = self.installation(home, request)
            with self.assertRaises(TimeoutError):
                controller.check_once(install, Path("candidate"))
            row = install.result["check_request"]
            self.assertEqual(row["response"], "transport_unknown")
            self.assertEqual(row["observation_after"]["snapshot"]["result"], "ok")
            self.assertEqual(row["observation_after"]["snapshot"]["events"], value["events"])
            self.assertFalse(row["observation_after"]["request_identity_bound"])

    def test_check_observation_failures_preserve_original_request_error(self):
        with tempfile.TemporaryDirectory() as directory:
            original = TimeoutError("original request error")
            install = self.installation(Path(directory), original)
            install.checkpoint.side_effect = [None, RuntimeError("private checkpoint error")]
            with patch.object(controller, "check_observation", side_effect=RuntimeError("private diagnostic path")), \
                 patch.object(controller, "check_failure_ledger", side_effect=RuntimeError("private ledger error")), \
                 self.assertRaises(TimeoutError) as caught:
                controller.check_once(install, Path("candidate"))
            self.assertIs(caught.exception, original)
            install.browser_request.assert_called_once_with("POST", controller.API + "/check", {"package_path": "candidate"}, timeout=40)
            row = install.result["check_request"]
            self.assertEqual(row["response"], "transport_unknown")
            self.assertEqual(row["observation_after"]["status"], "unavailable")
            self.assertNotIn("private", json.dumps(row))

    def test_check_observation_cancellation_before_request_never_dispatches(self):
        for interruption in [KeyboardInterrupt(), SystemExit(13)]:
            with self.subTest(kind=type(interruption).__name__), tempfile.TemporaryDirectory() as directory:
                install = self.installation(Path(directory), {"unexpected": "reply"})
                with patch.object(controller, "check_observation", side_effect=interruption), \
                     self.assertRaises(type(interruption)) as caught:
                    controller.check_once(install, Path("candidate"))
                self.assertIs(caught.exception, interruption)
                install.browser_request.assert_not_called()
                install.checkpoint.assert_not_called()

    def test_check_observation_cancellation_after_reply_never_reaches_apply(self):
        for interruption in [KeyboardInterrupt(), SystemExit(13)]:
            with self.subTest(kind=type(interruption).__name__), tempfile.TemporaryDirectory() as directory:
                install = self.installation(Path(directory), Mock(return_value={"check": {"id": self.NEW_ID, "available": True}, "actions": {"apply": True}}))
                before = {"status": "absent", "runtime_generation": None}
                status = {"owner": "standalone", "mode": "normal", "actions": {"check": True}}
                with patch.object(controller, "check_observation", side_effect=[before, interruption]), \
                     patch.object(controller, "request_status", return_value=status), \
                     patch.object(controller, "submit_once", side_effect=AssertionError("APPLY reached after observation cancellation")) as submit, \
                     self.assertRaises(type(interruption)) as caught:
                    controller.exercise_api(install, Path("a"), Path("b"), Mock())
                self.assertIs(caught.exception, interruption)
                submit.assert_not_called()
                install.browser_request.assert_called_once_with("POST", controller.API + "/check", {"package_path": "b"}, timeout=40)
                self.assertEqual(install.result["check_request"]["response"], "received")
                self.assertEqual(install.checkpoint.call_count, 1)

    def test_check_observation_secondary_cancellation_preserves_request_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            original = TimeoutError("original request failure")
            install = self.installation(Path(directory), original)
            install.checkpoint.side_effect = [None, KeyboardInterrupt()]
            with patch.object(controller, "check_observation", side_effect=[{"status": "absent"}, SystemExit(13)]), \
                 patch.object(controller, "check_failure_ledger", side_effect=KeyboardInterrupt()), \
                 self.assertRaises(TimeoutError) as caught:
                controller.check_once(install, Path("candidate"))
            self.assertIs(caught.exception, original)
            self.assertEqual(install.browser_request.call_count, 1)
            self.assertEqual(install.result["check_request"]["observation_after"]["code"], "check_observation_capture_interrupted")

    def test_check_observation_success_preserves_reply_and_checkpoint_rules(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            reply = {"check": {"id": self.NEW_ID}, "actual_reply": True}
            def request(*_args, **_kwargs):
                self.trace(home, self.snapshot())
                return reply
            install = self.installation(home, request)
            self.assertIs(controller.check_once(install, Path("candidate")), reply)
            self.assertEqual(install.result["check_request"]["response"], "received")
            self.assertEqual(install.checkpoint.call_count, 2)
            for calls in [[RuntimeError("before dispatch")], [None, RuntimeError("after success")]]:
                with self.subTest(calls=len(calls)):
                    install = self.installation(home, request)
                    install.checkpoint.side_effect = calls
                    with self.assertRaises(RuntimeError):
                        controller.check_once(install, Path("candidate"))
                    self.assertEqual(install.browser_request.call_count, len(calls) - 1)

    def test_check_observation_absent_or_missing_generation_stays_unattributed(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            absent = controller.check_observation(home)
            self.assertEqual(absent["status"], "absent")
            self.assertIsNone(absent["runtime_generation"])
            self.trace(home, self.snapshot())
            value = controller.check_observation(home, before=absent)
            self.assertEqual(value["status"], "observed")
            self.assertEqual(value["attribution"], "unknown")
            self.assertIsNone(value["generation_matches"])
            self.runtime(home)
            before = controller.check_observation(home)
            self.trace(home, {**self.snapshot(identity=self.OLD_ID), "process_nonce_sha256": None})
            value = controller.check_observation(home, before=before)
            self.assertIsNone(value["generation_matches"])
            self.assertEqual(value["attribution"], "unknown")

    def test_check_observation_same_id_is_stale_even_if_progress_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            self.trace(home, self.snapshot())
            before = controller.check_observation(home)
            self.trace(home, {**self.snapshot(), "elapsed_ms": 40001, "state": "returned", "result": "ok"})
            value = controller.check_observation(home, before=before)
            self.assertEqual(value["id_relation"], "same")
            self.assertEqual(value["attribution"], "stale_id")
            self.assertFalse(value["request_identity_bound"])

    def test_check_observation_generation_drift_or_mismatch_stays_unknown(self):
        for change in ["runtime_between", "trace_pid", "trace_nonce", "runtime_during"]:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                self.runtime(home)
                before = controller.check_observation(home)
                value = self.snapshot()
                if change == "runtime_between":
                    self.runtime(home, nonce="new-generation")
                    value = self.snapshot(nonce="new-generation")
                elif change == "trace_pid":
                    value["pid"] = 202
                elif change == "trace_nonce":
                    value["process_nonce_sha256"] = "0" * 64
                self.trace(home, value)
                if change == "runtime_during":
                    original = controller._check_observation_bytes
                    def change_runtime(path, **kwargs):
                        raw = original(path, **kwargs)
                        if path.name == "check-observation.json":
                            self.runtime(home, nonce="changed-during-read")
                        return raw
                    with patch.object(controller, "_check_observation_bytes", side_effect=change_runtime):
                        observed = controller.check_observation(home, before=before)
                    self.assertEqual(observed["runtime_bracket"], "changed")
                else:
                    observed = controller.check_observation(home, before=before)
                self.assertEqual(observed["attribution"], "unknown")
                self.assertFalse(observed["request_identity_bound"])

    def test_check_observation_concurrent_file_replacement_is_unavailable(self):
        for action in ["replace", "delete"]:
            with self.subTest(action=action), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                self.runtime(home)
                trace = self.trace(home, self.snapshot())
                replacement = home / "replacement.json"
                replacement.write_text(json.dumps(self.snapshot(identity=self.OLD_ID)))
                real_lstat, reads = Path.lstat, []
                def lstat(path, *args, **kwargs):
                    if path == trace:
                        reads.append(path)
                        if len(reads) == 2:
                            replacement.replace(trace) if action == "replace" else trace.unlink()
                    return real_lstat(path, *args, **kwargs)
                with patch.object(Path, "lstat", lstat):
                    value = controller.check_observation(home)
                self.assertEqual(value["status"], "unavailable")
                self.assertNotIn("snapshot", value)

    def test_check_observation_reparse_and_nonordinary_files_are_refused(self):
        for mode, attributes in [(stat.S_IFREG | 0o600, 0x400), (stat.S_IFLNK | 0o600, 0), (stat.S_IFDIR | 0o700, 0)]:
            with self.subTest(mode=mode, attributes=attributes), tempfile.TemporaryDirectory() as directory:
                home = Path(directory)
                self.runtime(home)
                trace = self.trace(home, self.snapshot())
                real_lstat = Path.lstat
                def lstat(path, *args, **kwargs):
                    if path == trace:
                        return SimpleNamespace(st_mode=mode, st_file_attributes=attributes)
                    return real_lstat(path, *args, **kwargs)
                with patch.object(Path, "lstat", lstat):
                    value = controller.check_observation(home)
                self.assertEqual(value["status"], "unavailable")
                self.assertNotIn("snapshot", value)

    @unittest.skipIf(controller.os.name == "nt", "Actual Unix directory symlink; Windows reparse is covered by the portable metadata cases")
    def test_check_observation_actual_symlink_parent_is_refused(self):
        for target in ["home", "diagnostic"]:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                home = root / "home"
                home.mkdir()
                self.runtime(home)
                self.trace(home, self.snapshot())
                if target == "home":
                    linked = root / "linked-home"
                    linked.symlink_to(home, target_is_directory=True)
                    home = linked
                else:
                    parent = home / "native-browser-update"
                    destination = root / "external-directory"
                    parent.rename(destination)
                    parent.symlink_to(destination, target_is_directory=True)
                value = controller.check_observation(home)
                self.assertEqual(value["status"], "unavailable")
                self.assertNotIn("snapshot", value)

    def test_check_observation_parent_reparse_or_replacement_is_unavailable(self):
        for target in ["home", "diagnostic"]:
            for change in ["reparse", "replacement"]:
                with self.subTest(target=target, change=change), tempfile.TemporaryDirectory() as directory:
                    home = Path(directory)
                    self.runtime(home)
                    self.trace(home, self.snapshot())
                    parent = home if target == "home" else home / "native-browser-update"
                    real_lstat, reads = Path.lstat, []
                    def lstat(path, *args, **kwargs):
                        value = real_lstat(path, *args, **kwargs)
                        if path == parent:
                            reads.append(path)
                            # Replace between the trace's parent reads, after
                            # the separate runtime-generation bracket if home.
                            return SimpleNamespace(st_mode=value.st_mode, st_dev=value.st_dev,
                                st_ino=value.st_ino + (change == "replacement" and len(reads) >= (4 if target == "home" else 2)),
                                st_file_attributes=0x400 if change == "reparse" else 0)
                        return value
                    with patch.object(Path, "lstat", lstat):
                        value = controller.check_observation(home)
                    self.assertEqual(value["status"], "unavailable")
                    self.assertNotIn("snapshot", value)

    def test_check_observation_malformed_oversized_and_invalid_contract_is_unavailable(self):
        invalid = ["{malformed", " " * 32769]
        updates = [{"schema": True}, {"schema": 2}, {"id": "private-invalid-id"}, {"pid": True},
                   {"pid": 0}, {"pid": 1 << 32}, {"process_nonce_sha256": "A" * 64},
                   {"queued_ms": -1}, {"elapsed_ms": 1.5}, {"prior_observation_io_ms": 1 << 64},
                   {"state": "private-state"}, {"result": "ok"}, {"state": "returned", "result": None},
                   {"truncated": 1}]
        invalid.extend(json.dumps({**self.snapshot(), **update}) for update in updates)
        for update in [{"phase": "private-path"}, {"phase": "bundle_content/private-path"},
                       {"state": "private-state"}, {"sequence": 2},
                       {"at_ms": True}, {"elapsed_ms": 0}, {"result": "ok"}, {"state": "returned"}]:
            value = self.snapshot()
            value["events"][0].update(update)
            invalid.append(json.dumps(value))
        value = self.snapshot()
        value["events"] *= 65
        invalid.append(json.dumps(value))
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            path = self.trace(home, self.snapshot())
            for index, raw in enumerate(invalid):
                with self.subTest(index=index):
                    path.write_text(raw)
                    observed = controller.check_observation(home)
                    self.assertEqual(observed["status"], "unavailable")
                    self.assertNotIn("snapshot", observed)
                    self.assertNotIn("private", json.dumps(observed))

    def test_check_observation_truncation_and_private_extensions_are_preserved_safely(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            before = controller.check_observation(home)
            value = {**self.snapshot(), "truncated": True, "raw_nonce": self.NONCE,
                     "private_path": "private-source-path"}
            value["events"] = [{**value["events"][0], "sequence": i, "private_error": "private-error"} for i in range(1, 65)]
            self.trace(home, value)
            observed = controller.check_observation(home, before=before)
            self.assertTrue(observed["snapshot"]["truncated"])
            self.assertEqual(len(observed["snapshot"]["events"]), 64)
            self.assertEqual(observed["attribution"], "new_id_same_generation")
            self.assertFalse(observed["request_identity_bound"])
            rendered = json.dumps(observed)
            for private in [self.NONCE, "private-source-path", "private-error", "runtime-private-extension"]:
                self.assertNotIn(private, rendered)

    def test_bundle_verification_phases_keep_three_scans_within_fixed_limits(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            value = self.snapshot()
            events = [{**value["events"][0], "sequence": index} for index in range(1, 35)]
            for _ in range(3):
                for phase in self.BUNDLE_VERIFICATION_PHASES:
                    for state in ("started", "returned"):
                        events.append({"sequence": len(events) + 1, "phase": phase, "state": state,
                                       "at_ms": 25, "elapsed_ms": 2 if state == "returned" else None,
                                       "result": "ok" if state == "returned" else None})
            value["events"] = events
            path = self.trace(home, value)
            self.assertEqual(len(events), 58)
            self.assertLessEqual(path.stat().st_size, 32768)
            observed = controller.check_observation(home)
            self.assertEqual(observed["status"], "observed")
            self.assertEqual(observed["snapshot"]["events"], events)
            self.assertFalse(observed["snapshot"]["truncated"])
            self.assertEqual(observed["snapshot"]["state"], "running")
            self.assertIsNone(observed["snapshot"]["result"])
            self.assertTrue(observed["observation_only"])
            self.assertFalse(observed["request_identity_bound"])

            # Sequential records isolate the event bound from sequence checks.
            events.extend({**events[-1], "sequence": index} for index in range(59, 65))
            value["truncated"] = True
            self.trace(home, value)
            observed = controller.check_observation(home)
            self.assertEqual(observed["snapshot"]["events"], events)
            self.assertEqual(len(observed["snapshot"]["events"]), 64)
            self.assertTrue(observed["snapshot"]["truncated"])
            events.append({**events[-1], "sequence": 65})
            self.trace(home, value)
            observed = controller.check_observation(home)
            self.assertEqual(observed["status"], "unavailable")
            self.assertNotIn("snapshot", observed)

    def test_check_observation_unknown_prior_cannot_attribute_new_trace(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            path = self.trace(home, self.snapshot())
            path.write_text("malformed")
            before = controller.check_observation(home)
            self.trace(home, self.snapshot())
            observed = controller.check_observation(home, before=before)
            self.assertEqual(observed["id_relation"], "unknown_prior")
            self.assertEqual(observed["attribution"], "unknown")

    def windows_stat_sample(self, home, *, drift=None):
        path = self.trace(home, self.snapshot())
        expected = path.read_bytes()
        lstat, fstat = Path.lstat, controller.os.fstat
        reads = {"path": 0, "handle": 0}

        def metadata(value, family):
            reads[family] += 1
            fields = {key: getattr(value, key) for key in
                      ("st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns")}
            # CPython3.13.17 Windows path stat preserves legacy birthtime in
            # ctime; descriptor stat uses ChangeTime instead. Both are stable.
            fields.update(st_birthtime_ns=101, st_ctime_ns=101 if family == "path" else 202,
                          st_file_attributes=0)
            if drift:
                target, field, sample = drift
                if family == target and reads[family] >= sample:
                    if field == "missing_birthtime":
                        del fields["st_birthtime_ns"]
                    else:
                        fields[field] += 0o100 if field == "st_mode" else 1
            return SimpleNamespace(**fields)

        def path_stat(value, *args, **kwargs):
            result = lstat(value, *args, **kwargs)
            return metadata(result, "path") if value == path else result

        with patch.object(controller.os, "name", "nt"), patch.object(Path, "lstat", path_stat), \
             patch.object(controller.os, "fstat", side_effect=lambda fd: metadata(fstat(fd), "handle")):
            actual = controller._check_observation_bytes(path, parents=(home, path.parent))
        return actual, expected, reads

    def test_check_observation_windows_distinct_stable_clocks_keep_same_file(self):
        with tempfile.TemporaryDirectory() as directory:
            actual, expected, reads = self.windows_stat_sample(Path(directory))
            self.assertEqual(actual, expected)
            self.assertEqual(reads, {"path": 2, "handle": 2})

    def test_check_observation_windows_same_family_drift_remains_unknown(self):
        for family in ["path", "handle"]:
            for field in ["st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns",
                          "st_ctime_ns", "st_birthtime_ns"]:
                with self.subTest(family=family, field=field), tempfile.TemporaryDirectory() as directory:
                    with self.assertRaises(AssertionError):
                        self.windows_stat_sample(Path(directory), drift=(family, field, 2))

    def test_check_observation_windows_cross_family_mismatch_or_missing_birth_is_unknown(self):
        for family, field in [("handle", key) for key in
                              ["st_dev", "st_ino", "st_mode", "st_size", "st_mtime_ns", "st_birthtime_ns"]] + \
                             [(family, "missing_birthtime") for family in ["path", "handle"]]:
            with self.subTest(family=family, field=field), tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(AssertionError):
                    self.windows_stat_sample(Path(directory), drift=(family, field, 1))

    def test_check_observation_all_fixed_phases_and_returned_or_unwound_states(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            self.runtime(home)
            self.assertIn("bundle_verify_existing", controller.CHECK_OBSERVATION_PHASES)
            self.assertTrue(set(self.BUNDLE_VERIFICATION_PHASES).issubset(controller.CHECK_OBSERVATION_PHASES))
            for state, result in [("returned", "ok"), ("returned", "error"), ("unwound", None)]:
                with self.subTest(state=state, result=result):
                    value = {**self.snapshot(), "state": state, "result": result}
                    value["events"] = [{"sequence": index, "phase": phase, "state": "returned",
                                        "at_ms": 10, "elapsed_ms": 2, "result": "ok"}
                                       for index, phase in enumerate(sorted(controller.CHECK_OBSERVATION_PHASES), 1)]
                    self.trace(home, value)
                    observed = controller.check_observation(home)
                    self.assertEqual(observed["status"], "observed")
                    self.assertEqual(observed["snapshot"]["state"], state)
                    self.assertEqual(observed["snapshot"]["result"], result)
                    self.assertEqual(observed["attribution"], "unknown")
                    self.assertFalse(observed["request_identity_bound"])


class ConsumerUpdateControllerTests(unittest.TestCase):
    def cleanup_fixture(self, home):
        install = controller.consumer.ConsumerInstallation.__new__(controller.consumer.ConsumerInstallation)
        install.home, install.ports, install.result = home, set(), {"cleanup": {}}
        install.checkpoint = Mock()
        install.api = Mock(side_effect=AssertionError("retained read-only API connection reset"))
        install.running = Mock(side_effect=AssertionError("Cleanup must not request authenticated status"))
        install.command = Mock(side_effect=AssertionError("Cleanup must not dispatch a command"))
        config = {"id": "owned-installation", "transport": "local_http", "port": 31471,
                  "public_url": "http://127.0.0.1:31471"}
        runtime = {"url": config["public_url"], "public_url": config["public_url"], "port": config["port"],
                   "provider_id": "web:" + config["id"], "pid": 101, "process_nonce": "retained-generation"}
        controller.consumer.write_json(home / "config.json", config)
        controller.consumer.write_json(home / "runtime.json", runtime)
        (home / "service.lock").write_bytes(b"0")
        return install

    def cleanup_socket(self, codes, *, during=None):
        connection = Mock()
        connection.__enter__ = Mock(return_value=connection)
        connection.__exit__ = Mock(return_value=False)
        values = iter(codes)
        def connect(_endpoint):
            if during:
                during()
            return next(values)
        connection.connect_ex.side_effect = connect
        return connection

    def test_cleanup_after_health_to_api_reset_uses_closed_socket_and_real_lock_only(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            connection = self.cleanup_socket([controller.errno.ECONNREFUSED])
            # Use the real fixture's filesystem lock acquisition. The socket is
            # deterministic; no manager, service, or authenticated API runs.
            with patch.object(controller.socket, "socket", return_value=connection):
                result = controller.await_cleanup_stopped(install, stage="manager_stop")
            self.assertEqual(result["status"], "stopped")
            self.assertEqual(result["samples"], 1)
            self.assertTrue(result["listener_closed"] and result["service_lock_released"])
            self.assertEqual(result["authenticated_requests"], 0)
            install.running.assert_not_called()
            install.api.assert_not_called()
            install.command.assert_not_called()
            connection.connect_ex.assert_called_once_with(("127.0.0.1", 31471))

    def test_cleanup_waits_for_both_socket_refusal_and_lock_release(self):
        scenarios = [([0, controller.errno.ECONNREFUSED], [True, True]),
                     ([controller.errno.ECONNREFUSED] * 2, [False, True])]
        for codes, locks in scenarios:
            with self.subTest(codes=codes, locks=locks), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                install.service_lock_available = Mock(side_effect=locks)
                connection = self.cleanup_socket(codes)
                with patch.object(controller.socket, "socket", return_value=connection), patch.object(controller.time, "sleep"):
                    value = controller.await_cleanup_stopped(install, stage="maintenance_exit")
                self.assertEqual(value["status"], "stopped")
                self.assertEqual(value["samples"], 2)
                self.assertEqual(value["connection_attempts"], 2)
                self.assertEqual(install.service_lock_available.call_count, 2)
                install.running.assert_not_called()
                connection.settimeout.assert_not_called()
                self.assertTrue(all(call.args == (False,) for call in connection.setblocking.call_args_list))

    def test_cleanup_open_or_locked_or_timed_out_never_becomes_absence(self):
        for code, free in [(0, True), (controller.errno.ECONNREFUSED, False)]:
            with self.subTest(code=code, free=free), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                install.service_lock_available = Mock(return_value=free)
                connection = self.cleanup_socket([code])
                with patch.object(controller.socket, "socket", return_value=connection), \
                     patch.object(controller.time, "monotonic", side_effect=[0, 0, 0, 15, 15]), \
                     self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                row = install.result["cleanup"]["listener_closure"][0]
                self.assertEqual(row["status"], "unknown")
                self.assertNotIn("listener_closed", row)
                self.assertEqual(row["timeout_seconds"], 15)

    def test_cleanup_pending_then_open_cannot_prove_absence_before_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            install.service_lock_available = Mock(return_value=True)
            first, second = self.cleanup_socket([10035]), self.cleanup_socket([0])
            first.getsockopt.return_value = 0
            with patch.object(controller.socket, "socket", side_effect=[first, second]), \
                 patch.object(controller.select, "select", return_value=([], [first], [])), \
                 patch.object(controller.time, "monotonic", side_effect=[0, 0, 0, 0, 1, 2, 2, 15, 15]), \
                 patch.object(controller.time, "sleep"), \
                 self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                controller.await_cleanup_stopped(install, stage="manager_stop")
            row = install.result["cleanup"]["listener_closure"][0]
            self.assertEqual(row["status"], "unknown")
            self.assertEqual(row["listener"], "open")
            self.assertEqual(row["samples"], 2)
            self.assertEqual(row["connection_attempts"], 2)
            self.assertNotIn("listener_closed", row)
            install.api.assert_not_called()
            first.connect_ex.assert_called_once()
            second.connect_ex.assert_called_once()
            first.close.assert_called_once()
            second.close.assert_called_once()

    def test_cleanup_delayed_refusal_retains_one_pending_attempt_across_slices(self):
        for initial, ready_kind in [(10035, "exception"), (controller.errno.EINPROGRESS, "write"),
                                    (controller.errno.EWOULDBLOCK, "write")]:
            with self.subTest(initial=initial, ready_kind=ready_kind), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                connection = self.cleanup_socket([initial])
                connection.getsockopt.return_value = 10061
                clock = [0.0]
                polls = []
                def ready(readable, writable, exceptional, timeout):
                    self.assertEqual((readable, writable, exceptional), ([], [connection], [connection]))
                    self.assertGreater(timeout, 0)
                    self.assertLessEqual(timeout, 0.2)
                    connection.close.assert_not_called()
                    clock[0] += timeout
                    polls.append(timeout)
                    if len(polls) < 3:
                        return [], [], []
                    return ([], [], [connection]) if ready_kind == "exception" else ([], [connection], [])
                with patch.object(controller.socket, "socket", return_value=connection) as create, \
                     patch.object(controller.select, "select", side_effect=ready), \
                     patch.object(controller.time, "monotonic", side_effect=lambda: clock[0]):
                    result = controller.await_cleanup_stopped(install, stage="manager_stop")
                self.assertEqual(result["status"], "stopped")
                self.assertEqual(result["connection_attempts"], 1)
                self.assertEqual(result["poll_slices"], 3)
                self.assertGreater(result["elapsed_seconds"], 0.2)
                create.assert_called_once()
                connection.connect_ex.assert_called_once()
                connection.getsockopt.assert_called_once_with(controller.socket.SOL_SOCKET, controller.socket.SO_ERROR)
                connection.close.assert_called_once()
                install.api.assert_not_called()

    def test_cleanup_pending_until_deadline_never_reads_error_or_proves_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            connection = self.cleanup_socket([10035])
            clock = [0.0]
            def no_readiness(_read, _write, _exception, timeout):
                self.assertLessEqual(timeout, min(0.2, 15 - clock[0]))
                clock[0] += timeout
                return [], [], []
            with patch.object(controller.socket, "socket", return_value=connection) as create, \
                 patch.object(controller.select, "select", side_effect=no_readiness), \
                 patch.object(controller.time, "monotonic", side_effect=lambda: clock[0]), \
                 self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                controller.await_cleanup_stopped(install, stage="manager_stop")
            row = install.result["cleanup"]["listener_closure"][0]
            self.assertEqual(row["status"], "unknown")
            self.assertEqual(row["listener"], "pending")
            self.assertEqual(row["elapsed_seconds"], 15)
            self.assertNotIn("listener_closed", row)
            create.assert_called_once()
            connection.connect_ex.assert_called_once()
            connection.getsockopt.assert_not_called()
            connection.close.assert_called_once()

    def test_cleanup_ready_error_or_ambiguous_zero_remains_unknown(self):
        for readiness, code in [("exception", 0), ("both", 0), ("write", 10054), ("exception", 10013), ("exception", 10035)]:
            with self.subTest(readiness=readiness, code=code), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                connection = self.cleanup_socket([10035])
                connection.getsockopt.return_value = code
                ready = ([], [connection] if readiness in {"write", "both"} else [],
                         [connection] if readiness in {"exception", "both"} else [])
                with patch.object(controller.socket, "socket", return_value=connection), \
                     patch.object(controller.select, "select", return_value=ready), \
                     self.assertRaisesRegex(AssertionError, "unknown after socket readiness"):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                row = install.result["cleanup"]["listener_closure"][0]
                self.assertEqual(row["status"], "unknown")
                self.assertNotIn("listener_closed", row)
                connection.connect_ex.assert_called_once()
                connection.close.assert_called_once()
                install.api.assert_not_called()

    def test_cleanup_poll_or_error_query_failure_closes_only_owned_probe(self):
        for failure in ["select", "socket_error"]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                connection = self.cleanup_socket([10035])
                error = OSError(controller.errno.EACCES, "fixture observation refused")
                if failure == "socket_error":
                    connection.getsockopt.side_effect = error
                with patch.object(controller.socket, "socket", return_value=connection), \
                     patch.object(controller.select, "select", side_effect=error if failure == "select" else None,
                                  return_value=([], [], [connection])), \
                     self.assertRaises(OSError):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                self.assertEqual(install.result["cleanup"]["listener_closure"][0]["status"], "unknown")
                connection.connect_ex.assert_called_once()
                connection.close.assert_called_once()
                install.api.assert_not_called()
                install.command.assert_not_called()

    def test_cleanup_identity_drift_during_pending_poll_retains_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            connection = self.cleanup_socket([10035])
            def drift(*_args):
                path = install.home / "runtime.json"
                runtime = controller.read_json(path)
                controller.consumer.write_json(path, {**runtime, "process_nonce": "changed"})
                return [], [], []
            with patch.object(controller.socket, "socket", return_value=connection), \
                 patch.object(controller.select, "select", side_effect=drift), \
                 self.assertRaisesRegex(AssertionError, "identity changed"):
                controller.await_cleanup_stopped(install, stage="manager_stop")
            self.assertEqual(install.result["cleanup"]["listener_closure"][0]["status"], "unknown")
            connection.getsockopt.assert_not_called()
            connection.close.assert_called_once()
            install.api.assert_not_called()

    def test_cleanup_actual_closed_loopback_endpoint_and_private_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            # Release the reservation before observing a closed endpoint. A
            # bound socket that remains open is not that fixture precondition.
            # Port reuse remains a failure; no new port or retry is selected.
            with controller.socket.socket() as reserved:
                reserved.bind(("127.0.0.1", 0))
                port = reserved.getsockname()[1]
            self.assertEqual(reserved.fileno(), -1)
            install = self.cleanup_fixture(Path(directory))
            for name in ["config.json", "runtime.json"]:
                path = install.home / name
                value = controller.read_json(path)
                value.update(port=port, public_url=f"http://127.0.0.1:{port}")
                if name == "runtime.json":
                    value["url"] = value["public_url"]
                controller.consumer.write_json(path, value)
            try:
                result = controller.await_cleanup_stopped(install, stage="manager_stop")
            except Exception as error:
                rows = install.result["cleanup"].get("listener_closure", [])
                row = rows[-1] if rows else {}
                keys = ("stage", "status", "timeout_seconds", "elapsed_seconds",
                        "samples", "connection_attempts", "poll_slices", "listener",
                        "service_lock_available", "authenticated_requests", "socket_error",
                        "listener_closed", "service_lock_released", "error_type")
                evidence = {key: row[key] for key in keys if key in row}
                error.add_note("Closed-loopback observation: " + json.dumps(evidence, sort_keys=True)[:1024])
                raise
            self.assertEqual(result["status"], "stopped")
            self.assertEqual(result["listener"], "refused")
            self.assertTrue(result["service_lock_available"])
            self.assertEqual(result["authenticated_requests"], 0)
            install.api.assert_not_called()
            install.command.assert_not_called()

    def test_cleanup_unknown_socket_error_is_not_swallowed_or_retried(self):
        for code in [controller.errno.EACCES, controller.errno.ECONNRESET, controller.errno.ETIMEDOUT, 10013, 10054]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                install.service_lock_available = Mock()
                connection = self.cleanup_socket([code])
                with patch.object(controller.socket, "socket", return_value=connection), \
                     self.assertRaisesRegex(AssertionError, "unknown after socket error"):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                row = install.result["cleanup"]["listener_closure"][0]
                self.assertEqual(row["status"], "unknown")
                self.assertEqual(row["socket_error"], code)
                self.assertEqual(connection.connect_ex.call_count, 1)
                install.service_lock_available.assert_not_called()
                install.api.assert_not_called()

    def test_cleanup_missing_malformed_or_drifted_identity_cannot_prove_stopped(self):
        cases = [("config.json", {"id": "another-installation"}),
                 ("config.json", {"public_url": "http://example.invalid:31471"}),
                 ("config.json", {"transport": "https"}),
                 ("runtime.json", {"url": "http://localhost:31471"}),
                 ("runtime.json", {"port": "31471"}),
                 ("runtime.json", {"provider_id": "web:other"}),
                 ("runtime.json", {"pid": 202}),
                 ("runtime.json", {"process_nonce": "different-generation"}),
                 ("runtime.json", None), ("service.lock", None)]
        for name, changes in cases:
            with self.subTest(name=name, changes=changes), tempfile.TemporaryDirectory() as directory:
                install = self.cleanup_fixture(Path(directory))
                def change():
                    path = install.home / name
                    if changes is None:
                        path.unlink()
                    else:
                        value = controller.read_json(path)
                        controller.consumer.write_json(path, {**value, **changes})
                connection = self.cleanup_socket([controller.errno.ECONNREFUSED], during=change)
                with patch.object(controller.socket, "socket", return_value=connection), \
                     self.assertRaises((AssertionError, OSError)):
                    controller.await_cleanup_stopped(install, stage="manager_stop")
                self.assertEqual(install.result["cleanup"]["listener_closure"][0]["status"], "unknown")
                install.running.assert_not_called()

    def test_cleanup_deadline_cannot_dispatch_socket_after_slow_identity_read(self):
        with tempfile.TemporaryDirectory() as directory:
            install = self.cleanup_fixture(Path(directory))
            with patch.object(controller.socket, "socket") as connection, \
                 patch.object(controller.time, "monotonic", side_effect=[0, 0, 15, 15]), \
                 self.assertRaisesRegex(AssertionError, "within 15 seconds"):
                controller.await_cleanup_stopped(install, stage="manager_stop")
            connection.assert_not_called()
            self.assertEqual(install.result["cleanup"]["listener_closure"][0]["status"], "unknown")

    def test_check_accepts_one_reply_after_ordinary_http_limit_within_ui_allowance(self):
        requests = []
        checked = {"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}

        class DelayedCheck(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                requests.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                # Exercise a real delayed HTTP status, beyond the former 15s
                # fixture limit but within the update page's existing 40s.
                time.sleep(16)
                body = json.dumps(checked).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        with tempfile.TemporaryDirectory() as directory, http.server.HTTPServer(("127.0.0.1", 0), DelayedCheck) as server:
            server.timeout = 20
            home = Path(directory)
            (home / "config.json").write_text(json.dumps({"public_url": f"http://127.0.0.1:{server.server_port}"}))
            install = controller.consumer.ConsumerInstallation.__new__(controller.consumer.ConsumerInstallation)
            install.home, install.csrf, install.result, install.checkpoint = home, None, {}, Mock()
            install.browser = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            thread = threading.Thread(target=server.handle_request)
            thread.start()
            try:
                self.assertEqual(controller.check_once(install, Path("candidate")), checked)
            finally:
                thread.join(timeout=20)
                self.assertFalse(thread.is_alive())
            self.assertEqual(requests, [(controller.API + "/check", {"package_path": "candidate"})])
            self.assertEqual(install.result["check_request"]["response"], "received")
            self.assertGreaterEqual(install.result["check_request"]["elapsed_seconds"], 15)
            self.assertLess(install.result["check_request"]["elapsed_seconds"], 40)

    def test_unknown_check_retains_only_bounded_ledger_evidence_without_apply(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / "native-browser-update").mkdir()
            (home / "config.json").write_text(json.dumps({"id": "installation"}))
            check_id = "f9b9dd53-f007-4d83-b4e8-90d56046e943"
            ledger = home / "native-browser-update/control.json"
            ledger.write_text(json.dumps({"schema": 1, "installation_id": "installation",
                "current_check": check_id, "checks": {check_id: {"plan": "private-package-path"}},
                "current_operation": None, "operations": {}, "private": "secret-nonce"}))
            install = SimpleNamespace(home=home, result={}, checkpoint=Mock(),
                browser_request=Mock(side_effect=TimeoutError("private socket error")))
            with patch.object(controller, "request_status", return_value={"owner": "standalone", "mode": "normal", "actions": {"check": True}}), patch.object(controller, "submit_once") as submit:
                with self.assertRaises(TimeoutError):
                    controller.exercise_api(install, Path("a"), Path("b"), Mock())
                submit.assert_not_called()
            install.browser_request.assert_called_once()
            row = install.result["check_request"]
            self.assertEqual(row["response"], "transport_unknown")
            self.assertEqual(row["mutation_retries"], 0)
            self.assertEqual(row["ledger_after_failure"]["current_check_id"], check_id)
            self.assertTrue(row["ledger_after_failure"]["current_check_record_present"])
            self.assertNotIn("private", json.dumps(row))
            self.assertNotIn("secret-nonce", json.dumps(row))
            ledger.write_text(json.dumps({"schema": 1, "installation_id": "installation",
                "current_check": "private-malformed-id", "current_operation": None}))
            self.assertEqual(controller.check_failure_ledger(home),
                {"status": "unavailable", "code": "browser_ledger_observation_failed"})

    def test_task_engine_without_actual_worker_cannot_prove_survival(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            ledger = home / "native-browser-update/control.json"
            ledger.parent.mkdir()
            ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
                "transaction_id": "transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
            observer = controller.NativeObserver.__new__(controller.NativeObserver)
            observer.install = SimpleNamespace(home=home)
            observer.previous_operation = observer.operation = observer.attempt = observer.transaction = None
            observer.worker_generation = observer.actual_worker_generation = observer.worker_observation = None
            observer.old_identity = {"pid": 1, "birth": "old"}
            observer.old_exit = observer.survived_old_exit = False
            observer.observations = []
            engine = {"status": "observed", "profile_matches": True, "active": True, "transitioning": False,
                      "processes": [{"identity": {"pid": 2, "birth": "engine"}}], "previous_process_alive": False}
            observer.call = Mock(return_value=engine)
            observer.sample()
            self.assertFalse(observer.survived_old_exit)
            self.assertIsNone(observer.actual_worker_generation)
            observer.call.return_value = {**engine, "actual_worker_identity": {"pid": 3, "birth": "worker"},
                                          "actual_worker_alive": True, "actual_worker_manager_binding": "descendant_of_task_engine"}
            observer.sample()
            self.assertTrue(observer.survived_old_exit)
            self.assertIsNotNone(observer.actual_worker_generation)

    def observer_read_fixture(self, home):
        ledger = home / "native-browser-update/control.json"
        ledger.parent.mkdir()
        ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
            "transaction_id": "transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
        observer = controller.NativeObserver.__new__(controller.NativeObserver)
        observer.install = SimpleNamespace(home=home)
        observer.previous_operation = observer.operation = observer.attempt = observer.transaction = None
        observer.worker_generation = observer.actual_worker_generation = observer.worker_observation = None
        observer.old_identity = {"pid": 1, "birth": "old"}
        observer.old_exit = observer.survived_old_exit = False
        observer.observations = []
        worker = {"status": "observed", "profile_matches": True, "active": True, "transitioning": False,
                  "processes": [{"identity": {"pid": 2, "birth": "engine"}}], "previous_process_alive": False,
                  "actual_worker_identity": {"pid": 3, "birth": "worker"}, "actual_worker_alive": True,
                  "actual_worker_manager_binding": "descendant_of_task_engine"}
        observer.call = Mock(return_value=worker)
        return observer, ledger, worker

    @staticmethod
    def observer_proof(observer):
        fields = ["previous_operation", "operation", "attempt", "transaction", "worker_generation",
                  "actual_worker_generation", "worker_observation", "old_identity", "old_exit",
                  "survived_old_exit", "observations"]
        return json.dumps({key: getattr(observer, key) for key in fields}, sort_keys=True)

    def test_unavailable_ledger_sample_advances_no_proof_and_next_sample_is_fresh(self):
        for error in [PermissionError(13, "fixture private reader unavailable"), FileNotFoundError("fixture replaced")]:
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as directory:
                observer, _, _ = self.observer_read_fixture(Path(directory))
                before = self.observer_proof(observer)
                with patch.object(controller, "read_json", side_effect=error):
                    self.assertFalse(observer.sample())
                self.assertEqual(self.observer_proof(observer), before)
                observer.call.assert_not_called()
                self.assertTrue(observer.sample())
                self.assertTrue(observer.old_exit and observer.survived_old_exit)
                self.assertIsNotNone(observer.actual_worker_generation)
                observer.call.assert_called_once()

    def test_persistent_unavailable_ledger_cannot_reuse_stale_exit_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, worker = self.observer_read_fixture(Path(directory))
            self.assertTrue(observer.sample())
            observer.worker_observation = {**worker, "active": False, "processes": [], "actual_worker_alive": False}
            before = self.observer_proof(observer)
            observer.call.reset_mock()
            with patch.object(controller, "read_json", side_effect=PermissionError(13, "unavailable")), \
                 patch.object(controller.time, "monotonic", side_effect=[0, 0, 1, 40]), \
                 patch.object(controller.time, "sleep"), patch.object(controller, "submit_once") as submit:
                with self.assertRaisesRegex(TimeoutError, "Actual worker exit is unobserved"):
                    observer.settled({"id": "operation", "transaction_id": "transaction"}, {})
                submit.assert_not_called()
            self.assertEqual(self.observer_proof(observer), before)
            observer.call.assert_not_called()

    def test_terminal_journal_cannot_supply_missed_worker_lifetime_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, _, worker = self.observer_read_fixture(Path(directory))
            with patch.object(controller, "read_json", side_effect=PermissionError(13, "unavailable")):
                self.assertFalse(observer.sample())
            observer.call.return_value = {**worker, "active": False, "processes": [], "actual_worker_alive": False}
            with self.assertRaisesRegex(AssertionError, "survival after old-parent exit"):
                observer.settled({"id": "operation", "transaction_id": "transaction", "phase": "completed", "outcome": "applied"}, {})
            self.assertFalse(observer.survived_old_exit)
            self.assertIsNone(observer.worker_generation)
            observer.call.assert_called_once()

    def test_malformed_or_rebound_ledger_still_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, ledger, _ = self.observer_read_fixture(Path(directory))
            self.assertTrue(observer.sample())
            before = self.observer_proof(observer)
            observer.call.reset_mock()
            ledger.write_text("{invalid-json")
            with self.assertRaises(json.JSONDecodeError):
                observer.sample()
            ledger.write_text(json.dumps({"current_operation": "operation", "operations": {"operation": {
                "transaction_id": "another-transaction", "attempt": {"id": "attempt", "claimed": True}}}}))
            with self.assertRaisesRegex(AssertionError, "Operation or launch attempt changed"):
                observer.sample()
            self.assertEqual(self.observer_proof(observer), before)
            observer.call.assert_not_called()

    def test_legacy_startup_success_does_not_require_json_output(self):
        install = SimpleNamespace(worker=Path("app"), home=Path("owned-home"),
                                  begin_operation=Mock(return_value={}), end_operation=Mock())
        native = SimpleNamespace(command=Mock(return_value=SimpleNamespace(returncode=0, stdout="", stderr="")))
        with patch.object(controller, "startup_module", return_value=native):
            controller.startup_command(install, {"PATH": "fixed-os-tools"}, "install")
        native.command.assert_called_once_with([Path("app"), "--home", Path("owned-home"), "startup", "install"],
            env={"PATH": "fixed-os-tools"}, check=False, timeout=30)
        self.assertEqual(install.end_operation.call_args.args[2:], ("succeeded", 0))

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
        with tempfile.TemporaryDirectory() as directory:
            install = SimpleNamespace(home=Path(directory), result={}, checkpoint=Mock(), browser_request=Mock(return_value={"check": {"available": True, "id": "checked"}, "actions": {"apply": True}}))
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
