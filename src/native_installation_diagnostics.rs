//! Content-free startup observations. Diagnostics neither authorize nor retry work.
use anyhow::Result;
use serde::Serialize;
use std::{
    io::{self, Write},
    time::Instant,
};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    DownloadVerify,
    IdentityCreate,
    BootstrapVerify,
    BundleVerify,
    BundleCopy,
    BundleVerifyCopied,
    BundlePublish,
    BinaryPublish,
    ReceiptPublish,
    InstalledVerify,
}

pub(crate) fn observe<T>(phase: Phase, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    observe_with(phase, operation, |line| {
        // Each record owns the lock only while it is written. The operation may
        // contain nested observed phases and never runs under this stderr lock.
        let mut stderr = io::stderr().lock();
        stderr.write_all(line)?;
        stderr.flush()
    })
}

fn observe_with<T>(
    phase: Phase,
    operation: impl FnOnce() -> Result<T>,
    mut emit: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<T> {
    fn record(emit: &mut impl FnMut(&[u8]) -> io::Result<()>, value: serde_json::Value) {
        if let Ok(mut line) = serde_json::to_vec(&value) {
            line.push(b'\n');
            let _ = emit(&line);
        }
    }
    record(
        &mut emit,
        serde_json::json!({
            "event": "native_installation_phase", "phase": phase, "state": "started"
        }),
    );
    let started = Instant::now();
    let result = operation();
    record(
        &mut emit,
        serde_json::json!({
            "event": "native_installation_phase", "phase": phase, "state": "returned",
            "elapsed_ms": started.elapsed().as_millis(),
            "result": if result.is_ok() { "ok" } else { "error" }
        }),
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, sync::mpsc, time::Duration};

    #[test]
    fn started_is_observable_while_operation_is_still_blocked() {
        let (records, received) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            observe_with(
                Phase::BundleCopy,
                || {
                    blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(42)
                },
                |line| {
                    records.send(line.to_vec()).unwrap();
                    Ok(())
                },
            )
        });
        let first = received.recv_timeout(Duration::from_secs(5)).unwrap();
        let first: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(first["phase"], "bundle_copy");
        assert_eq!(first["state"], "started");
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        let last = received.recv_timeout(Duration::from_secs(5)).unwrap();
        let last: serde_json::Value = serde_json::from_slice(&last).unwrap();
        assert_eq!(last["state"], "returned");
        assert_eq!(last["result"], "ok");
        assert!(last["elapsed_ms"].is_u64());
        assert_eq!(worker.join().unwrap().unwrap(), 42);
    }

    #[derive(Debug)]
    struct PrivateFailure(&'static str);
    impl std::fmt::Display for PrivateFailure {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for PrivateFailure {}

    #[test]
    fn error_identity_is_preserved_and_sensitive_text_never_enters_records() {
        let error = anyhow::Error::new(PrivateFailure("credential-and-private-path-sentinel"));
        let identity = error.downcast_ref::<PrivateFailure>().unwrap() as *const _;
        let mut output = Vec::new();
        let result: Result<()> = observe_with(
            Phase::ReceiptPublish,
            || Err(error),
            |line| output.write_all(line),
        );
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<PrivateFailure>().unwrap() as *const _,
            identity
        );
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("credential-and-private-path-sentinel"));
        let records: Vec<serde_json::Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["result"], "error");
        assert!(records.iter().all(|record| record.get("error").is_none()));
    }

    #[test]
    fn diagnostic_write_failure_does_not_change_operation_result() {
        let mut writes = 0;
        let value = observe_with(
            Phase::BinaryPublish,
            || Ok(91),
            |_| {
                writes += 1;
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed observer"))
            },
        )
        .unwrap();
        assert_eq!(value, 91);
        assert_eq!(writes, 2);
    }

    #[test]
    fn nested_phases_do_not_hold_the_observation_writer() {
        let output = RefCell::new(Vec::new());
        let emit = |line: &[u8]| output.borrow_mut().write_all(line);
        observe_with(
            Phase::BootstrapVerify,
            || observe_with(Phase::BundleVerify, || Ok(()), emit),
            emit,
        )
        .unwrap();
        let bytes = output.into_inner();
        let records: Vec<serde_json::Value> = String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["phase"], "bootstrap_verify");
        assert_eq!(records[1]["phase"], "bundle_verify");
        assert_eq!(records[2]["phase"], "bundle_verify");
        assert_eq!(records[3]["phase"], "bootstrap_verify");
    }
}
