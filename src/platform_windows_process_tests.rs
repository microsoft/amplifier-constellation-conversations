//! Windows-native process witnesses use only this test executable and loopback.
//! The ignored fixture cannot spawn unless passed the private test marker.
use super::*;
use std::{net::Ipv4Addr, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

const FIXTURE: &str = "platform::windows_process_tests::process_fixture";
const MARKER: &str = "companion-job-fixture:";

fn args(mode: &str, port: u16) -> Vec<String> {
    vec![
        "--ignored".into(),
        "--exact".into(),
        FIXTURE.into(),
        "--nocapture".into(),
        "--skip".into(),
        format!("{MARKER}{mode}:{port}"),
    ]
}
fn command(mode: &str, port: u16) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(args(mode, port))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

#[test]
#[ignore = "subprocess for bounded Windows job tests"]
fn process_fixture() {
    use std::io::Write;
    let Some(marker) = std::env::args().find_map(|arg| arg.strip_prefix(MARKER).map(str::to_owned))
    else {
        return;
    };
    let (mode, port) = marker.split_once(':').unwrap();
    let port = port.parse::<u16>().unwrap();
    if let Some(path) = std::env::var_os("COMPANION_JOB_TEST_MARKER") {
        std::fs::write(path, b"child code ran").unwrap();
    }
    if mode == "owner" {
        // This process owns the job handle. Killing it must close the handle in
        // the kernel and stop descendants, without relying on Rust destructors.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut command = command("parent", port);
            ProcessTree::prepare(&mut command);
            let mut child = command.spawn().unwrap();
            let _tree = ProcessTree::attach(&mut child).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        return;
    }
    // Start a descendant immediately, before the parent's first witness. The
    // descendant gets the parent's job automatically; it has no test-side guard.
    let descendant = if mode == "parent" || mode == "exiting-parent" {
        Some(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(args("descendant", port))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    } else {
        assert_eq!(mode, "descendant");
        None
    };
    let mut witness = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    witness
        .write_all(if descendant.is_some() { b"P" } else { b"D" })
        .unwrap();
    if mode != "exiting-parent" {
        // Independent safety stop bounds cleanup even if a regression breaks
        // containment. The tests require EOF much sooner than this deadline.
        std::thread::sleep(Duration::from_secs(30));
    }
    drop(descendant);
}

async fn listener() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap()
}
async fn witnesses(listener: &tokio::net::TcpListener) -> Vec<tokio::net::TcpStream> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut witnesses = Vec::new();
        let mut roles = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            roles.push(socket.read_u8().await.unwrap());
            witnesses.push(socket);
        }
        roles.sort_unstable();
        assert_eq!(roles, b"DP");
        witnesses
    })
    .await
    .expect("Parent and immediate descendant must become observable")
}
async fn assert_closed(witnesses: Vec<tokio::net::TcpStream>) {
    for mut socket in witnesses {
        let result = tokio::time::timeout(Duration::from_secs(5), socket.read_u8())
            .await
            .expect("Job cleanup must terminate all descendant witnesses");
        assert!(result.is_err_and(|e| matches!(
            e.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
        )));
    }
}

#[tokio::test]
async fn suspended_child_cannot_run_before_job_assignment_and_descendants_are_killed() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("executed");
    let listener = listener().await;
    let mut command = command("parent", listener.local_addr().unwrap().port());
    command.env("COMPANION_JOB_TEST_MARKER", &marker);
    ProcessTree::prepare(&mut command);
    let mut child = command.spawn().unwrap();
    // An immediately runnable process would execute before the deliberately
    // delayed assignment. No marker or socket may exist while it is suspended.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!marker.exists());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    let mut tree = ProcessTree::attach(&mut child).await.unwrap();
    let sockets = witnesses(&listener).await;
    assert!(marker.exists());
    tree.terminate(&mut child);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .code()
            .is_some()
    );
    assert_closed(sockets).await;
    tree.terminate(&mut child);
}

#[tokio::test]
async fn assignment_and_resume_failures_reap_suspended_child_without_execution() {
    for fail_assignment in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("must-not-execute");
        let listener = listener().await;
        let mut command = command("parent", listener.local_addr().unwrap().port());
        command.env("COMPANION_JOB_TEST_MARKER", &marker);
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let result = if fail_assignment {
            windows::ProcessJob::attach_with(
                &child,
                |_, _| anyhow::bail!("fixture assignment failure"),
                |_| panic!("Must not resume after failed assignment"),
            )
        } else {
            windows::ProcessJob::attach_with(&child, windows::assign_job, |_| {
                anyhow::bail!("fixture resume failure")
            })
        };
        assert!(
            ProcessTree::finish_attach(&mut child, result)
                .await
                .is_err()
        );
        assert!(child.id().is_none(), "Failed launch must already be reaped");
        assert!(child.try_wait().unwrap().is_some());
        assert!(!marker.exists(), "Child code must never execute on failure");
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn cancellation_drops_job_and_child_with_immediate_descendants() {
    let listener = listener().await;
    let port = listener.local_addr().unwrap().port();
    let owner = tokio::spawn(async move {
        let mut command = command("parent", port);
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let _tree = ProcessTree::attach(&mut child).await.unwrap();
        std::future::pending::<()>().await;
    });
    let sockets = witnesses(&listener).await;
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    assert_closed(sockets).await;
}

#[tokio::test]
async fn completed_parent_reaped_closes_job_for_remaining_descendant() {
    let listener = listener().await;
    let mut command = command("exiting-parent", listener.local_addr().unwrap().port());
    ProcessTree::prepare(&mut command);
    let mut child = command.spawn().unwrap();
    let mut tree = ProcessTree::attach(&mut child).await.unwrap();
    let sockets = witnesses(&listener).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    tree.reaped();
    assert_closed(sockets).await;
}

#[tokio::test]
async fn killing_job_owner_also_terminates_its_descendants() {
    let listener = listener().await;
    // Do not give this owner a parent job: only its own job must account for
    // stopping the parent/descendant pair after abrupt owner termination.
    let mut child = command("owner", listener.local_addr().unwrap().port())
        .spawn()
        .unwrap();
    let sockets = witnesses(&listener).await;
    child.start_kill().unwrap();
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_closed(sockets).await;
}
