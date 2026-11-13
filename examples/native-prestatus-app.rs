//! Test-only installed application for the manager/pre-status recovery fixture.
//!
//! Never packaged or enabled by a production environment flag. It accepts only
//! the application's fixed manager argv. The separately bound fixture control
//! selects this exact executable; other launches use the production server.
use agent_conversations::{config, native_update, server, state::AppState};
use amplifier_native_platform::process;
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, time::Duration};

const CONTROL: &str = "native-prestatus-fixture-control.json";
const OBSERVATION: &str = "native-prestatus-fixture-observation.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    schema: u32,
    fixture_id: String,
    candidate_binary_sha256: String,
}

fn read_control(home: &std::path::Path) -> Result<Option<Control>> {
    let path = home.join(CONTROL);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 4096,
        "Fixture control must be a bounded ordinary file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Fixture control cannot be a reparse point"
        );
    }
    let bytes = fs::read(path)?;
    ensure!(bytes.len() <= 4096, "Fixture control grew while reading");
    let control: Control = serde_json::from_slice(&bytes)?;
    ensure!(control.schema == 1, "Unknown fixture control schema");
    uuid::Uuid::parse_str(&control.fixture_id).context("Invalid fixture identity")?;
    ensure!(
        control.candidate_binary_sha256.len() == 64
            && control
                .candidate_binary_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Invalid fixture executable digest"
    );
    Ok(Some(control))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() >= 3 && args[0] == "--home",
        "Only fixed application manager argv is accepted"
    );
    let home = PathBuf::from(&args[1]);
    ensure!(home.is_absolute(), "Fixture home must be absolute");
    let explicit = match args[2].to_str() {
        Some("serve") if args.len() == 3 => None,
        Some("serve-update") if args.len() == 5 => Some((
            args[3]
                .to_str()
                .context("Invalid transaction argv")?
                .to_owned(),
            args[4].to_str().context("Invalid nonce argv")?.to_owned(),
        )),
        _ => bail!("Only serve and fixed serve-update are accepted"),
    };
    let control = read_control(&home)?;
    let actual = format!("{:x}", Sha256::digest(fs::read(std::env::current_exe()?)?));
    let Some(control) = control.filter(|value| value.candidate_binary_sha256 == actual) else {
        return match explicit {
            Some((transaction, nonce)) => server::serve_update(home, transaction, nonce).await,
            None => server::serve(home).await,
        };
    };

    // Same ordering as server::serve_inner. validate_start includes the actual
    // production STOP-only entry publisher once that boundary is implemented.
    // The fixture does not fabricate a receipt or manufacture health.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("service.lock"))?;
    lock.try_lock_exclusive()
        .context("Fixture service already owned")?;
    let ticket = match explicit {
        Some(value) => Some(value),
        None => native_update::managed_startup_ticket(&home)?,
    }
    .context("Fault fixture requires a real transaction startup ticket")?;
    let launch = native_update::validate_start(&home, Some((&ticket.0, &ticket.1)))?;
    let _state = AppState::open(home.clone())?;
    // A pause while retaining native-launch.lock would prevent revocation and
    // test a different failure. Production releases it after AppState::open,
    // before binding the listener; pause at that exact boundary instead.
    drop(launch);
    let identity = process::identity(std::process::id())?
        .context("Missing actual fixture process identity")?;
    let observed = json!({"schema":1,"fixture_id":control.fixture_id,
        "transaction_id":ticket.0,"launch_nonce":ticket.1,
        "binary_sha256":actual,"process_identity":identity,
        "boundary":"after_appstate_before_listener","service_lock_held":true,
        "listener_started":false,"health_proved":false,"wait_expired":false});
    amplifier_native_platform::private_create(
        &home.join(OBSERVATION),
        &serde_json::to_vec_pretty(&observed)?,
    )?;

    // Test-only leak bound, not a production verification-clock change. A
    // terminal reached by this timeout is explicitly not successful manager
    // cleanup. No release file, retry, listener, or effect is dispatched.
    tokio::time::sleep(Duration::from_secs(120)).await;
    let mut expired = observed;
    expired["wait_expired"] = json!(true);
    config::private_write(
        &home.join(OBSERVATION),
        &serde_json::to_vec_pretty(&expired)?,
    )?;
    drop(lock);
    Ok(())
}
