//! Parser/state-machine tests only. Native snapshots below are synthetic; the
//! separate manager regression must prove actual Start, Stop, exit and restore.
use super::*;
use amplifier_native_platform::installation::ServiceManager;

struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    journal: Value,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = dunce::canonicalize(directory.path()).unwrap();
        config::private_dir(&home).unwrap();
        let config = Config::local(&home, "Stop-only state fixture", 54127).unwrap();
        config.save(&home).unwrap();
        let binary =
            amplifier_native_platform::installed_binary(&native_bundle::APPLICATION, &home);
        config::private_dir(binary.parent().unwrap()).unwrap();
        amplifier_native_platform::private_create(&binary, b"never executed fixture").unwrap();
        let transaction = uuid::Uuid::new_v4().to_string();
        let nonce = "fixed-stop-only-fixture";
        let prepared = crate::native_update_launch::prepare_replacement(
            &home,
            &config.id,
            &transaction,
            nonce,
            &binary,
        )
        .unwrap();
        let service = prepared.service().unwrap();
        let process = process::identity(std::process::id()).unwrap().unwrap();
        let binding = Binding {
            installation_id: config.id.clone(),
            transaction_id: transaction.clone(),
            nonce: nonce.into(),
            package: json!({"binary_sha256":"a".repeat(64),"bundle":{"manifest_sha256":"b".repeat(64)}}),
        };
        let mut native = json!({
            "schema":1,"service":service,"definition":{"bytes":[1,2,3]},
            "effective_definition":{"text":"fixed fixture"},"definition_matches":true,
            "enabled":true,"start_allowed":true,"masked":false,"loaded":true,
            "active":true,"transitioning":false,
            "processes":[{"role":"MainPID","identity":process}],
            "native":{"Id":service.name,"Names":service.name,"LoadState":"loaded","UnitFileState":"enabled",
                "MainPID":std::process::id().to_string(),"ControlPID":"0",
                "FragmentPath":"fixed fixture","SourcePath":"","DropInPaths":"",
                "NeedDaemonReload":"no","RefuseManualStart":"no","RefuseManualStop":"no",
                "ExecStart":format!("{{ path=/fixture/Companion café 日本語 ; argv[]=/fixture/Companion café 日本語 --home /fixture/home serve ; ignore_errors=no ; start_time=[Thu 2026-11-12 21:46:12 PST] ; stop_time=[n/a] ; pid={} ; code=(null) ; status=0/0 }}",std::process::id()),
                "Restart":"no","KillMode":"control-group",
                "Type":"simple","InvocationID":"a".repeat(32)}
        });
        match service.manager {
            ServiceManager::SystemdUser => {}
            ServiceManager::LaunchAgent => {
                native["processes"][0]["role"] = json!("launchd_pid");
                native["native"]["domain"] = json!("gui/501");
                native["native"]["disabled_override"] = Value::Null;
                native["native"]["properties"] = json!({"pid":std::process::id().to_string(),
                    "path":"/fixture.plist","state":"running","program":"/fixture/companion",
                    "arguments":"{\n --home fixture\n}","environment":"{\n retained=value\n}"});
            }
            ServiceManager::ScheduledTask => {
                let guid = uuid::Uuid::new_v4().to_string();
                native["processes"][0]["role"] = json!("scheduled_task_engine");
                native["processes"][0]["instance_guid"] = json!(guid);
                native["native"]["path"] = json!("\\fixture-task");
                native["native"]["xml"] = json!("<Task>fixture</Task>");
                native["native"]["enabled"] = json!(true);
                native["native"]["state"] = json!(4);
                native["native"]["owner_sid"] = json!("S-1-5-21-fixture");
                native["native"]["logon_type"] = json!(3);
                native["native"]["security_descriptor"] = json!("O:fixtureD:P(A;;FA;;;fixture)");
                native["native"]["actions"] = json!([{"type":0,"id":"","path":"fixture.exe","args":"--home fixture","cwd":"fixture"}]);
                native["native"]["instances"] = json!([{"instance_guid":guid,
                    "engine_pid":std::process::id(),"state":4,"current_action":"","engine_instance_guids":[guid]}]);
            }
        }
        let authority = StopAuthority {
            schema: 1,
            binding: binding.clone(),
            service,
            native_snapshot: native,
            application: process.clone(),
            ancestry: vec![process],
        };
        let journal = json!({
            "installation_id":config.id,"transaction_id":transaction,"phase":"verifying",
            "launch_nonce":nonce,"selected":"candidate","candidate":binding.package,"previous":binding.package,
            "launch_cleanup":{"schema":1,"binding":binding,"target":Target::Replacement{prepared},
                "start":{"status":"accepted"},"authority":authority,"revoked":false,"stop":null,
                "exit_before_switch":null}
        });
        Self {
            _directory: directory,
            home,
            journal,
        }
    }
    fn exited(&self) -> Value {
        let mut j = self.journal.clone();
        let mut native = j["launch_cleanup"]["authority"]["native_snapshot"].clone();
        native["active"] = json!(false);
        native["processes"] = json!([]);
        match native["service"]["manager"].as_str().unwrap() {
            "systemd_user" => native["native"]["MainPID"] = json!("0"),
            "launch_agent" => {
                native["native"]["properties"] = json!({});
                native["loaded"] = json!(false);
            }
            "scheduled_task" => {
                native["native"]["instances"] = json!([]);
                native["native"]["state"] = json!(3);
            }
            manager => panic!("Unexpected fixture manager {manager}"),
        }
        j["launch_cleanup"]["revoked"] = json!(true);
        j["launch_cleanup"]["stop"] = json!({"status":"accepted"});
        j["launch_cleanup"]["exit_before_switch"] = json!({
            "application_exited":true,"service_lock_acquired":true,"native_snapshot":native,
            "binary_sha256":"a".repeat(64),"cleanup_is_health":false
        });
        j["phase"] = json!("switching");
        j["launch_nonce"] = Value::Null;
        j
    }
    fn status(&self) -> Value {
        let binding = &self.journal["launch_cleanup"]["binding"];
        let application = &self.journal["launch_cleanup"]["authority"]["application"];
        let mut status = json!({"id":binding["installation_id"],
            "binary_sha256":binding["package"]["binary_sha256"],
            "transaction_id":binding["transaction_id"],"launch_nonce":binding["nonce"],
            "pid":application["pid"],"process_nonce":"c".repeat(64),"admission":{"closed":true}});
        if let Some(birth) = application["creation_time_100ns"].as_str() {
            status["process_birth"] = json!(birth.parse::<u64>().unwrap());
        }
        status
    }
    fn uncaptured(&self) -> Value {
        let mut j = self.journal.clone();
        j["launch_cleanup"]["authority"] = Value::Null;
        j["launch_cleanup"]["authority_source"] = Value::Null;
        j
    }
}

