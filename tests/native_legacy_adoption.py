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
import shutil
import socket
import sqlite3
import ssl
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


def read_json(path):
    require(path.is_file() and not path.is_symlink(), "Expected ordinary fixture JSON")
    require(path.stat().st_size <= 1024 * 1024, "Fixture JSON exceeds bound")
    return json.loads(path.read_text(encoding="utf-8"))


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


def state_snapshot(home, original_columns=None):
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
                if name == "tokens" and row["id"] in {"local-control", "context-provider"}:
                    row["expires_at"] = "ordinary-startup-expiry"
                if name == "agent_endpoints" and row["id"] == "default":
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
    def __init__(self, root, label, old, package):
        self.root = root / label
        self.root.mkdir(mode=0o700)
        self.home = self.root / "home"
        self.home.mkdir(mode=0o700)
        self.old, self.package = old, package
        self.manifest = read_json(package / "manifest.json")
        relative = Path(self.manifest["entrypoints"]["service"])
        require(not relative.is_absolute() and ".." not in relative.parts, "Invalid package entrypoint")
        self.candidate = package / relative
        self.candidate_hash = digest(self.candidate)
        row = [r for r in self.manifest["files"] if r["path"] == str(relative)]
        require(len(row) == 1 and row[0]["sha256"] == self.candidate_hash, "Package executable binding differs")
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

    def healthy(self, *, native=False):
        deadline = time.monotonic() + 30
        while True:
            current = properties(self.unit)
            try:
                meta = self.api("/api/v1/meta")
                identity = process_identity(int(current["MainPID"]))
                if current["ActiveState"] == "active" and current["SubState"] == "running" and meta["id"] == self.installation and identity and identity["image"] == str(self.installed):
                    if native:
                        status = self.api("/api/v1/native-update/status")
                        require(status["binary_sha256"] == self.candidate_hash and status["live_work"] is False and status["admission"]["closed"] is False, "Candidate native identity/admission differs")
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
        def quote(value):
            require("\n" not in str(value), "Unit value contains newline")
            return '"' + str(value).replace("\\","\\\\").replace('"','\\"').replace("%","%%") + '"'
        self.definition = ("[Unit]\nDescription=Disposable legacy adoption custom profile\n"
            "[Service]\nType=simple\nRestart=no\nKillMode=control-group\nTimeoutStopSec=8\n"
            "Environment=CONSTELLATION_LEGACY_FIXTURE=synthetic\n"
            "UnsetEnvironment=OPENAI_API_KEY AZURE_OPENAI_API_KEY ANTHROPIC_API_KEY GOOGLE_API_KEY GEMINI_API_KEY AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN\n"
            f"WorkingDirectory={quote(self.home)}\nExecStart={quote(self.installed)} --home {quote(self.home)} serve\n").encode()
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
        else:
            require(journal["phase"] == "restored" and journal["direction"] == "applied", "Adoption did not commit")
            require(digest(self.installed) == self.candidate_hash, "Wrong committed candidate image")
            _, final_identity = self.healthy(native=True)
            after = self.preserve()
            require(after["gate"] and after["gate"][0][1] == "restored", "Adoption did not release its gate")
            self.result["candidate_health"] = {"authenticated":True,"admission_open":True}
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
    parser.add_argument("--output",required=True,type=Path)
    args = parser.parse_args()
    require(os.name == "posix" and Path("/proc/self/stat").exists(), "Linux actual manager fixture required")
    require(not args.output.exists(), "Never overwrite a retained qualification result")
    old = args.old_service.resolve(strict=True)
    candidate = args.candidate_package.resolve(strict=True)
    fault = args.fault_package.resolve(strict=True)
    require(old.is_file() and candidate.is_dir() and fault.is_dir() and candidate != fault, "Exact separate inputs required")
    root = Path(tempfile.mkdtemp(prefix="native-legacy-adoption-"))
    result = {"schema":1,"root":str(root),"status":"running","synthetic_only":True,"no_live_home_input":True,"cases":[]}
    write_json(args.output,result)
    try:
        for label, package, broken in [("success",candidate,False),("fault",fault,True)]:
            case = Case(root,label,old,package)
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
