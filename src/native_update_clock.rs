//! A clock for one fixed lifecycle-participant CLI invocation. Other application
//! processes and direct library callers have no installed deadline.
use anyhow::{Context, Result, ensure};
use std::{
    sync::{OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

const OUTPUT_RESERVE: Duration = Duration::from_secs(2);
const EXIT_RESERVE: Duration = Duration::from_secs(1);
const UNKNOWN_EXIT: i32 = 124;

#[derive(Clone, Copy)]
struct Budget {
    work: Instant,
    exit: Instant,
}
impl Budget {
    fn cap_at(&self, now: Instant, maximum: Duration) -> Result<Duration> {
        let remaining = self
            .work
            .checked_duration_since(now)
            .filter(|v| !v.is_zero());
        Ok(remaining
            .context("Participant deadline reached; retained outcome is unknown")?
            .min(maximum))
    }
}
static DEADLINE: OnceLock<Budget> = OnceLock::new();

/// Only the fixed CLI entry installs this; there is no environment or user
/// override, and a process cannot renew its budget for a second action.
pub(crate) fn install(action: &str) -> Result<Invocation> {
    let wire = match action {
        "prepare" | "restore" => Duration::from_secs(30),
        "verify" => Duration::from_secs(300),
        _ => anyhow::bail!("Unsupported participant clock action"),
    };
    let now = Instant::now();
    install_budget(Budget {
        work: now + wire - OUTPUT_RESERVE,
        exit: now + wire - EXIT_RESERVE,
    })
}
fn install_budget(budget: Budget) -> Result<Invocation> {
    ensure!(budget.work < budget.exit, "Invalid participant deadline");
    DEADLINE
        .set(budget)
        .map_err(|_| anyhow::anyhow!("A participant invocation clock is already installed"))?;
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("participant-deadline".into())
        .spawn(move || {
            let remaining = budget.exit.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(()) => return,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // An error/unwind may drop the guard. Do not remove the bound
                    // while runtime teardown or an OS call could still be blocked.
                    thread::sleep(budget.exit.saturating_duration_since(Instant::now()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            // No logging, journal writes, cancellation claim, or compensation here:
            // even stderr may be blocked. Do not run cleanup on the stuck process.
            terminate_unknown();
        })
        .context("Cannot establish participant invocation deadline")?;
    Ok(Invocation { sender, budget })
}

fn terminate_unknown() -> ! {
    // std::process::exit flushes Rust stdout and runs platform exit handlers.
    // Those can block on output or locks held by the operation we are bounding.
    #[cfg(unix)]
    unsafe {
        libc::_exit(UNKNOWN_EXIT)
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
            // Only this participant process, never a recorded PID or child.
            // Successful self-termination does not return or run DLL detach code.
            let _ = TerminateProcess(GetCurrentProcess(), UNKNOWN_EXIT as u32);
        }
        // A failed platform termination must not return to work or run cleanup.
        std::process::abort()
    }
}

pub(crate) struct Invocation {
    sender: mpsc::Sender<()>,
    budget: Budget,
}
impl Invocation {
    /// Call only after the complete command result has been serialized/flushed.
    pub(crate) fn complete(self) -> Result<()> {
        ensure!(
            Instant::now() < self.budget.exit,
            "Participant result missed its exit deadline"
        );
        self.sender
            .send(())
            .context("Participant watchdog already settled")
    }
}

/// Preserve existing operation ceilings, but never renew the enclosing clock.
pub(crate) fn cap(maximum: Duration) -> Result<Duration> {
    match DEADLINE.get() {
        Some(deadline) => deadline.cap_at(Instant::now(), maximum),
        None => Ok(maximum),
    }
}
pub(crate) fn check() -> Result<()> {
    cap(Duration::MAX).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn nested_caps_share_one_deadline_and_do_not_reset_it() {
        let now = Instant::now();
        let budget = Budget {
            work: now + Duration::from_secs(28),
            exit: now + Duration::from_secs(29),
        };
        assert_eq!(
            budget.cap_at(now, Duration::from_secs(3)).unwrap(),
            Duration::from_secs(3)
        );
        assert_eq!(
            budget
                .cap_at(now + Duration::from_secs(27), Duration::from_secs(30))
                .unwrap(),
            Duration::from_secs(1)
        );
        assert!(
            budget
                .cap_at(now + Duration::from_secs(28), Duration::from_secs(30))
                .is_err()
        );
        assert!(
            budget
                .cap_at(now + Duration::from_secs(29), Duration::from_secs(30))
                .is_err()
        );
    }

    #[test]
    fn normal_library_process_has_no_installed_limit() {
        assert_eq!(
            cap(Duration::from_secs(900)).unwrap(),
            Duration::from_secs(900)
        );
        check().unwrap();
    }

    #[test]
    #[ignore = "bounded blocked-operation subprocess"]
    fn blocked_operation_fixture() {
        let now = Instant::now();
        let _clock = install_budget(Budget {
            work: now + Duration::from_millis(150),
            exit: now + Duration::from_millis(250),
        })
        .unwrap();
        // Models a synchronous COM call/reader join, not an async timeout future.
        thread::sleep(Duration::from_secs(60));
        panic!("Watchdog did not bound a blocked native operation");
    }

    #[test]
    #[ignore = "bounded completed-invocation subprocess"]
    fn completed_operation_fixture() {
        use std::io::Write;
        let now = Instant::now();
        let clock = install_budget(Budget {
            work: now + Duration::from_millis(150),
            exit: now + Duration::from_millis(250),
        })
        .unwrap();
        std::io::stdout().write_all(b"completed fixture\n").unwrap();
        std::io::stdout().flush().unwrap();
        clock.complete().unwrap();
        thread::sleep(Duration::from_millis(400));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "bounded subprocess with blocked exit-time stdout cleanup"]
    fn buffered_stdout_fixture() {
        use std::{
            io::Write,
            os::fd::{AsRawFd, FromRawFd, OwnedFd},
        };
        // Clear test-harness output before replacing stdout. Keep the read end
        // open without consuming it, so the full pipe blocks instead of EPIPE.
        std::io::stdout().flush().unwrap();
        let mut descriptors = [-1; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let _reader = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        let flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let bytes = [b'x'; 4096];
        let mut size = bytes.len();
        loop {
            let written = unsafe { libc::write(writer.as_raw_fd(), bytes.as_ptr().cast(), size) };
            if written >= 0 {
                continue;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            if size == 1 {
                break;
            }
            size = 1; // Fill any capacity smaller than an atomic pipe write too.
        }
        assert_eq!(
            unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags) },
            0
        );
        assert_eq!(
            unsafe { libc::dup2(writer.as_raw_fd(), libc::STDOUT_FILENO) },
            libc::STDOUT_FILENO
        );
        drop(writer);
        // No newline: leave one byte buffered and RELEASE the stdout lock.
        // std::process::exit would acquire it and block flushing during cleanup.
        std::io::stdout().write_all(b"x").unwrap();
        let now = Instant::now();
        let _clock = install_budget(Budget {
            work: now + Duration::from_millis(150),
            exit: now + Duration::from_millis(250),
        })
        .unwrap();
        thread::sleep(Duration::from_secs(60));
        panic!("Watchdog waited for buffered stdout cleanup");
    }

    fn fixture(name: &str) -> std::process::ExitStatus {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", name, "--nocapture"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= end {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Exact clock fixture did not exit within five seconds");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn blocked_operation_exits_unknown_without_waiting_for_it() {
        assert_eq!(
            fixture("native_update_clock::tests::blocked_operation_fixture").code(),
            Some(UNKNOWN_EXIT)
        );
    }
    #[test]
    fn completed_and_flushed_invocation_cancels_watchdog() {
        assert!(fixture("native_update_clock::tests::completed_operation_fixture").success());
    }
    #[cfg(unix)]
    #[test]
    fn watchdog_exits_unknown_without_flushing_buffered_stdout() {
        assert_eq!(
            fixture("native_update_clock::tests::buffered_stdout_fixture").code(),
            Some(UNKNOWN_EXIT)
        );
    }
}