#[test]
fn no_cleanup_record_does_not_invent_a_launch_or_stop() {
    let mut journal = json!({"launch_nonce":"ordinary legacy ticket"});
    assert!(read_receipt(&journal).unwrap().is_none());
    ensure_new_start(&journal).unwrap();
    revoke(&mut journal).unwrap();
    assert!(journal["launch_nonce"].is_null());
    assert!(journal.get("launch_cleanup").is_none());
}

#[test]
fn accepted_start_is_not_exit_or_authority_to_redispatch() {
    let fixture = Fixture::new();
    assert!(read_receipt(&fixture.journal).unwrap().is_some());
    assert!(ensure_new_start(&fixture.journal).is_err());
    ensure_new_start(&fixture.exited()).unwrap();
    assert!(retains_replacement(&fixture.exited()).unwrap());
}

#[test]
fn revoke_preserves_exact_binding_and_is_observation_only_when_repeated() {
    let fixture = Fixture::new();
    let mut j = fixture.journal.clone();
    let binding = j["launch_cleanup"]["binding"].clone();
    let target = j["launch_cleanup"]["target"].clone();
    let authority = j["launch_cleanup"]["authority"].clone();
    revoke(&mut j).unwrap();
    assert_eq!(j["launch_cleanup"]["binding"], binding);
    assert_eq!(j["launch_cleanup"]["target"], target);
    assert_eq!(j["launch_cleanup"]["authority"], authority);
    assert_eq!(j["launch_cleanup"]["revoked"], true);
    assert!(j["launch_nonce"].is_null());
    let once = j.clone();
    revoke(&mut j).unwrap();
    assert_eq!(j, once);
}

#[test]
fn superseded_nonce_is_not_cleared_even_after_prior_revocation() {
    let fixture = Fixture::new();
    for already_revoked in [false, true] {
        let mut j = fixture.journal.clone();
        if already_revoked {
            revoke(&mut j).unwrap();
        }
        j["launch_nonce"] = json!("later-launch-nonce");
        let before = j.clone();
        assert!(revoke(&mut j).is_err());
        assert_eq!(j, before);
    }
}

#[test]
fn wrong_schema_installation_and_transaction_refuse() {
    let fixture = Fixture::new();
    for field in ["schema", "installation_id", "transaction_id"] {
        let mut j = fixture.journal.clone();
        if field == "schema" {
            j["launch_cleanup"]["schema"] = json!(2);
        } else {
            j[field] = json!(uuid::Uuid::new_v4().to_string());
        }
        assert!(read_receipt(&j).is_err(), "{field}");
        let before = j.clone();
        assert!(revoke(&mut j).is_err());
        assert_eq!(j, before);
    }
}

