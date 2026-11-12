//! OS mechanisms only. Conversations retains its own allowed-account policy.
#[cfg(target_os = "macos")]
pub mod macos {
    // macOS ships OpenPAM. pam-client's Linux constants/signatures are not
    // compatible; keep this small ABI surface matched to Apple's pam_appl.h.
    use std::{
        ffi::{CStr, CString, c_char, c_int, c_void},
        ptr,
    };
    use zeroize::Zeroizing;
    #[repr(C)]
    struct Message {
        style: c_int,
        text: *mut c_char,
    }
    #[repr(C)]
    struct Response {
        text: *mut c_char,
        code: c_int,
    }
    #[repr(C)]
    struct Conversation {
        callback: unsafe extern "C" fn(
            c_int,
            *mut *const Message,
            *mut *mut Response,
            *mut c_void,
        ) -> c_int,
        data: *mut c_void,
    }
    #[link(name = "pam")]
    unsafe extern "C" {
        fn pam_start(
            service: *const c_char,
            user: *const c_char,
            conversation: *const Conversation,
            handle: *mut *mut c_void,
        ) -> c_int;
        fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
        fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
        fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
    }
    struct Password {
        bytes: Zeroizing<Vec<u8>>,
        answered: bool,
    }
    unsafe extern "C" fn conversation(
        count: c_int,
        messages: *mut *const Message,
        output: *mut *mut Response,
        data: *mut c_void,
    ) -> c_int {
        const CONV_ERR: c_int = 6;
        if !(1..=32).contains(&count) || messages.is_null() || output.is_null() || data.is_null() {
            return CONV_ERR;
        }
        unsafe {
            *output = ptr::null_mut();
        }
        let password = unsafe { &mut *data.cast::<Password>() };
        let responses =
            unsafe { libc::calloc(count as usize, size_of::<Response>()).cast::<Response>() };
        if responses.is_null() {
            return 5;
        }
        let mut valid = true;
        for index in 0..count as usize {
            let message = unsafe { *messages.add(index) };
            if message.is_null() {
                valid = false;
                break;
            }
            let message = unsafe { &*message };
            match message.style {
                1 if !password.answered
                    && !message.text.is_null()
                    && unsafe { CStr::from_ptr(message.text) }
                        .to_string_lossy()
                        .trim()
                        .eq_ignore_ascii_case("password:") =>
                {
                    password.answered = true;
                    unsafe {
                        (*responses.add(index)).text = libc::strdup(password.bytes.as_ptr().cast());
                    }
                    if unsafe { (*responses.add(index)).text.is_null() } {
                        valid = false;
                        break;
                    }
                }
                3 | 4 => {} // Informational text; never supply a password as an answer.
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            // OpenPAM owns and frees the C-allocated replies on success.
            unsafe {
                *output = responses;
            }
            0
        } else {
            for index in 0..count as usize {
                unsafe {
                    libc::free((*responses.add(index)).text.cast());
                }
            }
            unsafe {
                libc::free(responses.cast());
            }
            CONV_ERR
        }
    }
    pub fn authenticate(service: &str, username: &str, password: String) -> bool {
        let password = Zeroizing::new(password);
        if password.contains('\0') {
            return false;
        }
        let (Ok(service), Ok(username)) = (CString::new(service), CString::new(username)) else {
            return false;
        };
        let mut bytes = Zeroizing::new(password.as_bytes().to_vec());
        bytes.push(0);
        let mut data = Password {
            bytes,
            answered: false,
        };
        let conversation = Conversation {
            callback: conversation,
            data: (&mut data as *mut Password).cast(),
        };
        let mut handle = ptr::null_mut();
        let mut status = unsafe {
            pam_start(
                service.as_ptr(),
                username.as_ptr(),
                &conversation,
                &mut handle,
            )
        };
        if status == 0 {
            status = unsafe { pam_authenticate(handle, 0) };
            if status == 0 {
                status = unsafe { pam_acct_mgmt(handle, 0) };
            }
        }
        if !handle.is_null() {
            unsafe {
                pam_end(handle, status);
            }
        }
        status == 0
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn prompt_refuses_mfa_and_repeated_passwords() {
            let mut password = Password {
                bytes: Zeroizing::new(b"fixture\0".to_vec()),
                answered: false,
            };
            let mut response = ptr::null_mut();
            for (prompt, expected) in [(c"One-time code:", 6), (c"Password:", 0), (c"Password:", 6)]
            {
                let message = Message {
                    style: 1,
                    text: prompt.as_ptr().cast_mut(),
                };
                let mut pointer = &message as *const Message;
                let code = unsafe {
                    conversation(
                        1,
                        &mut pointer,
                        &mut response,
                        (&mut password as *mut Password).cast(),
                    )
                };
                assert_eq!(code, expected);
                if code == 0 {
                    assert_eq!(
                        unsafe { CStr::from_ptr((*response).text) }.to_bytes(),
                        b"fixture"
                    );
                    unsafe {
                        libc::free((*response).text.cast());
                        libc::free(response.cast());
                    }
                }
            }
        }
        #[test]
        fn missing_pam_policy_fails_closed() {
            assert!(!authenticate(
                "agent-instance-nonexistent-test-policy",
                "agent-instance-no-such-user",
                "fixture".into()
            ));
        }
    }
}

use anyhow::Result;
#[cfg(unix)]
use anyhow::{Context, ensure};
use tokio::process::{Child, Command};

pub fn auth_method() -> &'static str {
    if cfg!(windows) {
        "windows_logon"
    } else {
        "pam"
    }
}

