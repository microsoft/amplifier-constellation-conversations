//! Fixed persistent-service launch for the installation updater. Windows uses
//! an explicit handle allowlist: null stdio alone does not exclude inherited
//! capture writers belonging to the short-lived updater's caller.
use std::{io, path::Path};

#[cfg(not(windows))]
pub(crate) use std::process::Child as ServiceChild;
#[cfg(windows)]
pub(crate) use windows::ServiceChild;

pub(crate) fn spawn_service(
    executable: &Path,
    home: &Path,
    transaction: &str,
    nonce: &str,
) -> io::Result<ServiceChild> {
    #[cfg(not(windows))]
    {
        use std::process::{Command, Stdio};
        Command::new(executable)
            .arg("--home")
            .arg(home)
            .arg("serve-update")
            .arg(transaction)
            .arg(nonce)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        windows::spawn(
            executable,
            &[
                OsStr::new("--home"),
                home.as_os_str(),
                OsStr::new("serve-update"),
                OsStr::new(transaction),
                OsStr::new(nonce),
            ],
        )
    }
}

// Keep UTF-16 units intact, including unpaired surrogates in Windows paths.
// argv[0] follows Windows' special program-name parsing; subsequent arguments
// follow CRT/Rust backslash-before-quote parsing. No shell is involved.
#[cfg(any(windows, test))]
fn command_line(program: &[u16], args: &[Vec<u16>]) -> io::Result<Vec<u16>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid service command line");
    if program.is_empty() || program.contains(&0) || program.contains(&34) {
        return Err(invalid());
    }
    let mut out = vec![34];
    out.extend_from_slice(program);
    out.push(34);
    for arg in args {
        if arg.contains(&0) {
            return Err(invalid());
        }
        out.extend_from_slice(&[32, 34]);
        let mut slashes = 0;
        for &unit in arg {
            if unit == 92 {
                slashes += 1;
            } else {
                let count = if unit == 34 { slashes * 2 + 1 } else { slashes };
                out.extend(std::iter::repeat_n(92, count));
                out.push(unit);
                slashes = 0;
            }
        }
        out.extend(std::iter::repeat_n(92, slashes * 2));
        out.push(34);
    }
    out.push(0);
    if out.len() > 32_767 {
        return Err(invalid());
    }
    Ok(out)
}

#[cfg(windows)]
mod windows {
    use super::command_line;
    use std::{
        ffi::{OsStr, c_void},
        io,
        mem::{size_of, size_of_val},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::ExitStatusExt,
        },
        path::Path,
        process::ExitStatus,
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{
            ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
            INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
            STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    };