#[test]
fn changed_authority_binding_or_service_refuses() {
    let fixture = Fixture::new();
    for which in 0..3 {
        let mut j = fixture.journal.clone();
        match which {
            0 => j["launch_cleanup"]["authority"]["binding"]["nonce"] = json!("different"),
            1 => {
                j["launch_cleanup"]["authority"]["binding"]["package"]["binary_sha256"] =
                    json!("c".repeat(64))
            }
            _ => j["launch_cleanup"]["authority"]["service"]["name"] = json!("another-service"),
        }
        assert!(read_receipt(&j).is_err());
    }
}

#[test]
fn completed_exit_does_not_hide_malformed_retained_authority() {
    let fixture = Fixture::new();
    for which in 0..5 {
        let mut j = fixture.exited();
        match which {
            0 => j["launch_cleanup"]["authority"]["schema"] = json!(0),
            1 => j["launch_cleanup"]["authority"]["ancestry"] = json!([]),
            2 => j["launch_cleanup"]["authority"]["application"]["pid"] = json!(0),
            3 => j["launch_cleanup"]["authority"]["ancestry"][0]["pid"] = json!(1),
            _ => j["launch_cleanup"]["authority"]["native_snapshot"]["active"] = json!(false),
        }
        assert!(read_receipt(&j).is_err(), "case{which}");
        assert!(ensure_new_start(&j).is_err(), "case{which}");
    }
}

#[test]
fn uncertain_start_or_stop_cannot_use_a_claimed_exit_to_authorize_new_start() {
    let fixture = Fixture::new();
    for field in ["start", "stop"] {
        for ack in [
            json!({"status":"intent"}),
            json!({"status":"unknown","code":"lost_reply"}),
            json!({"status":"refused","code":"policy_changed"}),
            Value::Null,
        ] {
            let mut j = fixture.exited();
            j["launch_cleanup"][field] = ack;
            let before = j.clone();
            assert!(ensure_new_start(&j).is_err(), "{field}");
            assert_eq!(j, before);
        }
    }
}

#[test]
fn missing_malformed_or_health_labeled_exit_never_authorizes_new_start() {
    let fixture = Fixture::new();
    for which in 0..10 {
        let mut j = fixture.exited();
        match which {
            0 => j["launch_cleanup"]["exit_before_switch"] = Value::Null,
            1 => j["launch_cleanup"]["exit_before_switch"] = json!({}),
            2 => j["launch_cleanup"]["exit_before_switch"]["application_exited"] = json!(false),
            3 => j["launch_cleanup"]["exit_before_switch"]["service_lock_acquired"] = json!(false),
            4 => j["launch_cleanup"]["exit_before_switch"]["cleanup_is_health"] = json!(true),
            5 => j["launch_cleanup"]["exit_before_switch"]["binary_sha256"] = json!("c".repeat(64)),
            6 => {
                j["launch_cleanup"]["exit_before_switch"]["native_snapshot"]["active"] = json!(true)
            }
            7 => {
                j["launch_cleanup"]["exit_before_switch"]["native_snapshot"]["processes"] =
                    json!([{"pid":1}])
            }
            8 => j["launch_cleanup"]["authority"] = Value::Null,
            _ => j["launch_cleanup"]["revoked"] = json!(false),
        }
        assert!(ensure_new_start(&j).is_err(), "case{which}");
    }
}

#[test]
fn valid_nullable_policy_fields_are_not_missing_policy() {
    let fixture = Fixture::new();
    let original = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    for field in ["definition", "enabled"] {
        let mut before = original.clone();
        before[field] = Value::Null;
        same_policy(&before, &before).unwrap();
        let mut missing = before.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(same_policy(&before, &missing).is_err());
        assert!(same_policy(&missing, &missing).is_err());
        let mut changed = before.clone();
        changed[field] = json!(false);
        assert!(same_policy(&before, &changed).is_err());
    }
}

#[test]
fn effective_policy_and_visible_invocation_drift_refuse_after_stop() {
    let fixture = Fixture::new();
    let mut original = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    original["service"]["manager"] = json!("systemd_user");
    for field in [
        "DropInPaths",
        "RefuseManualStop",
        "ExecStart",
        "Restart",
        "KillMode",
        "InvocationID",
    ] {
        let mut current = original.clone();
        current["native"][field] = json!("changed");
        assert!(same_policy(&original, &current).is_err(), "{field}");
    }
    let mut stopped = original.clone();
    stopped["active"] = json!(false);
    stopped["processes"] = json!([]);
    same_policy(&original, &stopped).unwrap();
}

