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
            "native":{"Id":service.name,"Names":service.name,"UnitFileState":"enabled",
                "MainPID":std::process::id().to_string(),"ControlPID":"0",
                "FragmentPath":"fixed fixture","SourcePath":"","DropInPaths":"",
                "NeedDaemonReload":"no","RefuseManualStart":"no","RefuseManualStop":"no",
                "ExecStart":"fixed fixture","ExecStop":"","Restart":"no","KillMode":"control-group",
                "Type":"simple","InvocationID":"a".repeat(32)}
        });
        match service.manager {
            ServiceManager::SystemdUser => {}
            ServiceManager::LaunchAgent => {
                native["processes"][0]["role"] = json!("launchd_pid");
                native["native"]["properties"] = json!({"pid":std::process::id().to_string()});
            }
            ServiceManager::ScheduledTask => {
                let guid = uuid::Uuid::new_v4().to_string();
                native["processes"][0]["role"] = json!("scheduled_task_engine");
                native["processes"][0]["instance_guid"] = json!(guid);
                native["native"]["instances"] = json!([{"instance_guid":guid,
                    "engine_pid":std::process::id(),"state":4,"engine_instance_guids":[guid]}]);
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
        native["native"]["MainPID"] = json!("0");
        native["native"]["instances"] = json!([]);
        native["native"]["properties"] = json!({});
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
        "UnitFileState",
        "FragmentPath",
        "SourcePath",
        "DropInPaths",
        "NeedDaemonReload",
        "RefuseManualStart",
        "RefuseManualStop",
        "ExecStart",
        "ExecStop",
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
