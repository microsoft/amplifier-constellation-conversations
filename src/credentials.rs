//! Installation-owned provider credentials. No response or configuration field
//! exposes the value; only already-authorized provider children receive it.
use anyhow::{Result, anyhow, ensure};
use std::{ffi::OsStr, fs, io::Read, path::Path};
use tokio::process::Command;
use zeroize::Zeroizing;

const OPENAI_FILE: &str = "openai-api-key";
const MAX_KEY_BYTES: usize = 8192;
const MAX_INPUT_BYTES: usize = MAX_KEY_BYTES + 2; // One optional CRLF.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Environment,
    Installation,
    Absent,
}

fn parse_key(bytes: &[u8]) -> Result<Zeroizing<String>> {
    let bytes = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= MAX_KEY_BYTES
            && bytes.iter().all(|byte| (0x21..=0x7e).contains(byte)),
        "Enter one nonempty API key without spaces or embedded line breaks (maximum 8 KiB)"
    );
    // The ASCII check also guarantees UTF-8. Never include credential bytes in errors.
    Ok(Zeroizing::new(
        String::from_utf8(bytes.to_vec()).map_err(|_| anyhow!("Invalid API key encoding"))?,
    ))
}

fn ordinary(metadata: &fs::Metadata, directory: bool) -> bool {
    let expected = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    expected && !metadata.file_type().is_symlink()
}

fn check_path(metadata: &fs::Metadata, directory: bool, require_private: bool) -> Result<()> {
    ensure!(
        ordinary(metadata, directory),
        "Credential storage must not be a link, reparse point or special file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "Credential storage must belong to this installation's OS user"
        );
        if require_private {
            ensure!(
                metadata.mode() & 0o077 == 0,
                "Credential storage must be private to this installation's OS user"
            );
        }
    }
    #[cfg(windows)]
    let _ = require_private; // Private import uses the shared protected owner/SYSTEM writer.
    Ok(())
}

fn read_bounded(reader: impl Read) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("Could not read credential input"))?;
    ensure!(
        bytes.len() <= MAX_INPUT_BYTES,
        "API key input exceeds 8 KiB"
    );
    Ok(bytes)
}