#[test]
fn systemd_policy_fields_must_remain_present_strings_even_when_both_sides_match() {
    let fixture = Fixture::new();
    let mut original = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    original["service"]["manager"] = json!("systemd_user");
    for field in [
        "Id",
        "Names",
        "LoadState",
        "UnitFileState",
        "FragmentPath",
        "SourcePath",
        "DropInPaths",
        "NeedDaemonReload",
        "RefuseManualStart",
        "RefuseManualStop",
        "ExecStart",
        "Restart",
        "KillMode",
        "Type",
    ] {
        let mut missing = original.clone();
        missing["native"].as_object_mut().unwrap().remove(field);
        assert!(
            same_policy(&original, &missing).is_err(),
            "missing current {field}"
        );
        assert!(
            same_policy(&missing, &original).is_err(),
            "missing accepted {field}"
        );
        assert!(
            same_policy(&missing, &missing).is_err(),
            "missing both {field}"
        );
        for invalid in [Value::Null, json!(false), json!(0), json!([]), json!({})] {
            let mut malformed = original.clone();
            malformed["native"][field] = invalid;
            assert!(
                same_policy(&original, &malformed).is_err(),
                "malformed current {field}"
            );
            assert!(
                same_policy(&malformed, &malformed).is_err(),
                "malformed both {field}"
            );
        }
    }
}

#[test]
fn systemd_invocation_absence_or_malformed_value_is_not_a_settled_empty_id() {
    let fixture = Fixture::new();
    let mut original = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    original["service"]["manager"] = json!("systemd_user");
    let mut settled = original.clone();
    settled["native"]["InvocationID"] = json!("");
    same_policy(&original, &settled).unwrap();

    let mut missing = original.clone();
    missing["native"]
        .as_object_mut()
        .unwrap()
        .remove("InvocationID");
    assert!(same_policy(&original, &missing).is_err());
    assert!(same_policy(&missing, &settled).is_err());
    assert!(same_policy(&missing, &missing).is_err());
    for invalid in [Value::Null, json!(false), json!(0), json!([]), json!({})] {
        let mut malformed = original.clone();
        malformed["native"]["InvocationID"] = invalid;
        assert!(same_policy(&original, &malformed).is_err());
        assert!(same_policy(&malformed, &settled).is_err());
        assert!(same_policy(&malformed, &malformed).is_err());
    }
}

// Captured systemctl rendering from the failed, retained actual manager run.
// These are fixture paths and times, not a live service lookup.
const ACCEPTED_EXEC_START: &str = "{ path=/tmp/Companion prestatus café 7sb2vefn/managed/Companion installation 日本語/bin/agent-conversations ; argv[]=/tmp/Companion prestatus café 7sb2vefn/managed/Companion installation 日本語/bin/agent-conversations --home /tmp/Companion prestatus café 7sb2vefn/managed/Companion installation 日本語 serve ; ignore_errors=no ; start_time=[Thu 2026-11-12 21:46:12 PST] ; stop_time=[n/a] ; pid=2553397 ; code=(null) ; status=0/0 }";
const STOPPED_EXEC_START: &str = "{ path=/tmp/Companion prestatus café 7sb2vefn/managed/Companion installation 日本語/bin/agent-conversations ; argv[]=/tmp/Companion prestatus café 7sb2vefn/managed/Companion installation 日本語/bin/agent-conversations --home /tmp/Companion prestatus café 7sb2vefn/managed/Companion installation 日本語 serve ; ignore_errors=no ; start_time=[Thu 2026-11-12 21:46:12 PST] ; stop_time=[Thu 2026-11-12 21:46:44 PST] ; pid=2553397 ; code=killed ; status=15/TERM }";

#[test]
fn actual_advertised_execstart_shape_accepts_termination_without_inventing_execstop() {
    let fixture = Fixture::new();
    let mut before = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    before["service"]["manager"] = json!("systemd_user");
    before["native"]["ExecStart"] = json!(ACCEPTED_EXEC_START);
    assert!(before["native"].get("ExecStop").is_none());
    let mut stopped = before.clone();
    stopped["active"] = json!(false);
    stopped["processes"] = json!([]);
    stopped["native"]["MainPID"] = json!("0");
    stopped["native"]["ActiveState"] = json!("inactive");
    stopped["native"]["SubState"] = json!("dead");
    stopped["native"]["ExecStart"] = json!(STOPPED_EXEC_START);
    same_policy(&before, &stopped).unwrap();
    assert_eq!(stopped["native"]["ExecStart"], STOPPED_EXEC_START);
    assert!(stopped["native"].get("ExecStop").is_none());
    // Ordinary exit has a different known result, but is still not health.
    stopped["native"]["ExecStart"] = json!(STOPPED_EXEC_START.replace(
        "code=killed ; status=15/TERM",
        "code=exited ; status=0/SUCCESS"
    ));
    same_policy(&before, &stopped).unwrap();
    stopped["effective_definition"]["text"] = json!("changed loaded command");
    assert!(same_policy(&before, &stopped).is_err());
}

