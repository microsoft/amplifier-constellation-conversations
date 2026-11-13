//! Best-effort, content-free observations of one synchronous browser CHECK.
//!
//! This is not an installation lock, cancellation signal, or completion receipt.
//! In particular, `returned` says nothing about delivery of an HTTP response.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    fs::{self, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 32_768;
const MAX_EVENTS: usize = 64;
const DIRECTORY: &str = "native-browser-update";
const FILE: &str = "check-observation.json";
static OWNER: AtomicBool = AtomicBool::new(false);
thread_local! {
    static CURRENT: RefCell<Option<Rc<Context>>> = const { RefCell::new(None) };
    static SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Cleanup,
    Prepare,
    Lock,
    PreviousVerify,
    Inputs,
    Staging,
    PackageRecord,
    Rebind,
    Launcher,
    PublicationGuards,
    BindingValidation,
    LedgerPublish,
    Response,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Cleanup => "check_cleanup",
            Self::Prepare => "check_prepare",
            Self::Lock => "check_lock",
            Self::PreviousVerify => "check_previous_verify",
            Self::Inputs => "check_inputs",
            Self::Staging => "check_stage",
            Self::PackageRecord => "check_package_record",
            Self::Rebind => "check_rebind",
            Self::Launcher => "check_launcher",
            Self::PublicationGuards => "check_publication_guards",
            Self::BindingValidation => "check_binding_validation",
            Self::LedgerPublish => "check_ledger_publish",
            Self::Response => "check_response",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Running,
    Returned,
    Unwound,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventState {
    Started,
    Returned,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok,
    Error,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    sequence: u64,
    phase: String,
    state: EventState,
    at_ms: u64,
    elapsed_ms: Option<u64>,
    result: Option<Outcome>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    observation_only: bool,
    id: String,
    pid: u32,
    process_nonce_sha256: Option<String>,
    started_at_ms: u64,
    queued_ms: u64,
    elapsed_ms: u64,
    prior_observation_io_ms: u64,
    state: State,
    result: Option<Outcome>,
    truncated: bool,
    events: Vec<Event>,
}

struct Context {
    home: PathBuf,
    path: PathBuf,
    started: Instant,
    snapshot: RefCell<Snapshot>,
    enabled: Cell<bool>,
    published: Cell<bool>,
    prior_io: Cell<Duration>,
}

struct Token<'a>(&'a AtomicBool);
impl Drop for Token<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Suppression(Option<bool>);
impl Drop for Suppression {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            let _ = SUPPRESSED.try_with(|value| value.set(previous));
        }
    }
}

fn without_capture<T, E>(operation: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    // A fallback must not inherit a caller's diagnostic context. Leave CURRENT
    // intact so the outer observation resumes after return or the original panic.
    let _suppression = Suppression(SUPPRESSED.try_with(|value| value.replace(true)).ok());
    operation()
}

struct Scope {
    context: Rc<Context>,
    previous: Option<Rc<Context>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        // No TLS or snapshot borrow is held across the final file operation.
        let previous = self.previous.take();
        let _ = CURRENT.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut()
                && slot
                    .as_ref()
                    .is_some_and(|value| Rc::ptr_eq(value, &self.context))
            {
                *slot = previous;
            }
        });
        self.context.unwound_if_running();
    }
}