    struct Attributes(Vec<usize>);
    impl Attributes {
        fn new(handles: &[HANDLE]) -> io::Result<Self> {
            let mut bytes = 0;
            let queried =
                unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes) };
            if queried != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || bytes == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
            if unsafe {
                InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let attributes = Self(storage);
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.as_ptr(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    size_of_val(handles),
                    ptr::null_mut(),
                    ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(attributes)
        }
        fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.0.as_ptr().cast::<c_void>().cast_mut()
        }
    }
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
        }
    }

    fn null_handle(read: bool) -> io::Result<OwnedHandle> {
        let name: Vec<u16> = OsStr::new(r"\\.\NUL")
            .encode_wide()
            .chain(Some(0))
            .collect();
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 1,
        };
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                if read { GENERIC_READ } else { GENERIC_WRITE },
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &security,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// Only the fixed service launcher calls this outside this module's tests.
    pub(super) fn spawn(executable: &Path, args: &[&OsStr]) -> io::Result<ServiceChild> {
        if !executable.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use an absolute service executable",
            ));
        }
        let program: Vec<u16> = executable.as_os_str().encode_wide().collect();
        let arguments: Vec<Vec<u16>> = args.iter().map(|arg| arg.encode_wide().collect()).collect();
        let mut line = command_line(&program, &arguments)?;
        let mut program = program;
        program.push(0);
        let input = null_handle(true)?;
        let output = null_handle(false)?;
        let error = null_handle(false)?;
        let handles = [
            input.as_raw_handle(),
            output.as_raw_handle(),
            error.as_raw_handle(),
        ];
        // Both the attribute list and referenced handles live through CreateProcessW.
        let attributes = Attributes::new(&handles)?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attributes.as_ptr();
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                program.as_ptr(),
                line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NEW_PROCESS_GROUP
                    | DETACHED_PROCESS
                    | EXTENDED_STARTUPINFO_PRESENT,
                ptr::null(),
                ptr::null(),
                &startup.StartupInfo,
                &mut process,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
        // The returned process/thread handles are non-inheritable. The detached
        // service outlives this owner, so dropping its handle must not kill it.
        drop(unsafe { OwnedHandle::from_raw_handle(process.hThread) });
        Ok(ServiceChild {
            handle,
            pid: process.dwProcessId,
            exit: None,
        })
    }

    pub(crate) struct ServiceChild {
        handle: OwnedHandle,
        pid: u32,
        exit: Option<ExitStatus>,
    }
    impl ServiceChild {
        pub(crate) fn id(&self) -> u32 {
            self.pid
        }
        fn observe(&mut self, timeout: u32) -> io::Result<Option<ExitStatus>> {
            if self.exit.is_some() {
                return Ok(self.exit);
            }
            match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), timeout) } {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    self.exit = Some(ExitStatus::from_raw(code));
                    Ok(self.exit)
                }
                _ => Err(io::Error::last_os_error()),
            }
        }
        pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.observe(0)
        }
        pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
            self.observe(INFINITE)?
                .ok_or_else(|| io::Error::other("Service wait did not settle"))
        }
        pub(crate) fn kill(&mut self) -> io::Result<()> {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            if unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) } == 0 {
                let error = io::Error::last_os_error();
                if self.try_wait()?.is_none() {
                    return Err(error);
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        const FIXTURE: &str = "native_update_process::windows::tests::service_fixture";
        const MARKER: &str = "native-service-fixture:";

        #[test]
        #[ignore = "bounded subprocess for native handle allowlist qualification"]
        fn service_fixture() {
            let Some(path) = std::env::args_os().find_map(|arg| {
                arg.to_str()
                    .and_then(|s| s.strip_prefix(MARKER))
                    .map(str::to_owned)
            }) else {
                return;
            };
            std::fs::write(path, b"ready").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(8));
        }

        #[test]
        fn persistent_service_excludes_other_inheritable_handles() {
            let root = tempfile::tempdir().unwrap();
            let witness = root.path().join("unrelated-handle.txt");
            std::fs::write(&witness, b"not inherited").unwrap();
            let path: Vec<u16> = witness.as_os_str().encode_wide().chain(Some(0)).collect();
            let security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: ptr::null_mut(),
                bInheritHandle: 1,
            };
            // Exclusive sharing makes an inherited copy observable without PID
            // enumeration: it would keep this file unavailable while child runs.
            let raw = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    0,
                    &security,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    ptr::null_mut(),
                )
            };
            assert_ne!(raw, INVALID_HANDLE_VALUE);
            let unrelated = unsafe { OwnedHandle::from_raw_handle(raw) };
            let ready = root.path().join("ready 日本語 café.txt");
            let marker = format!("{MARKER}{}", ready.display());
            let mut child = spawn(
                &std::env::current_exe().unwrap(),
                &[
                    OsStr::new("--ignored"),
                    OsStr::new("--exact"),
                    OsStr::new(FIXTURE),
                    OsStr::new("--nocapture"),
                    OsStr::new("--skip"),
                    OsStr::new(&marker),
                ],
            )
            .unwrap();
            drop(unrelated);
            let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !ready.exists()
                && child.try_wait().unwrap().is_none()
                && std::time::Instant::now() < end
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let alive = child.try_wait().unwrap().is_none();
            let opened = std::fs::read(&witness);
            // Reap this exact child even when an assertion below will fail.
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(
                ready.exists() && alive,
                "Witness must run while handle exclusion is checked"
            );
            assert_eq!(opened.unwrap(), b"not inherited");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::command_line;
    fn line(args: &[&str]) -> String {
        let args: Vec<Vec<u16>> = args.iter().map(|s| s.encode_utf16().collect()).collect();
        let line =
            command_line(&"C:\\service.exe".encode_utf16().collect::<Vec<_>>(), &args).unwrap();
        assert_eq!(line.last(), Some(&0));
        String::from_utf16(&line[..line.len() - 1]).unwrap()
    }
    #[test]
    fn quotes_empty_unicode_whitespace_and_trailing_slashes() {
        assert_eq!(
            line(&["", "日本語 café", r"C:\home path\"]),
            "\"C:\\service.exe\" \"\" \"日本語 café\" \"C:\\home path\\\\\""
        );
    }
    #[test]
    fn escapes_quotes_and_only_backslashes_preceding_quotes() {
        assert_eq!(
            line(&[r#"a\"b"#, r"a\b"]),
            r#""C:\service.exe" "a\\\"b" "a\b""#
        );
    }
    #[test]
    fn retains_utf16_units_and_rejects_invalid_or_oversized_input() {
        let program: Vec<u16> = "C:\\service.exe".encode_utf16().collect();
        assert!(
            command_line(&program, &[vec![0xd800]])
                .unwrap()
                .contains(&0xd800)
        );
        assert!(command_line(&[0], &[]).is_err());
        assert!(command_line(&[34], &[]).is_err());
        assert!(command_line(&program, &[vec![0]]).is_err());
        assert!(command_line(&program, &[vec![65; 32_767]]).is_err());
    }
}