#[test]
fn execstart_command_and_accepted_execution_identity_cannot_change_during_stop() {
    let fixture = Fixture::new();
    let mut before = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    before["service"]["manager"] = json!("systemd_user");
    before["native"]["ExecStart"] = json!(ACCEPTED_EXEC_START);
    same_policy(&before, &before).unwrap();
    for (old, new) in [
        ("path=/tmp/", "path=/different/"),
        (" serve ; ignore_errors", " serve --other ; ignore_errors"),
        ("ignore_errors=no", "ignore_errors=yes"),
        (
            "start_time=[Thu 2026-11-12 21:46:12 PST]",
            "start_time=[Thu 2026-11-12 21:46:13 PST]",
        ),
        ("pid=2553397", "pid=2553398"),
    ] {
        let mut changed = before.clone();
        changed["native"]["ExecStart"] = json!(STOPPED_EXEC_START.replace(old, new));
        assert!(same_policy(&before, &changed).is_err(), "{old}");
    }
}

#[test]
fn unknown_or_malformed_execstart_renderer_records_are_not_normalized() {
    for text in [
        String::new(),
        "fixed fixture".into(),
        ACCEPTED_EXEC_START.replace("/tmp/", "tmp/"),
        format!("{ACCEPTED_EXEC_START} {ACCEPTED_EXEC_START}"),
        ACCEPTED_EXEC_START.replace(" ; code=(null)", " ; unknown_runtime=0 ; code=(null)"),
        ACCEPTED_EXEC_START.replace(" ; code=(null)", " ; code=(null) ; code=killed"),
        ACCEPTED_EXEC_START.replace("stop_time=[n/a]", "stop_time=n/a"),
        ACCEPTED_EXEC_START.replace("stop_time=[n/a]", "stop_time=[[n/a]]"),
        ACCEPTED_EXEC_START.replace("pid=2553397", "pid=0"),
        ACCEPTED_EXEC_START.replace("pid=2553397", "pid=02553397"),
        ACCEPTED_EXEC_START.replace("code=(null)", "code=unknown"),
        ACCEPTED_EXEC_START.replace("status=0/0", "status=0"),
        ACCEPTED_EXEC_START.replace("status=0/0", "status=0/UNKNOWN/EXTRA"),
        ACCEPTED_EXEC_START.replace("status=0/0", "status=0/"),
        ACCEPTED_EXEC_START.replace("status=0/0", "status=-1/UNKNOWN"),
        ACCEPTED_EXEC_START.replace("ignore_errors=no", "ignore_errors=unknown"),
        ACCEPTED_EXEC_START.replace(" ; pid=", "\n ; pid="),
    ] {
        assert!(systemd_exec_identity(&json!(text)).is_err(), "{text}");
    }
}

#[test]
fn optional_execstop_and_unknown_native_properties_remain_preserved() {
    let fixture = Fixture::new();
    let mut before = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    before["service"]["manager"] = json!("systemd_user");
    same_policy(&before, &before).unwrap();
    let mut changed = before.clone();
    changed["native"]["ExecStop"] = json!("");
    assert!(same_policy(&before, &changed).is_err());
    same_policy(&changed, &changed).unwrap();
    changed["native"]["ExecStop"] = Value::Null;
    assert!(same_policy(&changed, &changed).is_err());
    before["native"]["FutureStaticPolicy"] = json!("retained unknown value");
    same_policy(&before, &before).unwrap();
    changed = before.clone();
    changed["native"]["FutureStaticPolicy"] = json!("different");
    assert!(same_policy(&before, &changed).is_err());
    changed["native"]
        .as_object_mut()
        .unwrap()
        .remove("FutureStaticPolicy");
    assert!(same_policy(&before, &changed).is_err());
}

fn synthetic_task_snapshot() -> Value {
    let fixture = Fixture::new();
    let mut snapshot = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    snapshot["service"]["manager"] = json!("scheduled_task");
    let guid = "11111111-2222-4333-8444-555555555555";
    snapshot["native"] = json!({"path":"\\fixture-task","xml":"<Task>retained</Task>",
        "enabled":true,"state":4,"owner_sid":"S-1-5-21-fixture","logon_type":3,
        "security_descriptor":"O:fixtureD:P(A;;FA;;;fixture)",
        "actions":[{"type":0,"id":"","path":"fixture.exe","args":"--home fixture","cwd":"fixture"}],
        "instances":[{"instance_guid":guid,"engine_pid":123,"state":4,
            "current_action":"","engine_instance_guids":[guid]}]});
    snapshot
}