fn milliseconds(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

fn current() -> Option<Rc<Context>> {
    if SUPPRESSED.try_with(Cell::get).unwrap_or(true) {
        return None;
    }
    CURRENT
        .try_with(|slot| slot.try_borrow().ok().and_then(|value| value.clone()))
        .ok()
        .flatten()
        .filter(|context| context.enabled.get())
}

pub(crate) fn run<T, E>(
    home: &Path,
    queued: Instant,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    run_with_token(home, queued, &OWNER, operation)
}

fn run_with_token<T, E>(
    home: &Path,
    queued: Instant,
    owner: &AtomicBool,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    if owner
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return without_capture(operation);
    }
    // Created before Scope so it is released after the last scoped observation,
    // including unwinding. It never acquires any product lock.
    let _token = Token(owner);
    let started = Instant::now();
    let Some(context) = Context::new(home, queued, started) else {
        return without_capture(operation);
    };
    let previous = CURRENT
        .try_with(|slot| {
            slot.try_borrow_mut()
                .ok()
                .map(|mut value| value.replace(Rc::clone(&context)))
        })
        .ok()
        .flatten();
    let Some(previous) = previous else {
        return without_capture(operation);
    };
    let _scope = Scope {
        context: Rc::clone(&context),
        previous,
    };
    context.publish();
    let result = operation();
    context.finish(if result.is_ok() {
        Outcome::Ok
    } else {
        Outcome::Error
    });
    result
}

pub(crate) fn observe<T, E>(
    stage: Stage,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let Some(context) = current() else {
        return operation();
    };
    context.record(stage.name(), EventState::Started, None, None);
    let started = Instant::now();
    let result = operation();
    context.record(
        stage.name(),
        EventState::Returned,
        Some(milliseconds(started.elapsed())),
        Some(if result.is_ok() {
            Outcome::Ok
        } else {
            Outcome::Error
        }),
    );
    result
}

fn installation_phase(phase: &str) -> Option<&'static str> {
    match phase {
        "download_verify" => Some("download_verify"),
        "identity_create" => Some("identity_create"),
        "bootstrap_verify" => Some("bootstrap_verify"),
        "bundle_verify" => Some("bundle_verify"),
        "bundle_copy" => Some("bundle_copy"),
        "bundle_verify_existing" => Some("bundle_verify_existing"),
        "bundle_verify_copied" => Some("bundle_verify_copied"),
        "bundle_publish" => Some("bundle_publish"),
        "binary_publish" => Some("binary_publish"),
        "receipt_publish" => Some("receipt_publish"),
        "installed_verify" => Some("installed_verify"),
        _ => None,
    }
}

fn known_phase(phase: &str) -> bool {
    installation_phase(phase).is_some()
        || matches!(
            phase,
            "check_cleanup"
                | "check_prepare"
                | "check_lock"
                | "check_previous_verify"
                | "check_inputs"
                | "check_stage"
                | "check_package_record"
                | "check_rebind"
                | "check_launcher"
                | "check_publication_guards"
                | "check_binding_validation"
                | "check_ledger_publish"
                | "check_response"
        )
}