pub fn default_auth_service() -> &'static str {
    if cfg!(target_os = "macos") {
        "login"
    } else {
        "agent-conversations"
    }
}

pub fn executable(name: &str) -> String {
    if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.into()
    }
}

/// Start with a small OS environment; callers add explicitly configured secrets.
/// In particular Windows needs its system paths even when PATH is supplied.
pub fn child_environment(command: &mut Command) {
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LANG",
        "SYSTEMROOT",
        "WINDIR",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "SSH_AUTH_SOCK",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[cfg(unix)]
pub fn current_user() -> Result<String> {
    // Resolve the effective OS account, not a caller-controlled USER variable or shell.
    let size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut size = usize::try_from(size)
        .unwrap_or(16_384)
        .clamp(1024, 1_048_576);
    loop {
        let mut record: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; size];
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut record,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE && size < 1_048_576 {
            size = (size * 2).min(1_048_576);
            continue;
        }
        ensure!(
            code == 0 && !result.is_null() && !record.pw_name.is_null(),
            "Cannot identify current OS account"
        );
        let name = unsafe { std::ffi::CStr::from_ptr(record.pw_name) };
        return Ok(name
            .to_str()
            .context("OS account name is not UTF-8")?
            .to_owned());
    }
}

#[cfg(windows)]
pub fn current_user() -> Result<String> {
    windows::current_user()
}