#[test]
fn task_dacl_static_policy_and_unknown_native_keys_are_not_lost_after_stop() {
    let before = synthetic_task_snapshot();
    let mut stopped = before.clone();
    stopped["native"]["state"] = json!(3);
    stopped["native"]["instances"] = json!([]);
    same_policy(&before, &stopped).unwrap();
    for field in [
        "path",
        "xml",
        "owner_sid",
        "security_descriptor",
        "enabled",
        "logon_type",
        "actions",
    ] {
        let mut changed = stopped.clone();
        changed["native"][field] = Value::Null;
        assert!(same_policy(&before, &changed).is_err(), "{field}");
    }
    let mut changed = stopped.clone();
    changed["native"]["security_descriptor"] = json!("O:anotherD:P(A;;FA;;;another)");
    assert!(same_policy(&before, &changed).is_err());
    let mut extended = before.clone();
    extended["native"]["unknown_native_policy"] = json!({"retained":true});
    same_policy(&extended, &extended).unwrap();
    for value in [json!({"retained":false}), Value::Null] {
        let mut changed = extended.clone();
        changed["native"]["unknown_native_policy"] = value;
        assert!(same_policy(&extended, &changed).is_err());
    }
    assert!(same_policy(&extended, &before).is_err());
    assert!(same_policy(&before, &extended).is_err());
    for state in [
        Value::Null,
        json!("3"),
        json!(false),
        json!(-1),
        json!(0),
        json!(5),
    ] {
        let mut changed = stopped.clone();
        changed["native"]["state"] = state;
        assert!(same_policy(&before, &changed).is_err());
        assert!(native_instances_settled(&changed).is_err());
    }
}

#[test]
fn task_instance_with_zero_engine_pid_still_blocks_exit_and_new_guid_is_refused() {
    let before = synthetic_task_snapshot();
    let mut stopping = before.clone();
    stopping["active"] = json!(false);
    stopping["processes"] = json!([]);
    stopping["native"]["state"] = json!(3);
    stopping["native"]["instances"][0]["state"] = json!(3);
    stopping["native"]["instances"][0]["engine_pid"] = json!(0);
    stopping["native"]["instances"][0]["engine_instance_guids"] = json!([]);
    same_policy(&before, &stopping).unwrap();
    assert!(!native_instances_settled(&stopping).unwrap());
    for which in 0..4 {
        let mut changed = stopping.clone();
        match which {
            0 => {
                changed["native"]["instances"][0]["instance_guid"] =
                    json!(uuid::Uuid::new_v4().to_string())
            }
            1 => changed["native"]["instances"][0]["engine_pid"] = json!(124),
            2 => {
                changed["native"]["instances"][0]["engine_instance_guids"] =
                    json!([uuid::Uuid::new_v4().to_string()])
            }
            _ => {
                changed["native"]["instances"] = json!([
                    stopping["native"]["instances"][0],
                    stopping["native"]["instances"][0]
                ])
            }
        }
        assert!(same_policy(&before, &changed).is_err(), "case{which}");
    }
    stopping["native"]["instances"] = json!([]);
    same_policy(&before, &stopping).unwrap();
    assert!(native_instances_settled(&stopping).unwrap());
    for state in [2, 4] {
        stopping["native"]["state"] = json!(state);
        assert!(!native_instances_settled(&stopping).unwrap());
    }
}

fn synthetic_launch_agent_snapshot() -> Value {
    let fixture = Fixture::new();
    let mut snapshot = fixture.journal["launch_cleanup"]["authority"]["native_snapshot"].clone();
    snapshot["service"]["manager"] = json!("launch_agent");
    snapshot["native"] = json!({"domain":"gui/501","disabled_override":null,
        "properties":{"state":"running","pid":"123","path":"/fixture.plist",
            "program":"/fixture/companion","arguments":"{\n --home fixture\n}",
            "environment":"{\n retained=value\n}"}});
    snapshot
}