/// Forward only the existing content-free installation records. Without a CHECK
/// context this returns before parsing, allocating, timing, or performing I/O.
pub(crate) fn capture_installation(line: &[u8]) {
    let Some(context) = current() else { return };
    if line.len() > 512 {
        return;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    let Some(object) = value.as_object() else {
        return;
    };
    if value["event"] != "native_installation_phase" {
        return;
    }
    let Some(phase) = value["phase"].as_str().and_then(installation_phase) else {
        return;
    };
    match value["state"].as_str() {
        Some("started") if object.len() == 3 => {
            context.record(phase, EventState::Started, None, None);
        }
        Some("returned") if object.len() == 5 => {
            let Some(elapsed) = value["elapsed_ms"].as_u64() else {
                return;
            };
            let outcome = match value["result"].as_str() {
                Some("ok") => Outcome::Ok,
                Some("error") => Outcome::Error,
                _ => return,
            };
            context.record(phase, EventState::Returned, Some(elapsed), Some(outcome));
        }
        _ => (),
    }
}

impl Context {
    fn new(home: &Path, queued: Instant, started: Instant) -> Option<Rc<Self>> {
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(milliseconds)
            .unwrap_or(0);
        if !home.is_absolute()
            || amplifier_native_platform::validate_private_directory(home).is_err()
        {
            return None;
        }
        let directory = home.join(DIRECTORY);
        match fs::symlink_metadata(&directory) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match amplifier_native_platform::private_create_dir(&directory) {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(_) => return None,
                }
            }
            Err(_) => return None,
        }
        if amplifier_native_platform::validate_private_directory(&directory).is_err() {
            return None;
        }
        Some(Rc::new(Self {
            home: home.to_owned(),
            path: directory.join(FILE),
            started,
            snapshot: RefCell::new(Snapshot {
                schema: 1,
                observation_only: true,
                id: uuid::Uuid::new_v4().to_string(),
                pid: std::process::id(),
                process_nonce_sha256: runtime_nonce(home),
                started_at_ms,
                queued_ms: milliseconds(started.saturating_duration_since(queued)),
                elapsed_ms: 0,
                prior_observation_io_ms: 0,
                state: State::Running,
                result: None,
                truncated: false,
                events: Vec::new(),
            }),
            enabled: Cell::new(true),
            published: Cell::new(false),
            prior_io: Cell::new(Duration::ZERO),
        }))
    }

    fn record(
        &self,
        phase: &'static str,
        state: EventState,
        elapsed_ms: Option<u64>,
        result: Option<Outcome>,
    ) {
        if !self.enabled.get() {
            return;
        }
        {
            let Ok(mut snapshot) = self.snapshot.try_borrow_mut() else {
                self.enabled.set(false);
                return;
            };
            if snapshot.events.len() == MAX_EVENTS {
                if snapshot.truncated {
                    return;
                }
                snapshot.truncated = true;
            } else {
                let sequence = snapshot.events.len() as u64 + 1;
                snapshot.events.push(Event {
                    sequence,
                    phase: phase.to_owned(),
                    state,
                    at_ms: milliseconds(self.started.elapsed()),
                    elapsed_ms,
                    result,
                });
            }
        }
        self.publish();
    }

    fn finish(&self, result: Outcome) {
        if let Ok(mut snapshot) = self.snapshot.try_borrow_mut() {
            snapshot.state = State::Returned;
            snapshot.result = Some(result);
        } else {
            self.enabled.set(false);
        }
        self.publish();
    }

    fn unwound_if_running(&self) {
        let write = if let Ok(mut snapshot) = self.snapshot.try_borrow_mut() {
            if snapshot.state != State::Running {
                false
            } else {
                snapshot.state = State::Unwound;
                snapshot.result = None;
                true
            }
        } else {
            self.enabled.set(false);
            false
        };
        if write {
            self.publish();
        }
    }

    fn publish(&self) {
        if !self.enabled.get() {
            return;
        }
        let started = Instant::now();
        let snapshot = self.snapshot.try_borrow().ok().map(|value| {
            let mut value = value.clone();
            value.elapsed_ms = milliseconds(self.started.elapsed());
            value.prior_observation_io_ms = milliseconds(self.prior_io.get());
            value
        });
        let saved = snapshot.is_some_and(|snapshot| {
            // Validate the fixed installation-owned parents, not arbitrary
            // Documents/tmp ancestors. Never repair an existing policy.
            if amplifier_native_platform::validate_private_directory(&self.home).is_err()
                || amplifier_native_platform::validate_private_directory(&self.home.join(DIRECTORY))
                    .is_err()
                || !destination_valid(
                    &self.path,
                    self.published.get().then_some(snapshot.id.as_str()),
                )
            {
                return false;
            }
            let Ok(bytes) = serde_json::to_vec(&snapshot) else {
                return false;
            };
            bytes.len() <= MAX_BYTES
                && amplifier_native_platform::private_write_data(&self.path, &bytes).is_ok()
        });
        self.prior_io
            .set(self.prior_io.get().saturating_add(started.elapsed()));
        if saved {
            self.published.set(true);
        } else {
            // No retries and no effect on the CHECK's original value/error.
            self.enabled.set(false);
        }
    }
}

fn ordinary(metadata: &Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return false;
        }
    }
    true
}

