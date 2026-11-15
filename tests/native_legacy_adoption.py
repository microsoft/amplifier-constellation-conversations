#!/usr/bin/env python3
"""Disposable Linux custom-unit legacy adoption qualification.

Requires independently reviewed exact old service, candidate package and a
separately identified test-only prestatus fault package. No existing home is an
input. No login, model, ACP request, Companion configuration or production unit
is used. Failed/unknown installations are retained for explicit owner review.
"""
from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import socket
import sqlite3
import ssl
import stat
import subprocess
import tempfile
import time
import uuid

LEGACY_TABLES = set("agent_endpoints attachments audit browser_sessions conversations grants import_sources imports input_authority input_deliveries login_attempts messages migrations room_agents room_members tokens views sqlite_sequence".split())


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)


def read_json_bytes(path, *, max_bytes=1024 * 1024):
    require(type(max_bytes) is int and max_bytes > 0, "Invalid fixture JSON bound")
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode), "Expected ordinary fixture JSON")
    require(before.st_size <= max_bytes, "Fixture JSON exceeds bound")
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NOFOLLOW), "rb") as stream:
        opened = os.fstat(stream.fileno())
        require(stat.S_ISREG(opened.st_mode), "Expected ordinary fixture JSON")
        require((opened.st_dev, opened.st_ino) == (before.st_dev, before.st_ino),
                "Fixture JSON changed before reading")
        require(opened.st_size <= max_bytes, "Fixture JSON exceeds bound")
        data = stream.read(max_bytes + 1)
        after_read = os.fstat(stream.fileno())
    after = path.lstat()
    require(stat.S_ISREG(after.st_mode), "Expected ordinary fixture JSON")
    require(len(data) <= max_bytes, "Fixture JSON exceeds bound")
    identity = lambda value: (value.st_dev, value.st_ino, value.st_mode, value.st_size,
                              value.st_mtime_ns, value.st_ctime_ns)
    require(identity(before) == identity(opened) == identity(after_read) == identity(after),
            "Fixture JSON changed while reading")
    return data


def read_json(path, *, max_bytes=1024 * 1024):
    return json.loads(read_json_bytes(path,max_bytes=max_bytes).decode("utf-8"))


def environment():
    # Preserve the real account HOME and manager address; never impersonate a
    # different account or inherit provider credentials into product processes.
    result = {k: os.environ[k] for k in ("HOME", "USER", "LOGNAME", "XDG_RUNTIME_DIR",
              "DBUS_SESSION_BUS_ADDRESS", "LANG", "LC_ALL") if k in os.environ}
    result["PATH"] = "/usr/bin:/bin"
    return result


def manager(arguments, *, allowed=(0,), timeout=15):
    result = subprocess.run(["/usr/bin/systemctl", "--user", *arguments],
                            capture_output=True, env=environment(), timeout=timeout)
    require(result.returncode in allowed, "Exact fixture manager command failed; no retry")
    return result.stdout.decode("utf-8", errors="strict").strip()


def properties(unit):
    fields = ("Id", "LoadState", "ActiveState", "SubState", "MainPID", "ControlPID",
              "ControlGroup", "UnitFileState", "FragmentPath", "DropInPaths", "NeedDaemonReload",
              "KillMode", "Restart", "RefuseManualStart", "RefuseManualStop", "ExecStart")
    text = manager(["show", unit, *["--property=" + name for name in fields]])
    result = dict(line.split("=", 1) for line in text.splitlines())
    require(set(result) == set(fields), "Incomplete fixed manager observation")
    return result


def start_static_fixture(unit):
    # A disabled installation is a preserved policy refusal in the shared
    # native Start API. This manually started disposable unit is static; it has
    # no [Install] section and must never be enabled to make the test pass.
    require(properties(unit)["UnitFileState"] == "static",
            "Fixture requires static registration; disabled policy is preserved")
    manager(["start", unit])


def custom_unit_definition(home, installed):
    # This disposable fixture uses generated ordinary paths. WorkingDirectory
    # is a raw systemd path directive, not ExecStart's quoted argv syntax.
    require(home.is_absolute() and ".." not in home.parts
            and re.fullmatch(r"/[A-Za-z0-9_./%\-]+", str(home)) is not None,
            "Fixture working directory must be a safe generated absolute path")

    def quote(value):
        require("\n" not in str(value), "Unit value contains newline")
        return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%") + '"'

    working_directory = str(home).replace("%", "%%")
    return ("[Unit]\nDescription=Disposable legacy adoption custom profile\n"
        "[Service]\nType=simple\nRestart=no\nKillMode=control-group\nTimeoutStopSec=8\n"
        "Environment=CONSTELLATION_LEGACY_FIXTURE=synthetic\n"
        "UnsetEnvironment=OPENAI_API_KEY AZURE_OPENAI_API_KEY ANTHROPIC_API_KEY GOOGLE_API_KEY GEMINI_API_KEY AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN\n"
        f"WorkingDirectory={working_directory}\nExecStart={quote(installed)} --home {quote(home)} serve\n").encode()