#[test]
fn launch_agent_loaded_effective_policy_and_unloaded_outer_policy_remain_exact() {
    let before = synthetic_launch_agent_snapshot();
    let mut loaded_stopped = before.clone();
    loaded_stopped["native"]["properties"]["state"] = json!("waiting");
    loaded_stopped["native"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("pid");
    same_policy(&before, &loaded_stopped).unwrap();
    for field in ["program", "arguments", "environment"] {
        let mut changed = loaded_stopped.clone();
        changed["native"]["properties"][field] = json!("changed loaded policy");
        assert!(same_policy(&before, &changed).is_err(), "{field}");
    }
    let mut unknown = before.clone();
    unknown["native"]["properties"]["future_policy"] = json!("retained");
    same_policy(&unknown, &unknown).unwrap();
    assert!(same_policy(&unknown, &before).is_err());
    assert!(same_policy(&before, &unknown).is_err());
    let mut unloaded = before.clone();
    unloaded["loaded"] = json!(false);
    unloaded["native"]["properties"] = json!({});
    same_policy(&before, &unloaded).unwrap();
    for field in ["domain", "disabled_override"] {
        let mut changed = unloaded.clone();
        changed["native"][field] = if field == "domain" {
            json!("gui/502")
        } else {
            json!(false)
        };
        assert!(same_policy(&before, &changed).is_err());
        changed["native"].as_object_mut().unwrap().remove(field);
        assert!(same_policy(&before, &changed).is_err());
    }
    unknown = before.clone();
    unknown["native"]["future_policy"] = json!("retained");
    assert!(same_policy(&unknown, &unloaded).is_err());
    assert!(same_policy(&unloaded, &unknown).is_err());
    unloaded["native"]["properties"] = json!({"program":"unconfirmed"});
    assert!(same_policy(&before, &unloaded).is_err());
}

#[test]
fn authenticated_status_proofs_bind_existing_fields_and_do_not_invent_legacy_provenance() {
    let fixture = Fixture::new();
    let r = read_receipt(&fixture.journal).unwrap().unwrap();
    assert!(r.authority_source.is_none());
    let proof = status_proof(&fixture.status(), &r.binding, false).unwrap();
    let mut captured = fixture.journal.clone();
    captured["launch_cleanup"]["authority_source"] =
        serde_json::to_value(AuthoritySource::AuthenticatedStatus {
            first_status: Box::new(proof.clone()),
            confirmed_status: Box::new(proof),
            restore_started: false,
        })
        .unwrap();
    assert!(
        read_receipt(&captured)
            .unwrap()
            .unwrap()
            .authority_source
            .is_some()
    );
    captured["launch_cleanup"]["authority_source"]["confirmed_status"]["process_nonce"] =
        json!("d".repeat(64));
    assert!(read_receipt(&captured).is_err());
    captured["launch_cleanup"]["authority_source"] = json!({"kind":"process_entry"});
    read_receipt(&captured).unwrap();
    captured["launch_cleanup"]["authority"] = Value::Null;
    assert!(read_receipt(&captured).is_err());
}

#[test]
fn authenticated_status_rejects_incomplete_binding_and_changed_native_generation() {
    let fixture = Fixture::new();
    let r = read_receipt(&fixture.journal).unwrap().unwrap();
    let status = fixture.status();
    for field in [
        "id",
        "binary_sha256",
        "transaction_id",
        "launch_nonce",
        "pid",
        "process_nonce",
        "admission",
    ] {
        let mut missing = status.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            status_proof(&missing, &r.binding, false).is_err(),
            "{field}"
        );
    }
    for pid in [
        json!(0),
        json!(u64::from(u32::MAX) + 1),
        json!("1"),
        Value::Null,
    ] {
        let mut changed = status.clone();
        changed["pid"] = pid;
        assert!(status_proof(&changed, &r.binding, false).is_err());
    }
    let mut open = status.clone();
    open["admission"]["closed"] = json!(false);
    assert!(status_proof(&open, &r.binding, false).is_err());
    status_proof(&open, &r.binding, true).unwrap();
    open["admission"]["closed"] = Value::Null;
    assert!(status_proof(&open, &r.binding, true).is_err());

    let application = &r.authority.as_ref().unwrap().application;
    validate_status_generation(application, Some(application)).unwrap();
    validate_status_pair(
        &status_proof(&status, &r.binding, false).unwrap(),
        &status_proof(&status, &r.binding, false).unwrap(),
        application,
    )
    .unwrap();
    let mut changed = application.clone();
    let field = if cfg!(windows) {
        "creation_time_100ns"
    } else if cfg!(target_os = "macos") {
        "start_microseconds"
    } else {
        "start_ticks"
    };
    changed[field] = json!(format!("{}1", application[field].as_str().unwrap()));
    assert!(validate_status_generation(application, Some(&changed)).is_err());
    assert!(validate_status_generation(application, None).is_err());
}