fn bounded_file(path: &Path) -> Option<Vec<u8>> {
    let before = fs::symlink_metadata(path).ok()?;
    if !ordinary(&before) || before.len() > MAX_BYTES as u64 {
        return None;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).ok()?;
    if !ordinary(&file.metadata().ok()?) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_BYTES || !ordinary(&fs::symlink_metadata(path).ok()?) {
        return None;
    }
    Some(bytes)
}

fn runtime_nonce(home: &Path) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_slice(&bounded_file(&home.join("runtime.json"))?).ok()?;
    if value["pid"].as_u64()? != u64::from(std::process::id()) {
        return None;
    }
    let nonce = value["process_nonce"].as_str()?;
    if nonce.is_empty() || nonce.len() > 256 {
        return None;
    }
    Some(format!("{:x}", Sha256::digest(nonce.as_bytes())))
}

fn destination_valid(path: &Path, expected_id: Option<&str>) -> bool {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return expected_id.is_none();
        }
        Err(_) => return false,
        Ok(_) => (),
    }
    let Some(bytes) = bounded_file(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    // Option fields must be present as null rather than silently defaulted.
    if value.as_object().is_none_or(|object| object.len() != 13)
        || value.get("result").is_none()
        || value.get("process_nonce_sha256").is_none()
        || !value["events"].as_array().is_some_and(|events| {
            events.len() <= MAX_EVENTS
                && events.iter().all(|event| {
                    event.as_object().is_some_and(|object| object.len() == 6)
                        && event.get("elapsed_ms").is_some()
                        && event.get("result").is_some()
                })
        })
    {
        return false;
    }
    let Ok(snapshot) = serde_json::from_value::<Snapshot>(value) else {
        return false;
    };
    snapshot.schema == 1
        && snapshot.observation_only
        && snapshot.pid != 0
        && uuid::Uuid::parse_str(&snapshot.id).is_ok_and(|id| id.to_string() == snapshot.id)
        && expected_id.is_none_or(|id| snapshot.id == id)
        && snapshot.process_nonce_sha256.as_ref().is_none_or(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        && (snapshot.state == State::Returned) == snapshot.result.is_some()
        && snapshot.events.iter().enumerate().all(|(index, event)| {
            event.sequence == index as u64 + 1
                && known_phase(&event.phase)
                && event.at_ms <= snapshot.elapsed_ms
                && match event.state {
                    EventState::Started => event.elapsed_ms.is_none() && event.result.is_none(),
                    EventState::Returned => event.elapsed_ms.is_some() && event.result.is_some(),
                }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{panic::AssertUnwindSafe, sync::Arc, sync::mpsc};

    fn home() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        amplifier_native_platform::private_dir(directory.path()).unwrap();
        directory
    }

    fn path(home: &Path) -> PathBuf {
        home.join(DIRECTORY).join(FILE)
    }

    fn snapshot(home: &Path) -> Value {
        serde_json::from_slice(&fs::read(path(home)).unwrap()).unwrap()
    }

    fn write(path: &Path, value: &Value) {
        amplifier_native_platform::private_write_data(path, &serde_json::to_vec(value).unwrap())
            .unwrap();
    }

    #[test]
    fn blocked_phase_stays_observable_after_result_observer_disappears() {
        let home = home();
        let owner = Arc::new(AtomicBool::new(false));
        let worker_owner = Arc::clone(&owner);
        let worker_home = home.path().to_owned();
        let (entered, entry) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (sender, observer) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result: Result<_, ()> =
                run_with_token(&worker_home, Instant::now(), &worker_owner, || {
                    observe(Stage::Staging, || {
                        entered.send(()).unwrap();
                        blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(42)
                    })
                });
            let _ = sender.send(result);
        });
        entry.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(observer);
        assert!(owner.load(Ordering::Acquire));
        let live = snapshot(home.path());
        assert_eq!(live["state"], "running");
        assert_eq!(live["events"][0]["state"], "started");
        assert_eq!(live["events"][0]["phase"], "check_stage");
        assert_eq!(live["events"][0]["elapsed_ms"], Value::Null);
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(!owner.load(Ordering::Acquire));
        let finished = snapshot(home.path());
        assert_eq!(finished["id"], live["id"]);
        assert_eq!(finished["state"], "returned");
        assert_eq!(finished["result"], "ok");
        assert_eq!(finished["events"][1]["state"], "returned");
    }

    #[test]
    fn concurrent_diagnostic_loser_runs_without_waiting_or_overwriting() {
        let home = home();
        let owner = Arc::new(AtomicBool::new(false));
        let worker_owner = Arc::clone(&owner);
        let worker_home = home.path().to_owned();
        let (entered, entry) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_with_token(&worker_home, Instant::now(), &worker_owner, || {
                observe(Stage::Lock, || {
                    entered.send(()).unwrap();
                    blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok::<_, ()>(())
                })
            })
        });
        entry.recv_timeout(Duration::from_secs(5)).unwrap();
        let before = fs::read(path(home.path())).unwrap();
        let value = run_with_token(home.path(), Instant::now(), &owner, || {
            observe(Stage::Response, || Ok::<_, ()>(93))
        });
        assert_eq!(value, Ok(93));
        assert_eq!(fs::read(path(home.path())).unwrap(), before);
        release.send(()).unwrap();
        worker.join().unwrap().unwrap();
    }

    fn assert_only_outer_events(home: &Path) {
        let value = snapshot(home);
        let phases: Vec<_> = value["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["phase"].as_str().unwrap())
            .collect();
        assert_eq!(
            phases,
            [
                "check_prepare",
                "check_prepare",
                "check_response",
                "check_response"
            ]
        );
        assert_eq!(value["state"], "returned");
        assert_eq!(value["result"], "ok");
    }

    #[test]
    fn nested_busy_run_preserves_result_without_outer_attribution() {
        let home = home();
        let owner = AtomicBool::new(false);
        let calls = Cell::new(0);
        run_with_token(home.path(), Instant::now(), &owner, || {
            observe(Stage::Prepare, || Ok::<_, ()>(()))?;
            let before = fs::read(path(home.path())).unwrap();
            let value = Box::new(73);
            let identity = &*value as *const _;
            let result = run_with_token(home.path(), Instant::now(), &owner, || {
                calls.set(calls.get() + 1);
                observe(Stage::Inputs, || {
                    capture_installation(br#"{"event":"native_installation_phase","phase":"bundle_copy","state":"started"}"#);
                    Ok::<_, ()>(value)
                })
            })?;
            assert_eq!(&*result as *const _, identity);
            assert_eq!(calls.get(), 1);
            assert_eq!(fs::read(path(home.path())).unwrap(), before);
            assert!(owner.load(Ordering::Acquire));
            observe(Stage::Response, || Ok::<_, ()>(()))
        }).unwrap();
        assert_only_outer_events(home.path());
        assert!(!owner.load(Ordering::Acquire));
    }

    #[test]
    fn nested_invalid_home_preserves_error_without_outer_attribution() {
        let home = home();
        let owner = AtomicBool::new(false);
        let inner_owner = AtomicBool::new(false);
        let calls = Cell::new(0);
        run_with_token(home.path(), Instant::now(), &owner, || {
            observe(Stage::Prepare, || Ok::<_, ()>(()))?;
            let before = fs::read(path(home.path())).unwrap();
            let error = Box::new("inner-original-error");
            let identity = &*error as *const _;
            let result: Result<(), _> = run_with_token(
                Path::new("relative-invalid-home"),
                Instant::now(),
                &inner_owner,
                || {
                    calls.set(calls.get() + 1);
                    observe(Stage::Staging, || Err(error))
                },
            );
            assert_eq!(&*result.unwrap_err() as *const _, identity);
            assert_eq!(calls.get(), 1);
            assert!(!inner_owner.load(Ordering::Acquire));
            assert_eq!(fs::read(path(home.path())).unwrap(), before);
            observe(Stage::Response, || Ok::<_, ()>(()))
        })
        .unwrap();
        assert_only_outer_events(home.path());
    }

    #[test]
    fn nested_busy_panic_restores_outer_scope_without_inner_events() {
        let home = home();
        let owner = AtomicBool::new(false);
        let calls = Cell::new(0);
        run_with_token(home.path(), Instant::now(), &owner, || {
            observe(Stage::Prepare, || Ok::<_, ()>(()))?;
            let before = fs::read(path(home.path())).unwrap();
            let panic = std::panic::catch_unwind(AssertUnwindSafe(|| {
                run_with_token(home.path(), Instant::now(), &owner, || -> Result<(), ()> {
                    calls.set(calls.get() + 1);
                    observe(Stage::Staging, || panic!("inner-original-panic"))
                })
            }))
            .unwrap_err();
            assert_eq!(panic.downcast_ref::<&str>(), Some(&"inner-original-panic"));
            assert_eq!(calls.get(), 1);
            assert!(owner.load(Ordering::Acquire));
            assert_eq!(fs::read(path(home.path())).unwrap(), before);
            observe(Stage::Response, || Ok::<_, ()>(()))
        })
        .unwrap();
        assert_only_outer_events(home.path());
    }

    #[test]
    fn nested_tls_installation_failure_does_not_resume_outer_capture_early() {
        let inner_home = home();
        let home = home();
        let owner = AtomicBool::new(false);
        let inner_owner = AtomicBool::new(false);
        let calls = Cell::new(0);
        run_with_token(home.path(), Instant::now(), &owner, || {
            observe(Stage::Prepare, || Ok::<_, ()>(()))?;
            let before = fs::read(path(home.path())).unwrap();
            CURRENT.with(|slot| {
                // The short initial conflict ends inside the unchanged action;
                // fallback suppression must outlive that failed TLS installation.
                let borrowed = slot.borrow_mut();
                run_with_token(inner_home.path(), Instant::now(), &inner_owner, || {
                    drop(borrowed);
                    calls.set(calls.get() + 1);
                    observe(Stage::Inputs, || Ok::<_, ()>(()))
                })
            })?;
            assert_eq!(calls.get(), 1);
            assert!(!inner_owner.load(Ordering::Acquire));
            assert!(!path(inner_home.path()).exists());
            assert_eq!(fs::read(path(home.path())).unwrap(), before);
            observe(Stage::Response, || Ok::<_, ()>(()))
        })
        .unwrap();
        assert_only_outer_events(home.path());
    }

    #[test]
    fn original_error_identity_and_nested_events_preserve_no_secret_text() {
        let home = home();
        let owner = AtomicBool::new(false);
        write(
            &home.path().join("runtime.json"),
            &json!({"pid":std::process::id(),"process_nonce":"private-nonce-sentinel", "token":"private-config-sentinel"}),
        );
        let error = Box::new("private-error-and-path-sentinel");
        let identity = &*error as *const _;
        let result: Result<(), _> = run_with_token(home.path(), Instant::now(), &owner, || {
            observe(Stage::Prepare, || {
                capture_installation(br#"{"event":"native_installation_phase","phase":"bundle_copy","state":"started"}"#);
                capture_installation(br#"{"event":"native_installation_phase","phase":"bundle_copy","state":"returned","elapsed_ms":4,"result":"ok"}"#);
                Err(error)
            })
        });
        assert_eq!(&*result.unwrap_err() as *const _, identity);
        let bytes = fs::read(path(home.path())).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("sentinel"));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["result"], "error");
        assert_eq!(value["events"].as_array().unwrap().len(), 4);
        assert_eq!(value["events"][1]["phase"], "bundle_copy");
        assert_eq!(
            value["process_nonce_sha256"],
            format!("{:x}", Sha256::digest(b"private-nonce-sentinel"))
        );
    }

    #[test]
    fn original_panic_unwinds_and_releases_tls_and_diagnostic_token() {
        let home = home();
        let owner = AtomicBool::new(false);
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run_with_token(home.path(), Instant::now(), &owner, || -> Result<(), ()> {
                observe(Stage::Lock, || panic!("original-panic-sentinel"))
            })
        }));
        let payload = result.unwrap_err();
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"original-panic-sentinel")
        );
        assert!(!owner.load(Ordering::Acquire));
        assert!(current().is_none());
        let value = snapshot(home.path());
        assert_eq!(value["state"], "unwound");
        assert_eq!(value["result"], Value::Null);
        assert!(!value.to_string().contains("sentinel"));
        run_with_token(home.path(), Instant::now(), &owner, || Ok::<_, ()>(())).unwrap();
        assert_eq!(snapshot(home.path())["state"], "returned");
    }

    #[test]
    fn event_limit_is_bounded_and_truncation_does_not_change_result() {
        let home = home();
        let owner = AtomicBool::new(false);
        let result = run_with_token(home.path(), Instant::now(), &owner, || {
            for _ in 0..100 {
                observe(Stage::Inputs, || Ok::<_, ()>(()))?;
            }
            Ok::<_, ()>(71)
        });
        assert_eq!(result, Ok(71));
        let bytes = fs::read(path(home.path())).unwrap();
        assert!(bytes.len() <= MAX_BYTES);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["truncated"], true);
        assert_eq!(value["events"].as_array().unwrap().len(), MAX_EVENTS);
        assert_eq!(value["events"][63]["sequence"], 64);
        assert_eq!(value["state"], "returned");
    }

    #[test]
    fn malformed_oversize_or_directory_destination_is_never_repaired() {
        for bytes in [b"invalid-json".to_vec(), vec![b' '; MAX_BYTES + 1]] {
            let home = home();
            amplifier_native_platform::private_create_dir(&home.path().join(DIRECTORY)).unwrap();
            amplifier_native_platform::private_write_data(&path(home.path()), &bytes).unwrap();
            let owner = AtomicBool::new(false);
            assert_eq!(
                run_with_token(home.path(), Instant::now(), &owner, || Ok::<_, ()>(8)),
                Ok(8)
            );
            assert_eq!(fs::read(path(home.path())).unwrap(), bytes);
        }
        let home = home();
        amplifier_native_platform::private_create_dir(&home.path().join(DIRECTORY)).unwrap();
        amplifier_native_platform::private_create_dir(&path(home.path())).unwrap();
        run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(path(home.path()).is_dir());
    }

    #[test]
    fn stale_valid_observation_gets_new_id_but_unknown_fields_are_preserved() {
        let home = home();
        let owner = AtomicBool::new(false);
        run_with_token(home.path(), Instant::now(), &owner, || Ok::<_, ()>(())).unwrap();
        let old = snapshot(home.path())["id"].clone();
        run_with_token(home.path(), Instant::now(), &owner, || Ok::<_, ()>(())).unwrap();
        assert_ne!(snapshot(home.path())["id"], old);
        let mut malformed = snapshot(home.path());
        malformed["unknown_private_field"] = json!("do-not-repair");
        write(&path(home.path()), &malformed);
        let bytes = fs::read(path(home.path())).unwrap();
        run_with_token(home.path(), Instant::now(), &owner, || Ok::<_, ()>(())).unwrap();
        assert_eq!(fs::read(path(home.path())).unwrap(), bytes);
    }

    #[test]
    fn missing_changed_or_unbounded_runtime_generation_stays_unknown() {
        for runtime in [
            None,
            Some(json!({"pid":u64::from(std::process::id())+1,"process_nonce":"unknown"})),
            Some(json!({"pid":std::process::id(),"process_nonce":""})),
            Some(json!({"pid":std::process::id(),"process_nonce":"x".repeat(257)})),
            Some(json!({"pid":std::process::id(),"process_nonce":17})),
        ] {
            let home = home();
            if let Some(runtime) = runtime {
                write(&home.path().join("runtime.json"), &runtime);
            }
            run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
                Ok::<_, ()>(())
            })
            .unwrap();
            assert_eq!(snapshot(home.path())["process_nonce_sha256"], Value::Null);
        }
    }

    #[test]
    fn malformed_or_oversized_runtime_does_not_disable_the_observation() {
        for bytes in [b"invalid-runtime".to_vec(), vec![b' '; MAX_BYTES + 1]] {
            let home = home();
            amplifier_native_platform::private_write_data(
                &home.path().join("runtime.json"),
                &bytes,
            )
            .unwrap();
            run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
                Ok::<_, ()>(())
            })
            .unwrap();
            let value = snapshot(home.path());
            assert_eq!(value["state"], "returned");
            assert_eq!(value["process_nonce_sha256"], Value::Null);
            assert_eq!(fs::read(home.path().join("runtime.json")).unwrap(), bytes);
        }
    }

    #[test]
    fn absent_scope_leaves_operation_and_unparsed_input_alone() {
        assert!(current().is_none());
        capture_installation(&[0xff; 1024]);
        let value = Box::new(93);
        let identity = &*value as *const _;
        let result = observe(Stage::Response, || Ok::<_, ()>(value)).unwrap();
        assert_eq!(&*result as *const _, identity);
        assert!(current().is_none());
    }

    #[test]
    fn malformed_phase_data_and_secrets_are_not_forwarded() {
        let home = home();
        run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
            for line in [
                br#"{"event":"native_installation_phase","phase":"private-path","state":"started"}"#.as_slice(),
                br#"{"event":"native_installation_phase","phase":"bundle_copy","state":"started","token":"secret"}"#,
                br#"{"event":"native_installation_phase","phase":"bundle_copy","state":"returned","elapsed_ms":0,"result":"secret"}"#,
                b"not json",
            ] {
                capture_installation(line);
            }
            capture_installation(br#"{"event":"native_installation_phase","phase":"bundle_verify_existing","state":"started"}"#);
            Ok::<_, ()>(())
        }).unwrap();
        let value = snapshot(home.path());
        assert_eq!(value["events"].as_array().unwrap().len(), 1);
        assert_eq!(value["events"][0]["phase"], "bundle_verify_existing");
        assert!(!value.to_string().contains("secret"));
    }

    #[test]
    fn newly_invalid_destination_disables_capture_without_changing_result() {
        let home = home();
        run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
            amplifier_native_platform::private_write_data(&path(home.path()), b"external-invalid")
                .unwrap();
            observe(Stage::PublicationGuards, || Ok::<_, ()>(19))
        })
        .map(|value| assert_eq!(value, 19))
        .unwrap();
        assert_eq!(fs::read(path(home.path())).unwrap(), b"external-invalid");
    }

    #[test]
    fn snapshot_reports_only_prior_observation_io_cost() {
        let home = home();
        let started = Instant::now();
        let context = Context::new(home.path(), started, started).unwrap();
        context.prior_io.set(Duration::from_millis(17));
        context.publish();
        assert_eq!(snapshot(home.path())["prior_observation_io_ms"], 17);
        assert!(context.prior_io.get() >= Duration::from_millis(17));
    }

    #[cfg(unix)]
    #[test]
    fn reparse_destination_and_nonprivate_parent_are_not_repaired() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let home = home();
        let target = home.path().join("retained");
        amplifier_native_platform::private_write_data(&target, b"retained").unwrap();
        amplifier_native_platform::private_create_dir(&home.path().join(DIRECTORY)).unwrap();
        symlink(&target, path(home.path())).unwrap();
        run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"retained");
        assert!(path(home.path()).is_symlink());
        fs::remove_file(path(home.path())).unwrap();
        fs::set_permissions(
            home.path().join(DIRECTORY),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        run_with_token(home.path(), Instant::now(), &AtomicBool::new(false), || {
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(!path(home.path()).exists());
        assert_eq!(
            fs::metadata(home.path().join(DIRECTORY))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}