def process_identity(pid):
    try:
        raw = Path(f"/proc/{pid}/stat").read_text()
        fields = raw[raw.rfind(")") + 2:].split()
        return {"pid": pid, "birth": int(fields[19]), "image": os.readlink(f"/proc/{pid}/exe")}
    except FileNotFoundError:
        return None


def same_process(value):
    return value is not None and process_identity(value["pid"]) == value


def listener_refused(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(0.25)
        code = probe.connect_ex(("127.0.0.1", port))
    # Timeout, unreachable and other errors never prove listener absence.
    return code == errno.ECONNREFUSED


def service_lock_free(home):
    path = home / "service.lock"
    require(path.is_file() and not path.is_symlink(), "Service lock is missing or redirected")
    with path.open("rb+") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        fcntl.flock(handle, fcntl.LOCK_UN)
        return True


def group_empty(group):
    if not group:
        return True
    require(group.startswith("/") and ".." not in Path(group).parts, "Invalid fixture cgroup")
    root = Path("/sys/fs/cgroup") / group.lstrip("/")
    if not root.exists():
        return True
    require(root.is_dir() and not root.is_symlink(), "Invalid fixture cgroup directory")
    files = list(root.rglob("cgroup.procs"))
    require(len(files) <= 64, "Unexpected fixture descendant bound")
    return all(not p.is_symlink() and not p.read_text().strip() for p in files)


def typed(value):
    if value is None:
        return ["null"]
    if isinstance(value, bytes):
        return ["blob", len(value), hashlib.sha256(value).hexdigest()]
    if isinstance(value, str):
        data = value.encode()
        return ["text", len(data), hashlib.sha256(data).hexdigest()]
    return [type(value).__name__, value]


def state_snapshot(home, original_columns=None, *, normalize_startup=True):
    with sqlite3.connect((home / "state.sqlite3").as_uri() + "?mode=ro", uri=True, timeout=1) as db:
        db.execute("PRAGMA query_only=ON")
        db.execute("PRAGMA trusted_schema=OFF")
        db.execute("BEGIN")
        require(db.execute("PRAGMA integrity_check").fetchall() == [("ok",)], "Database integrity failed")
        require(not db.execute("PRAGMA foreign_key_check").fetchall(), "Foreign key continuity failed")
        if original_columns is None:
            names = [r[0] for r in db.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")]
            require(set(names) == LEGACY_TABLES, "Expected exact original 0.9 application table set")
            original_columns = {name: [r[1] for r in db.execute(f'PRAGMA table_info("{name}")')] for name in names}
        rows = {}
        for name, columns in original_columns.items():
            require(all(c.replace("_", "").isalnum() for c in [name, *columns]), "Unsafe fixture identifier")
            selected = ",".join('"' + c + '"' for c in columns)
            values = []
            for record in db.execute(f'SELECT {selected} FROM "{name}"'):
                row = dict(zip(columns, record, strict=True))
                if normalize_startup and name == "tokens" and row["id"] in {"local-control", "context-provider"}:
                    row["expires_at"] = "ordinary-startup-expiry"
                if normalize_startup and name == "agent_endpoints" and row["id"] == "default":
                    row["updated_at"] = "ordinary-startup-timestamp"
                values.append([typed(row[c]) for c in columns])
            values.sort(key=repr)
            rows[name] = {"count": len(values), "sha256": hashlib.sha256(json.dumps(values).encode()).hexdigest()}
        schema = list(db.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name"))
        gate = None
        if db.execute("SELECT 1 FROM sqlite_schema WHERE name='native_update_admission'").fetchone():
            gate = list(db.execute("SELECT transaction_id,phase,acquired_at,restored_at FROM native_update_admission ORDER BY transaction_id"))
        db.rollback()
    return {"columns": original_columns, "rows": rows, "schema": schema, "gate": gate}


def current_data_snapshot(home, *, normalize_startup=True):
    with sqlite3.connect((home / "state.sqlite3").as_uri() + "?mode=ro", uri=True, timeout=1) as db:
        db.execute("PRAGMA query_only=ON")
        names = [r[0] for r in db.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")]
        require(all(name.replace("_", "").isalnum() for name in names), "Unsafe current table name")
        columns = {name: [r[1] for r in db.execute(f'PRAGMA table_info("{name}")')] for name in names}
    return state_snapshot(home, columns, normalize_startup=normalize_startup)


def require_current_data(before, after):
    require(before["schema"] == after["schema"], "Current database schema was rewound or changed")
    ordinary = lambda snapshot: {k:v for k,v in snapshot["rows"].items() if k != "native_update_admission"}
    require(ordinary(before) == ordinary(after), "Current application rows changed across lifecycle")


def require_gate_transfer(before, after, previous_transaction, transaction):
    old = {row[0]:row for row in before}
    new = {row[0]:row for row in after}
    require(len(old) == len(before) and len(new) == len(after), "Duplicate admission gate rows")
    require(previous_transaction in old and transaction not in old
            and set(new) == set(old) | {transaction}, "Gate transfer lost or invented transaction rows")
    for key, row in old.items():
        if key == previous_transaction:
            require(row[1] == "prepared" and new[key][1] == "restored" and row[2] == new[key][2]
                    and type(new[key][3]) is int, "Previous gate identity/acquisition was not preserved")
        else:
            require(new[key] == row, "Unrelated gate changed")
    require(new[transaction][1] == "restored" and type(new[transaction][2]) is int
            and type(new[transaction][3]) is int, "Fresh adoption gate did not restore")


def require_gate_history(before, after, transaction):
    old, new = {row[0]:row for row in before}, {row[0]:row for row in after}
    require(len(old) == len(before) and len(new) == len(after) and transaction not in old
            and set(new) == set(old) | {transaction}, "Native lifecycle lost or invented gate history")
    require(all(new[key] == row for key,row in old.items()), "Native lifecycle changed earlier gate history")
    require(new[transaction][1] == "restored" and type(new[transaction][2]) is int
            and type(new[transaction][3]) is int, "Native lifecycle gate did not restore")


def package_service(package):
    manifest = read_json(package / "manifest.json", max_bytes=16 * 1024 * 1024)
    relative = Path(manifest["entrypoints"]["service"])
    require(not relative.is_absolute() and ".." not in relative.parts, "Invalid package entrypoint")
    binary = package / relative
    binary_hash = digest(binary)
    rows = [r for r in manifest["files"] if r["path"] == str(relative)]
    require(len(rows) == 1 and rows[0]["sha256"] == binary_hash, "Package executable binding differs")
    return manifest, binary, binary_hash


def gate_records(rows):
    return [{"transaction":r[0], "phase":r[1], "acquired_at":r[2], "restored_at":r[3]} for r in rows]


def require_archive(archive, saved, installation, transaction):
    require(archive["schema"] == 1 and archive["installation_id"] == installation
            and archive["id"] == transaction, "Predecessor archive envelope changed")
    value = archive["value"]
    require(value["schema"] == 1 and value["kind"] == "legacy_adoption_rollback"
            and value["id"] == installation and value["transaction_id"] == transaction,
            "Wrong predecessor archive")
    for key in ("capture", "origin", "journal", "gates", "capture_bytes", "journal_bytes",
                "root_origin_bytes", "root_origin_sha256"):
        require(value[key] == saved[key], "Predecessor archive lost exact " + key)
    return value


SEED = """
INSERT INTO conversations(id,title,created_at,updated_at,status,note,session_id,endpoint,agent_info,surface,read_only,owner,shared)
VALUES('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','Synthetic retained room',100,101,'idle','Historical unknown is not replay authority','original-session','original-endpoint','{}','web',0,'fixture-owner',1);
INSERT INTO room_members VALUES('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','fixture-owner','owner',102);
INSERT INTO room_agents VALUES('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','default','original-session','original-endpoint',2);
INSERT INTO messages(id,conversation_id,role,text,status,created_at,updated_at,actor,agent_id,targets)
VALUES('historical-input','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','user','Do not replay this historical input','complete',103,104,'fixture-owner',NULL,'["default"]'),
('historical-answer','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','assistant','Retained answer','complete',105,106,'Original actor','default','[]');
INSERT INTO grants(hash,conversation_id,expires_at,owner_scope) VALUES('synthetic-retained-grant','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',4102444800000,1);
INSERT INTO browser_sessions VALUES('synthetic-browser','fixture-owner','synthetic-csrf',4102444800000);
INSERT INTO input_authority VALUES('historical-input','synthetic-browser',NULL);
INSERT INTO input_deliveries(id,message_id,conversation_id,agent_id,participation,state,mode,target_turn,session_id,endpoint,receipt,attention,updated_at)
VALUES('historical-unknown','historical-input','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','default','direct','unknown','prompt',NULL,'original-session','original-endpoint','{"transport_submitted":null,"recipient_received":null}','{"retained":true}',107);
INSERT INTO attachments VALUES('synthetic-attachment','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','retained.bin','application/octet-stream',X'00017F80FF',5,'fixture-owner',108,'historical-input');
INSERT INTO views VALUES('synthetic-view','{"retained":true}',3,109,'fixture-owner');
INSERT INTO audit(at,kind,conversation_id,data) VALUES(110,'synthetic-retained','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa','{"unknown":true}');
"""


class Case:
    def __init__(self, root, label, old, package, *, corrected_package=None, roundtrip_package=None,
                 predecessor_service=None):
        self.root = root / label
        self.root.mkdir(mode=0o700)
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.old, self.package = old, package
        self.corrected_package, self.roundtrip_package = corrected_package, roundtrip_package
        self.predecessor_service = predecessor_service
        self.manifest, self.candidate, self.candidate_hash = package_service(package)
        self.transaction = str(uuid.uuid4())
        self.result = {"case":label,"home":str(self.home),"transaction":self.transaction,
                       "old_sha256":digest(old),"candidate_sha256":self.candidate_hash,
                       "manifest_sha256":digest(package / "manifest.json"),"commands":[],"retry_count":0}
        self.created_unit = False
        self.unit = None

    def checkpoint(self):
        write_json(self.root / "result.json", self.result)

    def command(self, label, argv, *, expected=0, timeout=180):
        entry = {"label":label,"argv":list(map(str,argv)),"timeout_seconds":timeout,"retry":False}
        self.result["commands"].append(entry)
        self.checkpoint()
        start = time.monotonic()
        with (self.root / (label + ".stdout")).open("wb") as out, (self.root / (label + ".stderr")).open("wb") as err:
            try:
                result = subprocess.run(entry["argv"], stdout=out, stderr=err, env=environment(), timeout=timeout)
                entry.update(returncode=result.returncode, status="returned")
            except subprocess.TimeoutExpired:
                entry.update(returncode=None, status="unknown_timeout")
                raise
            finally:
                entry["elapsed_seconds"] = time.monotonic() - start
                self.checkpoint()
        require(result.returncode == 0 if expected == 0 else result.returncode != 0, "Unexpected command result; retained without retry")
        return result.returncode

    def api(self, path):
        config = read_json(self.home / "config.json")
        context = ssl.create_default_context(cafile=config.get("tls_ca") or config["tls_cert"])
        token = (self.home / "secrets/control-token").read_text().strip()
        connection = http.client.HTTPSConnection("127.0.0.1", self.port, context=context, timeout=3)
        try:
            connection.request("GET", path, headers={"Authorization":"Bearer " + token})
            response = connection.getresponse()
            require(response.status == 200, "Authenticated fixed local GET failed")
            body = response.read(65537)
            require(len(body) <= 65536, "Local metadata response exceeds bound")
            return json.loads(body)
        finally:
            connection.close()

    def healthy(self, *, native=False, expected_hash=None):
        deadline = time.monotonic() + 30
        while True:
            current = properties(self.unit)
            try:
                meta = self.api("/api/v1/meta")
                identity = process_identity(int(current["MainPID"]))
                if current["ActiveState"] == "active" and current["SubState"] == "running" and meta["id"] == self.installation and identity and identity["image"] == str(self.installed):
                    if native:
                        status = self.api("/api/v1/native-update/status")
                        require(status["binary_sha256"] == (expected_hash or self.candidate_hash) and status["live_work"] is False and status["admission"]["closed"] is False, "Candidate native identity/admission differs")
                    return current, identity
            except (OSError, http.client.HTTPException, AssertionError):
                pass
            require(time.monotonic() < deadline, "Fixture listener identity did not settle")
            time.sleep(0.1)

    def stopped(self, old_identity):
        deadline = time.monotonic() + 15
        while True:
            current = properties(self.unit)
            if current["ActiveState"] == "inactive" and current["SubState"] == "dead" and current["MainPID"] == current["ControlPID"] == "0" and not same_process(old_identity) and group_empty(current["ControlGroup"]) and group_empty(self.original_properties["ControlGroup"]) and listener_refused(self.port) and service_lock_free(self.home):
                return current
            require(time.monotonic() < deadline, "Fixture exact stop/absence remained unknown")
            time.sleep(0.1)

    def setup(self):
        (self.home / "bin").mkdir(mode=0o700)
        self.installed = self.home / "bin/agent-conversations"
        shutil.copyfile(self.old, self.installed)
        self.installed.chmod(0o700)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1",0))
            self.port = reservation.getsockname()[1]
        sentinel = self.home / "must-not-run-agent"
        sentinel.write_text("#!/bin/sh\nprintf unexpected > \"$1\"\nexit 97\n")
        sentinel.chmod(0o700)
        self.command("old-init", [self.installed,"--home",self.home,"init","--owner","fixture-owner",
            "--port",str(self.port),"--public-url",f"https://127.0.0.1:{self.port}","--pam-service","login",
            "--agent-command",sentinel,"--agent-arg",self.home / "unexpected-agent-launch"], timeout=30)
        config = read_json(self.home / "config.json")
        self.installation = config["id"]
        require(uuid.UUID(self.installation).version == 4, "Expected fresh fixture installation UUID")
        self.unit = "agent-conversations-" + self.installation + ".service"
        self.unit_path = Path.home() / ".config/systemd/user" / self.unit
        require(not self.unit_path.exists(), "Refuse preexisting exact unit")
        with sqlite3.connect(self.home / "state.sqlite3") as db:
            db.executescript(SEED)
        write_json(self.home / "retained-unknown-evidence.json", {"transport_submitted":None,"retry_authorized":False})
        self.definition = custom_unit_definition(self.home, self.installed)
        self.unit_path.parent.mkdir(parents=True,exist_ok=True)
        with self.unit_path.open("xb") as out:
            out.write(self.definition)
        self.created_unit = True
        manager(["daemon-reload"])
        start_static_fixture(self.unit)
        self.original_properties, self.original_identity = self.healthy()
        require(self.original_properties["UnitFileState"] == "static", "Fixture startup policy changed")
        self.baseline = state_snapshot(self.home)
        self.protected = {str(p.relative_to(self.home)):digest(p) for p in [self.home / "config.json",self.home / "retained-unknown-evidence.json",*sorted((self.home / "secrets").rglob("*")),*sorted((self.home / "tls").rglob("*"))] if p.is_file()}
        self.database_identity = [self.home.joinpath("state.sqlite3").stat().st_dev,self.home.joinpath("state.sqlite3").stat().st_ino]
        self.result.update(installation_id=self.installation, unit=self.unit, unit_sha256=hashlib.sha256(self.definition).hexdigest(),original_identity=self.original_identity,baseline_rows=self.baseline["rows"],protected=self.protected)
        self.checkpoint()

    def preserve(self):
        require(self.unit_path.read_bytes() == self.definition, "Original custom unit changed")
        current = properties(self.unit)
        for field in ["Id","LoadState","UnitFileState","FragmentPath","DropInPaths","NeedDaemonReload","KillMode","Restart","RefuseManualStart","RefuseManualStop"]:
            require(current[field] == self.original_properties[field], "Original startup definition or policy changed")
        for name, expected in self.protected.items():
            require(digest(self.home / name) == expected, "Protected original file changed")
        after = state_snapshot(self.home,self.baseline["columns"])
        require(after["rows"] == self.baseline["rows"], "Original application data changed")
        metadata = self.home.joinpath("state.sqlite3").stat()
        require([metadata.st_dev,metadata.st_ino] == self.database_identity, "Database was replaced instead of retaining current data")
        require(not (self.home / "unexpected-agent-launch").exists(), "An agent launch was attempted")
        return after

    def add_current_canary(self, label):
        self.preserve()
        before = current_data_snapshot(self.home)
        identifier = "fixture-current-" + str(uuid.uuid4())
        canary = (identifier, json.dumps({"created_after":label,"retain_current_data":True}), 1, 113, "fixture-owner")
        with sqlite3.connect(self.home / "state.sqlite3") as db:
            previous = list(db.execute("SELECT * FROM views ORDER BY id"))
            db.execute("INSERT INTO views(id,value,revision,updated_at,owner) VALUES(?,?,?,?,?)", canary)
            require(list(db.execute("SELECT * FROM views WHERE id!=? ORDER BY id", [identifier])) == previous,
                    "Canary insertion changed previous views")
        after = current_data_snapshot(self.home)
        require(before["schema"] == after["schema"], "Canary changed the schema")
        require({k:v for k,v in before["rows"].items() if k != "views"}
                == {k:v for k,v in after["rows"].items() if k != "views"}, "Canary changed unrelated data")
        require(after["rows"]["views"]["count"] == before["rows"]["views"]["count"] + 1,
                "Canary did not add exactly one view")
        # The original baseline stays in result.json. Advance the in-memory
        # expectation only for the single explicitly inserted fixture row.
        self.baseline = state_snapshot(self.home, self.baseline["columns"])
        self.result.setdefault("fresh_current_data", []).append({"label":label,"id":identifier,
            "row_sha256":hashlib.sha256(json.dumps([typed(v) for v in canary]).encode()).hexdigest(),
            "prior_rows_preserved":True,"current_rows":after["rows"]})
        self.checkpoint()
        return after

    def ordinary_roundtrip(self):
        require(self.roundtrip_package is not None, "Separate private B package required")
        require(not (self.home / "native-prestatus-fixture-control.json").exists(),
                "Successful roundtrip must not activate the fault barrier")
        _, _, other_hash = package_service(self.roundtrip_package)
        require(other_hash != self.candidate_hash, "Roundtrip requires distinct exact A and B binaries")
        origin_path = self.home / "native-legacy-origin.json"
        origin_hash = digest(origin_path)
        require(read_json(self.home / "native-installation.json")["legacy_origin_sha256"] == origin_hash,
                "Initial native receipt lost legacy origin")
        self.result["initial_adoption_journal"] = read_json(self.home / "native-update.json")
        before = self.add_current_canary("successful_adoption_before_native_update")
        self.command("native-roundtrip-run", [self.candidate,"--home",self.home,"native-update","run",
                     "--package",self.roundtrip_package])
        response = read_json(self.root / "native-roundtrip-run.stdout")
        applied = read_json(self.home / "native-update.json")
        transaction = response["transaction_id"]
        require(uuid.UUID(transaction).version == 4 and transaction != self.transaction
                and response["restored"] is True and applied["transaction_id"] == transaction
                and applied["phase"] == "restored" and applied["direction"] == "applied"
                and applied["candidate"]["binary_sha256"] == other_hash, "Private B update did not settle")
        _, middle_identity = self.healthy(native=True, expected_hash=other_hash)
        self.preserve()
        middle = current_data_snapshot(self.home)
        require_current_data(before, middle)
        require_gate_history(before["gate"], middle["gate"], transaction)
        require(digest(origin_path) == origin_hash
                and read_json(self.home / "native-installation.json")["legacy_origin_sha256"] == origin_hash,
                "Native update lost the original legacy provenance")
        self.result["native_roundtrip"] = {"private_B_overlay_not_release":True,
            "fault_control_absent":True,"A_sha256":self.candidate_hash,"B_sha256":other_hash,
            "apply_transaction":transaction,"apply_journal":applied,"B_authenticated_identity":middle_identity}
        self.checkpoint()
        # Rollback names the exact completed apply transaction; this is one
        # new operation, never a repeat of an uncertain run.
        self.command("native-roundtrip-rollback", [self.candidate,"--home",self.home,
                     "native-update","rollback",transaction])
        response = read_json(self.root / "native-roundtrip-rollback.stdout")
        rolled = read_json(self.home / "native-update.json")
        require(response["restored"] is True and rolled["transaction_id"] == response["transaction_id"]
                and rolled["transaction_id"] not in {transaction,self.transaction}
                and rolled["phase"] == "restored" and rolled["direction"] == "rolled_back"
                and digest(self.installed) == self.candidate_hash, "Rollback did not restore exact A")
        _, identity = self.healthy(native=True)
        self.preserve()
        after = current_data_snapshot(self.home)
        require_current_data(before, after)
        require_gate_history(middle["gate"], after["gate"], rolled["transaction_id"])
        require(digest(origin_path) == origin_hash
                and read_json(self.home / "native-installation.json")["legacy_origin_sha256"] == origin_hash,
                "Native rollback lost the original legacy provenance")
        self.result["native_roundtrip"].update(rollback_transaction=rolled["transaction_id"],
            rollback_journal=rolled,A_authenticated_identity=identity,current_data_preserved=True,
            legacy_origin_sha256=origin_hash,custom_unit_unchanged=True)
        self.checkpoint()
        return identity

    def readopt_after_legacy_restore(self, previous_identity):
        require(self.corrected_package is not None, "Corrected normal package required for fresh adoption")
        _, corrected, corrected_hash = package_service(self.corrected_package)
        require(corrected_hash != self.candidate_hash, "Fresh adoption must not reuse fault binary")
        before = self.add_current_canary("legacy_restored_before_fresh_adoption")
        names = {"capture":"native-legacy-capture.json", "origin":"native-legacy-origin.json",
                 "journal":"native-update.json"}
        raw = {key:read_json_bytes(self.home/name).decode("utf-8") for key,name in names.items()}
        saved = {key:json.loads(value) for key,value in raw.items()}
        saved.update(gates=gate_records(before["gate"]),capture_bytes=raw["capture"],
                     journal_bytes=raw["journal"],root_origin_bytes=raw["origin"],
                     root_origin_sha256=digest(self.home / names["origin"]))
        old_transaction = self.transaction
        if self.predecessor_service is not None:
            refused_transaction = str(uuid.uuid4())
            hashes = {name:digest(self.home / name) for name in names.values()}
            strict_before = current_data_snapshot(self.home, normalize_startup=False)
            self.command("predecessor-capture-refusal", [self.predecessor_service,"--home",self.home,
                "native-update","capture-legacy",refused_transaction,"--legacy-sha256",digest(self.old)], expected=1,timeout=30)
            require(b"already has native update state" in (self.root / "predecessor-capture-refusal.stderr").read_bytes(),
                    "Predecessor refused for a different reason")
            require(hashes == {name:digest(self.home / name) for name in names.values()}
                    and strict_before == current_data_snapshot(self.home, normalize_startup=False)
                    and not (self.home / "native-installation.json").exists()
                    and same_process(previous_identity), "Predecessor refusal changed retained state")
            self.result["predecessor_refusal"] = {"transaction":refused_transaction,
                "binary_sha256":digest(self.predecessor_service),"records_and_current_data_unchanged":True,
                "no_owner_stop":True,"no_retry":True}
            self.preserve()
        transaction = str(uuid.uuid4())
        require(transaction != old_transaction, "Fresh explicit adoption transaction required")
        self.command("fresh-capture", [corrected,"--home",self.home,"native-update","capture-legacy",
            transaction,"--legacy-sha256",digest(self.old)],timeout=30)
        capture = read_json(self.home / "native-legacy-capture.json")
        archive_path = self.home / "native-browser-update/history/legacy-adoptions" / (old_transaction + ".json")
        archive_hash = digest(archive_path)
        predecessor = {"transaction_id":old_transaction,"sha256":archive_hash}
        require(capture["transaction_id"] == transaction and capture["predecessor"] == predecessor,
                "Fresh capture is not bound to the immutable predecessor archive")
        require_archive(read_json(archive_path), saved, self.installation, old_transaction)
        self.result["fresh_adoption"] = {"transaction":transaction,"predecessor":predecessor,
            "archive_path":str(archive_path),"archive_exact_prior_evidence":True,"capture":capture}
        self.checkpoint()
        manager(["stop",self.unit])
        self.result["fresh_adoption"]["owner_stop"] = self.stopped(previous_identity)
        self.preserve()
        self.command("fresh-adopt", [corrected,"--home",self.home,"native-update","adopt-existing",
            transaction,"--package",self.corrected_package])
        journal = read_json(self.home / "native-update.json")
        origin = read_json(self.home / "native-legacy-origin.json")
        installed = read_json(self.home / "native-installation.json")
        require(journal["transaction_id"] == transaction and journal["phase"] == "restored"
                and journal["direction"] == "applied" and journal["predecessor"] == predecessor
                and origin["transaction_id"] == transaction and origin["predecessor"] == predecessor
                and journal["legacy_origin_record"] == origin,
                "Fresh adoption did not commit its exact predecessor-bound origin")
        origin_hash = digest(self.home / "native-legacy-origin.json")
        require(journal["legacy_origin_sha256"] == installed["legacy_origin_sha256"] == origin_hash
                and digest(self.installed) == corrected_hash, "Fresh installation provenance differs")
        _, identity = self.healthy(native=True, expected_hash=corrected_hash)
        self.preserve()
        after = current_data_snapshot(self.home)
        require_current_data(before, after)
        require_gate_transfer(before["gate"], after["gate"], old_transaction, transaction)
        transfer = origin["gate_transfer"]
        require(transfer["format"] == "conversations-legacy-gate-transfer-v1"
                and transfer["transaction"] == transaction and transfer["before"] == saved["gates"],
                "Fresh gate transfer lost its typed predecessor rows")
        prepared = [dict(row) for row in gate_records(after["gate"])]
        for row in prepared:
            if row["transaction"] == transaction:
                row.update(phase="prepared",restored_at=None)
        require(transfer["after_prepared"] == prepared, "Retained prepared transfer differs from committed history")
        require(digest(archive_path) == archive_hash, "Fresh adoption rewrote predecessor history")
        require_archive(read_json(archive_path), saved, self.installation, old_transaction)
        self.result["fresh_adoption"].update(journal=journal,origin_sha256=origin_hash,
            authenticated_identity=identity,old_and_new_gate_rows_retained=True,current_data_preserved=True,
            archive_unchanged=True,corrected_sha256=corrected_hash)
        self.checkpoint()
        return identity

    def exercise(self, fault=False):
        self.setup()
        if fault:
            write_json(self.home / "native-prestatus-fixture-control.json", {"schema":1,"fixture_id":str(uuid.uuid4()),"candidate_binary_sha256":self.candidate_hash})
        self.command("capture",[self.candidate,"--home",self.home,"native-update","capture-legacy",self.transaction,"--legacy-sha256",digest(self.old)],timeout=30)
        manager(["stop",self.unit])  # Installation owner stop is outside adoption.
        self.result["owner_stop"] = self.stopped(self.original_identity)
        self.preserve()
        self.command("adopt",[self.candidate,"--home",self.home,"native-update","adopt-existing",self.transaction,"--package",self.package],expected=1 if fault else 0)
        journal = read_json(self.home / "native-update.json")
        if fault:
            observed = read_json(self.home / "native-prestatus-fixture-observation.json")
            require(observed["boundary"] == "after_appstate_before_listener" and observed["wait_expired"] is False and observed["health_proved"] is False and observed["service_lock_held"] is True and observed["transaction_id"] == self.transaction, "Fault did not reach the exact prestatus boundary")
            require(journal["launch_cleanup"]["start"]["status"] == "accepted" and journal["phase"] == "verifying" and not (self.home / "native-installation.json").exists(), "Unexpected candidate failure state")
            before_restore = self.preserve()
            require(before_restore["gate"] and before_restore["gate"][0][1] == "prepared", "Fault candidate did not retain closed admission")
            self.command("recover",[self.candidate,"--home",self.home,"native-update","recover-adoption",self.transaction])
            journal = read_json(self.home / "native-update.json")
            require(journal["phase"] == "restored" and journal["direction"] == "rolled_back" and journal["legacy_restore_ack"] == "accepted", "Legacy recovery did not settle")
            cleanup = journal["launch_cleanup"]
            require(cleanup["revoked"] is True and cleanup["stop"]["status"] == "accepted", "Candidate was not revoked and stopped exactly")
            exit_proof = cleanup["exit_before_switch"]
            require(exit_proof["application_exited"] is True and exit_proof["service_lock_acquired"] is True and exit_proof["cleanup_is_health"] is False, "No candidate exit-before-old-switch proof")
            require(not (self.home / "native-installation.json").exists() and digest(self.installed) == digest(self.old), "Legacy restoration invented a native baseline")
            _, final_identity = self.healthy()
            after = self.preserve()
            require(after["schema"] == before_restore["schema"] and after["gate"] == before_restore["gate"], "Rollback rewound the migrated current database")
            self.result["fault_proof"] = {"observed":observed,"cleanup":cleanup,"current_schema_and_gate_retained":True,"old_authenticated_health":True}
            final_identity = self.readopt_after_legacy_restore(final_identity)
        else:
            require(journal["phase"] == "restored" and journal["direction"] == "applied", "Adoption did not commit")
            require(digest(self.installed) == self.candidate_hash, "Wrong committed candidate image")
            _, final_identity = self.healthy(native=True)
            after = self.preserve()
            require(after["gate"] and after["gate"][0][1] == "restored", "Adoption did not release its gate")
            self.result["candidate_health"] = {"authenticated":True,"admission_open":True}
            final_identity = self.ordinary_roundtrip()
        self.result.update(status="passed",final_identity=final_identity,retained_state=True,custom_unit_unchanged=True,no_agent_launch=True)
        self.checkpoint()
        manager(["stop",self.unit])
        stopped = self.stopped(final_identity)
        require(self.unit_path.read_bytes() == self.definition, "Refuse changed fixture unit cleanup")
        self.unit_path.unlink()
        manager(["daemon-reload"])
        state = manager(["show",self.unit,"--property=LoadState","--value"],allowed=(0,1))
        require(state == "not-found", "Fixture unit removal not confirmed")
        self.result["cleanup"] = {"exact_final_generation_absent":True,"listener_refused":True,"service_lock_free":True,"unit_absent":True,"stopped":stopped}
        self.checkpoint()
        shutil.rmtree(self.home)
        self.result["cleanup"]["disposable_home_removed"] = not self.home.exists()
        self.checkpoint()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-service",required=True,type=Path)
    parser.add_argument("--candidate-package",required=True,type=Path)
    parser.add_argument("--fault-package",required=True,type=Path)
    parser.add_argument("--predecessor-service",type=Path,
                        help="Optional frozen old implementation for one capture-only refusal proof")
    parser.add_argument("--output",required=True,type=Path)
    args = parser.parse_args()
    require(os.name == "posix" and Path("/proc/self/stat").exists(), "Linux actual manager fixture required")
    require(not args.output.exists(), "Never overwrite a retained qualification result")
    old = args.old_service.resolve(strict=True)
    candidate = args.candidate_package.resolve(strict=True)
    fault = args.fault_package.resolve(strict=True)
    predecessor = args.predecessor_service.resolve(strict=True) if args.predecessor_service else None
    require(old.is_file() and candidate.is_dir() and fault.is_dir() and candidate != fault, "Exact separate inputs required")
    require(predecessor is None or predecessor.is_file(), "Exact predecessor service required")
    root = Path(tempfile.mkdtemp(prefix="native-legacy-adoption-"))
    result = {"schema":1,"root":str(root),"status":"running","synthetic_only":True,"no_live_home_input":True,"cases":[]}
    write_json(args.output,result)
    try:
        for label, package, broken in [("success",candidate,False),("fault",fault,True)]:
            case = Case(root,label,old,package,corrected_package=candidate,roundtrip_package=fault,
                        predecessor_service=predecessor)
            result["cases"].append(case.result)
            try:
                case.exercise(fault=broken)
            except BaseException as error:
                case.result.update(status="failed_or_unknown",error_type=type(error).__name__,retained_without_automatic_stop=True)
                case.checkpoint()
                raise
            finally:
                write_json(args.output,result)
        result["status"] = "passed"
    except BaseException as error:
        result.update(status="failed_or_unknown",error_type=type(error).__name__)
        raise
    finally:
        write_json(args.output,result)


if __name__ == "__main__":
    main()