#[cfg(windows)]
pub mod windows {
    use anyhow::{Context, Result, ensure};
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{
            Authentication::Identity::{GetUserNameExW, NameSamCompatible},
            EqualSid, GetTokenInformation, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT,
            LogonUserW, LookupAccountNameW, SidTypeUser, TOKEN_USER, TokenUser,
        },
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
    };
    use zeroize::Zeroizing;

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }
    fn token_user(token: HANDLE) -> Result<Vec<usize>> {
        let mut size = 0;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
        }
        ensure!(size > 0 && size < 65536, "Invalid OS identity size");
        // TOKEN_USER contains a pointer to the SID in this same aligned allocation.
        let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        ensure!(
            unsafe {
                GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), size, &mut size)
            } != 0,
            "Cannot inspect authenticated OS identity"
        );
        Ok(data)
    }
    pub fn current_user() -> Result<String> {
        let mut size = 0;
        unsafe {
            GetUserNameExW(NameSamCompatible, ptr::null_mut(), &mut size);
        }
        ensure!(
            size > 0 && size < 32768,
            "Cannot identify current Windows account"
        );
        let mut data = vec![0u16; size as usize];
        ensure!(
            unsafe { GetUserNameExW(NameSamCompatible, data.as_mut_ptr(), &mut size) },
            "Cannot identify current Windows account"
        );
        String::from_utf16(&data[..size as usize]).context("Invalid Windows account name")
    }
    fn account_sid(account: &str) -> Result<Vec<usize>> {
        ensure!(
            !account.is_empty() && !account.contains('\0'),
            "Invalid configured account"
        );
        let account = wide(account);
        let (mut sid_size, mut domain_size, mut kind) = (0, 0, 0);
        unsafe {
            LookupAccountNameW(
                ptr::null(),
                account.as_ptr(),
                ptr::null_mut(),
                &mut sid_size,
                ptr::null_mut(),
                &mut domain_size,
                &mut kind,
            );
        }
        ensure!(
            sid_size > 0 && sid_size <= 65536 && domain_size <= 32768,
            "Cannot resolve configured Windows account"
        );
        let mut sid = vec![0usize; (sid_size as usize).div_ceil(size_of::<usize>())];
        let mut domain = vec![0u16; (domain_size as usize).max(1)];
        ensure!(
            unsafe {
                LookupAccountNameW(
                    ptr::null(),
                    account.as_ptr(),
                    sid.as_mut_ptr().cast(),
                    &mut sid_size,
                    domain.as_mut_ptr(),
                    &mut domain_size,
                    &mut kind,
                )
            } != 0
                && kind == SidTypeUser,
            "Configured Windows identity must be a user account"
        );
        Ok(sid)
    }
    fn configured_actor(sid: *mut std::ffi::c_void, allowed: &[String]) -> Option<String> {
        // The owner is first, followed by additional users. Aliases resolve to
        // the same SID but always retain one configured actor/audience identity.
        // There is deliberately no comparison with the service process's SID:
        // an installation may authorize several distinct Windows users.
        allowed.iter().find_map(|actor| {
            let account = account_sid(actor).ok()?;
            (unsafe { EqualSid(account.as_ptr().cast_mut().cast(), sid) } != 0)
                .then(|| actor.clone())
        })
    }
    pub fn authenticate_allowed(
        username: &str,
        password: String,
        allowed: &[String],
    ) -> Option<String> {
        // Neither the password nor a credential-bearing command is spawned/logged.
        let password = Zeroizing::new(password);
        if username.is_empty()
            || username.contains('\0')
            || password.contains('\0')
            || allowed.is_empty()
            || allowed.len() > 101
        {
            return None;
        }
        let (domain, user) = if let Some((domain, user)) = username.split_once('\\') {
            (Some(wide(domain)), wide(user))
        } else if username.contains('@') {
            // UPNs require a null domain, rather than the local-computer alias.
            (None, wide(username))
        } else {
            (Some(wide(".")), wide(username))
        };
        let secret = Zeroizing::new(wide(&password));
        let mut token = ptr::null_mut();
        if unsafe {
            LogonUserW(
                user.as_ptr(),
                domain.as_ref().map_or(ptr::null(), |v| v.as_ptr()),
                secret.as_ptr(),
                LOGON32_LOGON_NETWORK,
                LOGON32_PROVIDER_DEFAULT,
                &mut token,
            )
        } == 0
        {
            return None;
        }
        let token = OwnedHandle(token);
        let identity = token_user(token.0).ok()?;
        let sid = unsafe { (*(identity.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        configured_actor(sid, allowed)
    }

    /// Closing the last job handle terminates the attached process and descendants.
    pub struct ProcessJob(usize);
    impl ProcessJob {
        pub fn attach(child: &tokio::process::Child) -> Result<Self> {
            // Bind Tokio's actual process handle rather than reopening a PID.
            // This is immediate post-spawn assignment, not suspended-spawn containment.
            let process = child
                .raw_handle()
                .context("Child exited before job assignment")?;
            let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            ensure!(!job.is_null(), "Cannot create child process job");
            let guard = Self(job as usize);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            ensure!(
                unsafe {
                    SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of_val(&limits) as u32,
                    )
                } != 0,
                "Cannot configure child process cleanup"
            );
            ensure!(
                unsafe { AssignProcessToJobObject(job, process) } != 0,
                "Cannot attach child to process job: {}",
                std::io::Error::last_os_error()
            );
            Ok(guard)
        }
    }
    impl Drop for ProcessJob {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0 as HANDLE);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn configured_nonowner_account_is_eligible_by_sid() {
            let current = current_user().unwrap();
            let sid = account_sid(&current).unwrap();
            let unrelated_owner = ".\\constellation-nonexistent-test-owner".to_owned();
            assert_eq!(
                configured_actor(
                    sid.as_ptr().cast_mut().cast(),
                    &[unrelated_owner.clone(), current.clone()]
                ),
                Some(current.clone())
            );
            assert_eq!(
                configured_actor(sid.as_ptr().cast_mut().cast(), &[unrelated_owner]),
                None
            );
            // Account-name casing is not authority; Windows-resolved SID is.
            let alias = current.to_lowercase();
            assert_eq!(
                configured_actor(sid.as_ptr().cast_mut().cast(), std::slice::from_ref(&alias)),
                Some(alias)
            );
        }
        #[test]
        fn unknown_or_unlisted_credentials_fail_closed() {
            assert_eq!(
                authenticate_allowed(
                    ".\\constellation-nonexistent-test-user",
                    "fixture".into(),
                    &[current_user().unwrap()]
                ),
                None
            );
            assert_eq!(
                authenticate_allowed(&current_user().unwrap(), "fixture".into(), &[]),
                None
            );
        }
    }
}