#[tokio::test]
async fn uncertain_start_or_superseded_binding_never_reaches_status_confirmation() {
    let fixture = Fixture::new();
    for which in 0..6 {
        let mut j = fixture.uncaptured();
        match which {
            0 => j["launch_cleanup"]["start"] = json!({"status":"unknown","code":"lost_reply"}),
            1 => j["launch_cleanup"]["start"] = json!({"status":"intent"}),
            2 => j["launch_cleanup"]["start"] = json!({"status":"refused","code":"changed"}),
            3 => j["launch_cleanup"]["revoked"] = json!(true),
            4 => j["launch_nonce"] = json!("superseded"),
            _ => j["candidate"]["binary_sha256"] = json!("d".repeat(64)),
        }
        let before = j.clone();
        let queried = std::cell::Cell::new(false);
        let result =
            capture_authenticated_status_with(&fixture.home, &mut j, &fixture.status(), || {
                queried.set(true);
                std::future::ready(Err(anyhow::anyhow!("must not query")))
            })
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("uncertain or superseded"),
            "case{which}"
        );
        assert!(!queried.get(), "case{which}");
        assert_eq!(j, before);
    }
}

#[tokio::test]
async fn malformed_or_stale_present_entry_never_falls_through_to_status_capture() {
    for which in 0..3 {
        let fixture = Fixture::new();
        let mut j = fixture.uncaptured();
        let before = j.clone();
        let r = read_receipt(&j).unwrap().unwrap();
        let path = identity::entry_path(&fixture.home, &r.binding).unwrap();
        let mut entry = json!({"schema":1,"binding":r.binding,
            "application":fixture.journal["launch_cleanup"]["authority"]["application"]});
        let bytes = match which {
            0 => b"{".to_vec(),
            1 => {
                entry["binding"]["nonce"] = json!("stale");
                serde_json::to_vec(&entry).unwrap()
            }
            _ => {
                entry["application"]["pid"] = json!(u64::from(std::process::id()) + 1);
                serde_json::to_vec(&entry).unwrap()
            }
        };
        amplifier_native_platform::private_create(&path, &bytes).unwrap();
        let queried = std::cell::Cell::new(false);
        let result =
            capture_authenticated_status_with(&fixture.home, &mut j, &fixture.status(), || {
                queried.set(true);
                std::future::ready(Err(anyhow::anyhow!("must not query")))
            })
            .await;
        assert!(result.is_err(), "case{which}");
        assert!(!queried.get(), "case{which}");
        assert_eq!(j, before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn changed_confirmation_refuses_before_native_manager_observation_or_publication() {
    let fixture = Fixture::new();
    let first = fixture.status();
    for field in [
        "id",
        "binary_sha256",
        "transaction_id",
        "launch_nonce",
        "pid",
        "process_nonce",
        "admission",
    ] {
        let mut j = fixture.uncaptured();
        let before = j.clone();
        let mut second = first.clone();
        second[field] = match field {
            "pid" => json!(first["pid"].as_u64().unwrap() + 1),
            "admission" => json!({"closed":false}),
            _ => json!("d".repeat(64)),
        };
        let queries = std::cell::Cell::new(0);
        let result = capture_authenticated_status_with(&fixture.home, &mut j, &first, || {
            queries.set(queries.get() + 1);
            std::future::ready(Ok(second))
        })
        .await;
        assert!(result.is_err(), "{field}");
        assert_eq!(queries.get(), 1, "{field}");
        assert_eq!(j, before);
        let r = read_receipt(&j).unwrap().unwrap();
        assert!(optional_entry(&fixture.home, &r.binding).unwrap().is_none());
    }
}

#[test]
fn manager_error_is_unknown_and_does_not_become_refusal_or_acceptance() {
    assert_eq!(
        Ack::from_result(Err(anyhow::anyhow!("lost native reply"))),
        Ack::Unknown {
            code: "manager_call_unconfirmed".into()
        }
    );
    assert_eq!(
        Ack::from_result(Ok(ActionOutcome::Unknown {
            code: "native_unknown".into(),
            message: "retained".into()
        })),
        Ack::Unknown {
            code: "native_unknown".into()
        }
    );
    assert_eq!(
        Ack::from_result(Ok(ActionOutcome::Refused {
            code: "changed".into(),
            message: "retained".into()
        })),
        Ack::Refused {
            code: "changed".into()
        }
    );
}

#[test]
fn no_receipt_publish_is_a_noop_and_unrelated_identity_is_not_created() {
    let fixture = Fixture::new();
    publish_entry(&fixture.home, &json!({})).unwrap();
    assert!(!fs::read_dir(&fixture.home).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("native-update-entry-")
    }));
    let mut j = fixture.journal.clone();
    j["launch_nonce"] = json!("wrong");
    assert!(publish_entry(&fixture.home, &j).is_err());
}