/// Import one key from stdin (or an equivalent local reader), never from an argv
/// value. The caller checks the installation configuration before invoking this.
/// Replacing this fixed file does not enable voice, alter configuration or test a provider.
pub fn import_openai(home: &Path, reader: impl Read) -> Result<()> {
    let bytes = read_bounded(reader)?;
    let key = parse_key(&bytes)?;
    ensure!(
        home.is_dir(),
        "Initialize this Conversations installation first"
    );
    let directory = home.join("secrets");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => check_path(&metadata, true, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err(anyhow!("Cannot inspect credential storage")),
    }
    crate::config::private_dir(&directory)?;
    let file = directory.join(OPENAI_FILE);
    match fs::symlink_metadata(&file) {
        Ok(metadata) => check_path(&metadata, false, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err(anyhow!("Cannot inspect credential file")),
    }
    crate::config::private_write(&file, key.as_bytes())
}

fn stored_openai(home: &Path) -> Result<Option<Zeroizing<String>>> {
    let directory = home.join("secrets");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => check_path(&metadata, true, true)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(anyhow!("Cannot inspect credential storage")),
    }
    let path = directory.join(OPENAI_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => check_path(&metadata, false, true)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(anyhow!("Cannot inspect credential file")),
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A replaced FIFO must not turn a credential read into an unbounded wait.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&path)
        .map_err(|_| anyhow!("Cannot open credential file"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("Cannot inspect opened credential file"))?;
    check_path(&metadata, false, true)?;
    ensure!(
        metadata.len() <= MAX_INPUT_BYTES as u64,
        "API key file exceeds 8 KiB"
    );
    // Bound the actual read too, in case a concurrent writer grows the file.
    let bytes = read_bounded(file)?;
    Ok(Some(parse_key(&bytes)?))
}

fn apply_with_environment(
    command: &mut Command,
    home: &Path,
    inherited: Option<&OsStr>,
) -> Result<CredentialSource> {
    if let Some(value) = inherited {
        // An explicitly set empty value still wins, preserving existing behavior.
        command.env("OPENAI_API_KEY", value);
        return Ok(CredentialSource::Environment);
    }
    if let Some(value) = stored_openai(home)? {
        command.env("OPENAI_API_KEY", value.as_str());
        return Ok(CredentialSource::Installation);
    }
    Ok(CredentialSource::Absent)
}

/// Call only at an existing OpenAI-key forwarding site, after env_clear().
/// ACP children and read-only observation/action helpers must not call this.
pub fn apply_openai_environment(command: &mut Command, home: &Path) -> Result<CredentialSource> {
    let value = std::env::var_os("OPENAI_API_KEY");
    apply_with_environment(command, home, value.as_deref())
}

/// Presence only: saving a key does not prove that a provider will accept it.
pub fn openai_status(home: &Path) -> Result<serde_json::Value> {
    if let Some(value) = std::env::var_os("OPENAI_API_KEY") {
        return Ok(
            serde_json::json!({"configured":!value.is_empty(),"source":"environment","environment_override":true,"provider_verified":false}),
        );
    }
    let configured = stored_openai(home)?.is_some();
    Ok(
        serde_json::json!({"configured":configured,"source":if configured {"installation"} else {"absent"},"environment_override":false,"provider_verified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn child_key(command: &Command) -> Option<std::ffi::OsString> {
        command.as_std().get_envs().find_map(|(name, value)| {
            (name == "OPENAI_API_KEY")
                .then(|| value.map(OsStr::to_owned))
                .flatten()
        })
    }

    fn provider_child() -> Command {
        let mut child = Command::new("never-spawned");
        crate::platform::child_environment(&mut child);
        child
    }

    #[test]
    fn absent_key_does_not_create_files_or_add_a_child_secret() {
        let home = tempfile::tempdir().unwrap();
        let mut child = provider_child();
        assert_eq!(
            apply_with_environment(&mut child, home.path(), None).unwrap(),
            CredentialSource::Absent
        );
        assert!(child_key(&child).is_none());
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    }

    #[test]
    fn import_is_fixed_private_and_does_not_enable_voice_or_edit_configuration() {
        let home = tempfile::tempdir().unwrap();
        let config = b"existing configuration kept byte-for-byte";
        let voice = b"{\"enabled\":false}";
        fs::write(home.path().join("config.json"), config).unwrap();
        fs::write(home.path().join("companion.json"), voice).unwrap();
        for newline in ["", "\n", "\r\n"] {
            import_openai(
                home.path(),
                Cursor::new(format!("fixture-openai-key{newline}")),
            )
            .unwrap();
            let mut child = provider_child();
            assert_eq!(
                apply_with_environment(&mut child, home.path(), None).unwrap(),
                CredentialSource::Installation
            );
            assert_eq!(child_key(&child).unwrap(), "fixture-openai-key");
        }
        assert_eq!(fs::read(home.path().join("config.json")).unwrap(), config);
        assert_eq!(fs::read(home.path().join("companion.json")).unwrap(), voice);
        assert_eq!(
            fs::read_dir(home.path().join("secrets")).unwrap().count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(home.path().join("secrets"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(home.path().join("secrets").join(OPENAI_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn explicit_environment_including_empty_wins_without_reading_stored_file() {
        let home = tempfile::tempdir().unwrap();
        import_openai(home.path(), Cursor::new(b"file-fixture")).unwrap();
        fs::write(
            home.path().join("secrets").join(OPENAI_FILE),
            b"malformed saved secret\n",
        )
        .unwrap();
        for value in ["environment-fixture", ""] {
            let mut child = provider_child();
            assert_eq!(
                apply_with_environment(&mut child, home.path(), Some(OsStr::new(value))).unwrap(),
                CredentialSource::Environment
            );
            assert_eq!(child_key(&child).unwrap(), value);
        }
    }

    #[test]
    fn malformed_whitespace_and_oversized_inputs_leave_existing_key_unchanged() {
        let home = tempfile::tempdir().unwrap();
        import_openai(home.path(), Cursor::new(b"original-fixture")).unwrap();
        for invalid in [
            "",
            " ",
            " leading",
            "trailing ",
            "two\nlines",
            "tab\tkey",
            "key\n\n",
            "key\0tail",
            "non-ascii-é",
            "fixture-sensitive-secret with-space",
        ] {
            let error = import_openai(home.path(), Cursor::new(invalid))
                .unwrap_err()
                .to_string();
            assert!(!error.contains("original-fixture"));
            assert!(!error.contains("fixture-sensitive-secret"));
            assert_eq!(
                fs::read(home.path().join("secrets").join(OPENAI_FILE)).unwrap(),
                b"original-fixture"
            );
        }
        let mut input = Cursor::new(vec![b'a'; MAX_INPUT_BYTES + 1000]);
        assert!(import_openai(home.path(), &mut input).is_err());
        assert_eq!(input.position(), (MAX_INPUT_BYTES + 1) as u64);
        assert_eq!(
            fs::read(home.path().join("secrets").join(OPENAI_FILE)).unwrap(),
            b"original-fixture"
        );
    }

    #[test]
    fn malformed_or_oversized_stored_key_fails_without_populating_child_environment() {
        let home = tempfile::tempdir().unwrap();
        import_openai(home.path(), Cursor::new(b"fixture")).unwrap();
        for bytes in [
            b"malformed saved key".to_vec(),
            vec![b'x'; MAX_INPUT_BYTES + 1],
        ] {
            crate::config::private_write(&home.path().join("secrets").join(OPENAI_FILE), &bytes)
                .unwrap();
            let mut child = provider_child();
            assert!(apply_with_environment(&mut child, home.path(), None).is_err());
            assert!(child_key(&child).is_none());
        }
    }

    #[test]
    fn special_file_or_directory_cannot_be_a_credential() {
        let home = tempfile::tempdir().unwrap();
        crate::config::private_dir(&home.path().join("secrets")).unwrap();
        fs::create_dir(home.path().join("secrets").join(OPENAI_FILE)).unwrap();
        let mut child = provider_child();
        assert!(apply_with_environment(&mut child, home.path(), None).is_err());
        assert!(import_openai(home.path(), Cursor::new(b"fixture")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_files_and_parent_directories_are_refused_without_changing_the_target() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let outside = external.path().join("untouched");
        fs::write(&outside, b"external-fixture").unwrap();
        crate::config::private_dir(&home.path().join("secrets")).unwrap();
        let path = home.path().join("secrets").join(OPENAI_FILE);
        symlink(&outside, &path).unwrap();
        let mut child = provider_child();
        assert!(apply_with_environment(&mut child, home.path(), None).is_err());
        assert!(import_openai(home.path(), Cursor::new(b"fixture")).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(home.path().join("secrets")).unwrap();
        symlink(external.path(), home.path().join("secrets")).unwrap();
        assert!(apply_with_environment(&mut child, home.path(), None).is_err());
        assert!(import_openai(home.path(), Cursor::new(b"fixture")).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"external-fixture");
    }

    #[cfg(unix)]
    #[test]
    fn insecure_stored_file_is_refused_and_import_does_not_restrict_the_home_folder() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o755)).unwrap();
        import_openai(home.path(), Cursor::new(b"fixture")).unwrap();
        assert_eq!(
            fs::metadata(home.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let path = home.path().join("secrets").join(OPENAI_FILE);
        fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(apply_with_environment(&mut provider_child(), home.path(), None).is_err());
    }
}