/// Owns descendants until explicit cleanup or drop. Keep the child unreaped until
/// `terminate`: a reaped Unix PID/process-group ID could otherwise be reused.
pub struct ProcessTree {
    #[cfg(unix)]
    pid: Option<i32>,
    #[cfg(windows)]
    job: Option<windows::ProcessJob>,
}
impl ProcessTree {
    pub fn prepare(command: &mut Command) {
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    pub fn attach(child: &Child) -> Result<Self> {
        #[cfg(unix)]
        let pid = child
            .id()
            .context("Child exited before process-tree ownership was established")?;
        Ok(Self {
            #[cfg(unix)]
            pid: Some(i32::try_from(pid).context("Invalid child process identifier")?),
            #[cfg(windows)]
            job: Some(windows::ProcessJob::attach(child)?),
        })
    }
    pub fn terminate(&mut self, child: &mut Child) {
        self.close();
        let _ = child.start_kill();
    }
    /// Call immediately after a successful `Child::wait` if no explicit
    /// termination was needed. Never signal a potentially reused Unix PID.
    /// A Windows job remains an unambiguous kernel handle and is closed here.
    pub fn reaped(&mut self) {
        #[cfg(unix)]
        {
            self.pid = None;
        }
        #[cfg(windows)]
        drop(self.job.take());
    }
    fn close(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.take() {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        drop(self.job.take());
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_os_user_and_platform_defaults() {
        assert!(!current_user().unwrap().is_empty());
        assert!(!default_auth_service().is_empty());
        assert_eq!(
            auth_method(),
            if cfg!(windows) {
                "windows_logon"
            } else {
                "pam"
            }
        );
        assert_eq!(
            executable("agent-instance"),
            if cfg!(windows) {
                "agent-instance.exe"
            } else {
                "agent-instance"
            }
        );
        assert_eq!(executable("agent-instance.exe"), "agent-instance.exe");
    }

    #[test]
    fn child_environment_is_explicit_and_preserves_os_paths() {
        let mut command = Command::new("unused");
        command.env("UNCONFIGURED_SECRET_FIXTURE", "not for child");
        child_environment(&mut command);
        let environment: Vec<_> = command.as_std().get_envs().collect();
        assert!(
            !environment
                .iter()
                .any(|(key, _)| *key == "UNCONFIGURED_SECRET_FIXTURE")
        );
        for name in ["PATH", "SYSTEMROOT", "USERPROFILE", "TEMP"] {
            if let Some(value) = std::env::var_os(name) {
                assert!(
                    environment
                        .iter()
                        .any(|(key, actual)| *key == name && *actual == Some(value.as_os_str()))
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_child_disarms_pid_cleanup() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("exit 0");
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let mut tree = ProcessTree::attach(&child).unwrap();
        assert!(child.wait().await.unwrap().success());
        tree.reaped();
        assert!(tree.pid.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_cleanup_terminates_descendants_before_reaping() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let child_pid_path = dir.path().join("descendant");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 60 & echo $! > \"$1\"; wait")
            .arg("fixture")
            .arg(&child_pid_path)
            .stdout(std::process::Stdio::piped());
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut tree = ProcessTree::attach(&child).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if std::fs::read_to_string(&child_pid_path)
                    .is_ok_and(|s| s.trim().parse::<i32>().is_ok())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let descendant: i32 = std::fs::read_to_string(child_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
        tree.terminate(&mut child);
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
        // A killed orphan may remain a zombie until the OS reaps it. A pipe probe
        // avoids mistaking that state for a running descendant in the assertion.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            output.read_to_end(&mut Vec::new()),
        )
        .await
        .unwrap()
        .unwrap();
        tree.terminate(&mut child); // Cleanup is idempotent after the child is reaped.
    }
}
