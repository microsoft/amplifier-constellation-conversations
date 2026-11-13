//! Installation of an already acquired, immutable native bundle. This does not
//! enroll updates, change an agent, replace configuration, or start a service.
use crate::config;
use crate::native_installation_diagnostics::{Phase, observe};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    thread,
};

pub const APPLICATION: amplifier_native_platform::Application =
    amplifier_native_platform::Application {
        display_name: "Companion",
        startup_description: "Constellation Conversations and Companion",
        executable_name: "agent-conversations",
        startup_prefix: "agent-conversations",
        data_directory: "agent-conversations",
        background_command: "serve",
    };

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    component: String,
    version: String,
    source_revision: String,
    target: String,
    entrypoints: Entrypoints,
    files: Vec<File>,
}
#[derive(Deserialize)]
struct Entrypoints {
    service: String,
    node: String,
    bridge: String,
    mcp: String,
}
#[derive(Deserialize)]
struct File {
    path: String,
    sha256: String,
    bytes: u64,
    executable: bool,
}

pub fn target() -> Result<String> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        _ => anyhow::bail!("Unsupported native operating system"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => anyhow::bail!("Unsupported native architecture"),
    };
    Ok(format!("{os}-{arch}"))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn digest(path: &Path) -> Result<String> {
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex(&hash.finalize()))
}

const MAX_VERIFICATION_WORKERS: usize = 4;

fn verify_files(root: &Path, files: &[File]) -> Result<()> {
    let parallelism = thread::available_parallelism().map_or(1, usize::from);
    verify_files_with(files, parallelism, |file| verify_file(root, file))
}

fn verify_file(root: &Path, file: &File) -> Result<()> {
    let path = root.join(&file.path);
    ensure!(
        fs::metadata(&path)?.len() == file.bytes && digest(&path)? == file.sha256,
        "Bundle file failed verification: {}",
        file.path
    );
    Ok(())
}

fn verify_files_with(
    files: &[File],
    parallelism: usize,
    verify: impl Fn(&File) -> Result<()> + Sync,
) -> Result<()> {
    let workers = parallelism
        .clamp(1, MAX_VERIFICATION_WORKERS)
        .min(files.len());
    if workers <= 1 {
        return files.iter().try_for_each(verify);
    }
    // Only the independent content checks run concurrently. Manifest/path and
    // complete inventory validation precede this call; entrypoint validation
    // follows it. All started workers are joined, including on an error, so no
    // verification can outlive its caller or its installation locks.
    thread::scope(|scope| {
        let mut jobs = Vec::with_capacity(workers);
        let mut spawn_error = None;
        for chunk in files.chunks(files.len().div_ceil(workers)) {
            let verify = &verify;
            match thread::Builder::new()
                .spawn_scoped(scope, move || chunk.iter().try_for_each(verify))
            {
                Ok(job) => jobs.push(job),
                Err(error) => {
                    spawn_error = Some(error);
                    break;
                }
            }
        }
        // Collect every join before selecting the first failure in manifest
        // order. Short-circuiting the joins could leave work running on error.
        let results: Vec<_> = jobs.into_iter().map(|job| job.join()).collect();
        if let Some(error) = spawn_error {
            return Err(error).context("Cannot start bundle verification worker");
        }
        for result in results {
            result.map_err(|_| anyhow::anyhow!("Bundle verification worker failed"))??;
        }
        Ok(())
    })
}
fn relative(value: &str) -> Result<PathBuf> {
    ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.contains(['\\', ':', '\0']),
        "Invalid bundle path"
    );
    ensure!(
        value.split('/').all(|s| !s.is_empty()
            && s != "."
            && s != ".."
            && !s.ends_with(['.', ' '])
            && !s.chars().any(char::is_control)),
        "Invalid bundle path"
    );
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9')),
            "Reserved bundle path"
        );
    }
    let path = PathBuf::from(value);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|v| matches!(v, std::path::Component::Normal(_))),
        "Bundle path escapes its directory"
    );
    Ok(path)
}
fn ordinary(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    !metadata.file_type().is_symlink()
}
fn native_executable(path: &Path) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header)?;
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "linux") {
        ensure!(
            &header[..4] == b"\x7fELF"
                && header[4] == 2
                && header[5] == 1
                && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
                && u16::from_le_bytes([header[18], header[19]]) == if arm { 183 } else { 62 },
            "Executable is not native 64-bit ELF"
        );
        // ET_DYN includes ordinary shared libraries as well as PIE executables.
        // A real entrypoint must lie in file-backed executable loadable bytes.
        let entry = u64::from_le_bytes(header[24..32].try_into()?);
        let offset = u64::from_le_bytes(header[32..40].try_into()?);
        let stride = u16::from_le_bytes(header[54..56].try_into()?);
        let count = u16::from_le_bytes(header[56..58].try_into()?);
        let length = file.metadata()?.len();
        ensure!(
            entry != 0
                && offset >= 64
                && stride == 56
                && (1..=4096).contains(&count)
                && offset
                    .checked_add(u64::from(stride) * u64::from(count))
                    .is_some_and(|end| end <= length),
            "ELF executable has no valid entrypoint or program headers"
        );
        let mut entry_is_executable = false;
        file.seek(SeekFrom::Start(offset))?;
        for _ in 0..count {
            let mut program = [0u8; 56];
            file.read_exact(&mut program)?;
            let kind = u32::from_le_bytes(program[..4].try_into()?);
            let flags = u32::from_le_bytes(program[4..8].try_into()?);
            let file_start = u64::from_le_bytes(program[8..16].try_into()?);
            let virtual_start = u64::from_le_bytes(program[16..24].try_into()?);
            let file_size = u64::from_le_bytes(program[32..40].try_into()?);
            let memory_size = u64::from_le_bytes(program[40..48].try_into()?);
            if kind == 1
                && flags & 1 != 0
                && file_size <= memory_size
                && file_start
                    .checked_add(file_size)
                    .is_some_and(|end| end <= length)
                && entry
                    .checked_sub(virtual_start)
                    .is_some_and(|at| at < file_size)
            {
                entry_is_executable = true;
            }
        }
        ensure!(
            entry_is_executable,
            "ELF entrypoint is not executable file-backed code"
        );
    } else if cfg!(target_os = "macos") {
        ensure!(
            &header[..4] == b"\xcf\xfa\xed\xfe"
                && u32::from_le_bytes(header[4..8].try_into()?)
                    == if arm { 0x0100000c } else { 0x01000007 }
                && u32::from_le_bytes(header[12..16].try_into()?) == 2,
            "Executable is not native 64-bit Mach-O"
        );
    } else if cfg!(windows) {
        ensure!(&header[..2] == b"MZ", "Executable is not Windows PE");
        let offset = u32::from_le_bytes(header[60..64].try_into()?);
        ensure!(offset <= 1024 * 1024, "Invalid PE header offset");
        file.seek(SeekFrom::Start(u64::from(offset)))?;
        let mut pe = [0u8; 26];
        file.read_exact(&mut pe)?;
        let flags = u16::from_le_bytes([pe[22], pe[23]]);
        ensure!(
            &pe[..4] == b"PE\0\0"
                && u16::from_le_bytes([pe[4], pe[5]]) == if arm { 0xaa64 } else { 0x8664 }
                && u16::from_le_bytes([pe[24], pe[25]]) == 0x20b
                && flags & 2 != 0
                && flags & 0x2000 == 0,
            "Executable is not native 64-bit Windows PE"
        );
    }
    Ok(())
}
fn enumerate(root: &Path, at: &Path, files: &mut BTreeSet<String>) -> Result<()> {
    ensure!(files.len() <= 100_000, "Bundle contains too many files");
    for entry in fs::read_dir(at)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let kind = metadata.file_type();
        ensure!(
            ordinary(&metadata),
            "Bundle links/reparse points are not supported"
        );
        if kind.is_dir() {
            enumerate(root, &path, files)?;
        } else {
            ensure!(kind.is_file(), "Bundle contains a special file");
            let rel = path
                .strip_prefix(root)?
                .to_str()
                .context("Bundle path is not Unicode")?
                .replace('\\', "/");
            relative(&rel)?;
            ensure!(files.insert(rel), "Duplicate bundle path");
        }
    }
    Ok(())
}
#[cfg(test)]
thread_local! {
    static VERIFICATION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn count_verifications<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    let before = VERIFICATION_COUNT.get();
    let result = operation();
    (result, VERIFICATION_COUNT.get() - before)
}

fn load(root: &Path) -> Result<(Manifest, Vec<u8>, String)> {
    #[cfg(test)]
    VERIFICATION_COUNT.set(VERIFICATION_COUNT.get() + 1);
    let root_meta = fs::symlink_metadata(root)?;
    ensure!(
        ordinary(&root_meta) && root_meta.file_type().is_dir(),
        "Use a real bundle directory"
    );
    let path = root.join("manifest.json");
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        ordinary(&meta) && meta.file_type().is_file() && meta.len() <= 16 * 1024 * 1024,
        "Invalid bundle manifest"
    );
    let bytes = fs::read(path)?;
    let m: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        m.schema_version == 1 && m.component == "conversations",
        "Unsupported bundle manifest"
    );
    ensure!(
        m.target == target()?,
        "This package is for {}, not this computer",
        m.target
    );
    ensure!(
        !m.version.is_empty()
            && m.source_revision.len() == 40
            && m.source_revision.bytes().all(|v| v.is_ascii_hexdigit()),
        "Missing package provenance"
    );
    ensure!(
        !m.files.is_empty() && m.files.len() <= 100_000,
        "Invalid bundle file list"
    );
    let mut expected = BTreeSet::from(["manifest.json".to_owned()]);
    let mut folded = BTreeSet::from(["manifest.json".to_owned()]);
    for file in &m.files {
        relative(&file.path)?;
        ensure!(
            file.sha256.len() == 64
                && file.sha256.bytes().all(|v| v.is_ascii_hexdigit())
                && expected.insert(file.path.clone()),
            "Invalid or duplicate bundle file"
        );
        ensure!(
            folded.insert(file.path.to_lowercase()),
            "Case-colliding bundle file"
        );
    }
    let mut actual = BTreeSet::new();
    enumerate(root, root, &mut actual)?;
    ensure!(actual == expected, "Bundle has missing or unexpected files");
    verify_files(root, &m.files)?;
    for path in [
        &m.entrypoints.service,
        &m.entrypoints.node,
        &m.entrypoints.bridge,
        &m.entrypoints.mcp,
    ] {
        relative(path)?;
        ensure!(
            m.files.iter().any(|v| &v.path == path),
            "Missing bundle entrypoint"
        );
    }
    let windows = cfg!(windows);
    ensure!(
        m.entrypoints.service
            == if windows {
                "agent-conversations.exe"
            } else {
                "agent-conversations"
            },
        "Unexpected service entrypoint"
    );
    ensure!(
        m.entrypoints.node
            == if windows {
                "runtime/node.exe"
            } else {
                "runtime/node"
            },
        "Unexpected Node entrypoint"
    );
    ensure!(
        m.entrypoints.bridge == "bridge/live.mjs"
            && m.entrypoints.mcp == "bridge/companion-mcp.mjs",
        "Unexpected bridge entrypoint"
    );
    for path in [&m.entrypoints.service, &m.entrypoints.node] {
        ensure!(
            m.files.iter().any(|v| &v.path == path && v.executable),
            "Entrypoint is not executable"
        );
        native_executable(&root.join(path))?;
    }
    let hash = hex(&Sha256::digest(&bytes));
    Ok((m, bytes, hash))
}
fn result(root: &Path, m: &Manifest, hash: &str) -> Value {
    json!({"bundle":root,"manifest_sha256":hash,"target":m.target,"version":m.version,"source_revision":m.source_revision,"entrypoints":{"service":root.join(&m.entrypoints.service),"node":root.join(&m.entrypoints.node),"bridge":root.join(&m.entrypoints.bridge),"mcp":root.join(&m.entrypoints.mcp)},"note":"Bundle bytes verified. No service started, agent changed, or configuration replaced. Integrity is not publisher signature verification."})
}
pub fn verify(root: &Path) -> Result<Value> {
    let (m, _, hash) = load(root)?;
    // Expose a canonical path usable by the bundled Node module loader too.
    // On Windows std canonicalization always adds the verbatim prefix; dunce
    // removes it only when the ordinary spelling names the same filesystem path.
    Ok(result(&dunce::canonicalize(root)?, &m, &hash))
}

const MAX_COPY_WORKERS: usize = 4;

fn copy_file(root: &Path, staged: &Path, file: &File) -> Result<()> {
    let dest = staged.join(&file.path);
    config::private_dir(dest.parent().context("Invalid package parent")?)?;
    // Keep this protection for every file, including files sharing a parent.
    fs::copy(root.join(&file.path), &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            &dest,
            fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }),
        )?;
    }
    Ok(())
}

fn copy_files(root: &Path, staged: &Path, files: &[File]) -> Result<()> {
    let parallelism = thread::available_parallelism().map_or(1, usize::from);
    copy_files_with(
        files,
        parallelism,
        |file| copy_file(root, staged, file),
        |_| Ok(()),
    )
}

fn copy_files_with(
    files: &[File],
    parallelism: usize,
    copy: impl Fn(&File) -> Result<()> + Sync,
    // A private seam for deterministic spawn-refusal tests. Production has no
    // alternate launch, retry or scheduling policy.
    before_spawn: impl Fn(usize) -> std::io::Result<()>,
) -> Result<()> {
    let workers = parallelism.clamp(1, MAX_COPY_WORKERS).min(files.len());
    if workers <= 1 {
        return files.iter().try_for_each(copy);
    }
    // The complete source inventory and paths have already been validated.
    // Only distinct file destinations run concurrently; per-file parent
    // protection is unchanged even when workers share a directory.
    thread::scope(|scope| {
        let mut jobs = Vec::with_capacity(workers);
        let mut spawn_error = None;
        for (index, chunk) in files.chunks(files.len().div_ceil(workers)).enumerate() {
            let copy = &copy;
            match before_spawn(index).and_then(|()| {
                thread::Builder::new().spawn_scoped(scope, move || chunk.iter().try_for_each(copy))
            }) {
                Ok(job) => jobs.push(job),
                Err(error) => {
                    spawn_error = Some(error);
                    break;
                }
            }
        }
        // Never short-circuit joins: no worker may outlive manifest writing,
        // copied verification, publication, failed-stage cleanup or its lock.
        let results: Vec<_> = jobs.into_iter().map(|job| job.join()).collect();
        if let Some(error) = spawn_error {
            return Err(error).context("Cannot start bundle copy worker");
        }
        for result in results {
            result.map_err(|_| anyhow::anyhow!("Bundle copy worker failed"))??;
        }
        Ok(())
    })
}

pub fn install(root: &Path, home: &Path) -> Result<Value> {
    // Verify before creating installation state; extraction/download belongs to
    // the package acquisition surface, not to a remotely callable host API.
    let (m, bytes, hash) = observe(Phase::BundleVerify, || load(root))?;
    config::private_dir(home)?;
    let home = dunce::canonicalize(home)?;
    let packages = home.join("packages");
    config::private_dir(&packages)?;
    let destination = packages.join(&hash);
    if destination.exists() {
        let (_, existing, _) = observe(Phase::BundleVerifyExisting, || load(&destination))?;
        ensure!(existing == bytes, "Existing immutable package differs");
        return Ok(result(&destination, &m, &hash));
    }
    let staged = packages.join(format!(".install-{}", uuid::Uuid::new_v4()));
    config::private_dir(&staged)?;
    let attempt = (|| -> Result<()> {
        observe(Phase::BundleCopy, || {
            copy_files(root, &staged, &m.files)?;
            config::private_write(&staged.join("manifest.json"), &bytes)?;
            Ok(())
        })?;
        observe(Phase::BundleVerifyCopied, || load(&staged))?;
        observe(Phase::BundlePublish, || {
            amplifier_native_platform::publish_directory_if_absent(&staged, &destination)
        })?;
        Ok(())
    })();
    if attempt.is_err() && staged.exists() {
        let _ = fs::remove_dir_all(&staged);
    }
    attempt?;
    Ok(result(&destination, &m, &hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn directory_policy(path: &Path) -> Vec<u8> {
        use std::ptr;
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetFileSecurityW, OWNER_SECURITY_INFORMATION,
        };

        let path = amplifier_native_platform::windows_native_path(path).unwrap();
        let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let mut needed = 0;
        unsafe { GetFileSecurityW(path.as_ptr(), flags, ptr::null_mut(), 0, &mut needed) };
        assert!(needed > 0 && needed <= 65536);
        let mut storage = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        assert_ne!(
            unsafe {
                GetFileSecurityW(
                    path.as_ptr(),
                    flags,
                    storage.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                )
            },
            0
        );
        unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), needed as usize) }
            .to_vec()
    }

    fn fixture(root: &Path) {
        let service = if cfg!(windows) {
            "agent-conversations.exe"
        } else {
            "agent-conversations"
        };
        let node = if cfg!(windows) {
            "runtime/node.exe"
        } else {
            "runtime/node"
        };
        let paths = [service, node, "bridge/live.mjs", "bridge/companion-mcp.mjs"];
        let files: Vec<Value> = paths.iter().map(|path| {
            let p = root.join(path); fs::create_dir_all(p.parent().unwrap()).unwrap();
            let executable = *path == service || *path == node;
            let mut content = vec![0u8; 128];
            if executable {
                let arm = cfg!(target_arch="aarch64");
                if cfg!(target_os="linux") {
                    content[..6].copy_from_slice(b"\x7fELF\x02\x01"); content[16]=2; content[18..20].copy_from_slice(&(if arm {183u16} else {62u16}).to_le_bytes());
                    content[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
                    content[32..40].copy_from_slice(&64u64.to_le_bytes());
                    content[54..56].copy_from_slice(&56u16.to_le_bytes()); content[56]=1;
                    content[64]=1; content[68]=5;
                    content[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
                    content[96..104].copy_from_slice(&128u64.to_le_bytes());
                    content[104..112].copy_from_slice(&128u64.to_le_bytes());
                }
                else if cfg!(target_os="macos") { content[..4].copy_from_slice(b"\xcf\xfa\xed\xfe"); content[4..8].copy_from_slice(&(if arm {0x0100000cu32} else {0x01000007u32}).to_le_bytes()); content[12]=2; }
                else if cfg!(windows) { content[..2].copy_from_slice(b"MZ"); content[60]=64; content[64..68].copy_from_slice(b"PE\0\0"); content[68..70].copy_from_slice(&(if arm {0xaa64u16} else {0x8664u16}).to_le_bytes()); content[86]=2; content[88..90].copy_from_slice(&0x20bu16.to_le_bytes()); }
            }
            fs::write(&p, &content).unwrap();
            json!({"path":path,"sha256":digest(&p).unwrap(),"bytes":content.len(),"executable":executable})
        }).collect();
        fs::write(root.join("manifest.json"), serde_json::to_vec(&json!({"schema_version":1,"component":"conversations","version":"fixture","source_revision":"a".repeat(40),"target":target().unwrap(),"entrypoints":{"service":service,"node":node,"bridge":paths[2],"mcp":paths[3]},"files":files})).unwrap()).unwrap();
    }
    #[test]
    fn install_relocates_and_preserves_existing_state() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source with spaces");
        fs::create_dir(&source).unwrap();
        fixture(&source);
        let home = tmp.path().join("personal — home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("config.json"),
            b"existing credentials and sessions",
        )
        .unwrap();
        let receipt = install(&source, &home).unwrap();
        fs::remove_dir_all(&source).unwrap();
        verify(Path::new(receipt["bundle"].as_str().unwrap())).unwrap();
        assert_eq!(
            fs::read(home.join("config.json")).unwrap(),
            b"existing credentials and sessions"
        );
        assert!(!home.join("runtime.json").exists());
    }
    #[test]
    #[cfg(windows)]
    fn public_entrypoints_keep_identity_with_node_compatible_windows_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source café");
        fs::create_dir(&source).unwrap();
        fixture(&source);
        let home = tmp.path().join("Companion 日本語");
        let installed = install(&source, &home).unwrap();
        for receipt in [verify(&source).unwrap(), installed.clone()] {
            let root = Path::new(receipt["bundle"].as_str().unwrap());
            assert!(!root.as_os_str().to_string_lossy().starts_with(r"\\?\"));
            for value in receipt["entrypoints"].as_object().unwrap().values() {
                let path = Path::new(value.as_str().unwrap());
                assert!(!path.as_os_str().to_string_lossy().starts_with(r"\\?\"));
                assert!(
                    fs::canonicalize(path)
                        .unwrap()
                        .starts_with(fs::canonicalize(root).unwrap())
                );
                assert!(path.is_file());
            }
        }
        assert_eq!(
            verify(Path::new(installed["bundle"].as_str().unwrap())).unwrap()["entrypoints"],
            installed["entrypoints"]
        );
    }
    #[test]
    fn rejects_tampering_extras_and_wrong_architecture_before_install() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        fs::write(tmp.path().join("extra"), b"unexpected").unwrap();
        assert!(verify(tmp.path()).is_err());
        fs::remove_file(tmp.path().join("extra")).unwrap();
        fs::write(tmp.path().join("bridge/live.mjs"), b"tampered").unwrap();
        assert!(verify(tmp.path()).is_err());
        fixture(tmp.path());
        let path = tmp.path().join("manifest.json");
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        v["target"] = json!("wrong-platform");
        fs::write(path, v.to_string()).unwrap();
        assert!(verify(tmp.path()).is_err());
    }

    #[test]
    fn copying_shared_and_nested_parents_matches_sequential_bytes_and_modes() {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source 日本語");
        fs::create_dir(&source).unwrap();
        fixture(&source);
        let manifest_path = source.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        for index in 0..48 {
            let parent = if index % 2 == 0 {
                "shared"
            } else {
                "shared/nested/deep"
            };
            let name = format!("bridge/{parent}/file-{index}.js");
            let contents = format!("dependency-{index}").into_bytes();
            let path = source.join(&name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &contents).unwrap();
            manifest["files"].as_array_mut().unwrap().push(json!({
                "path": name, "sha256": hex(&Sha256::digest(&contents)),
                "bytes": contents.len(), "executable": index % 3 == 0,
            }));
        }
        fs::write(&manifest_path, manifest.to_string()).unwrap();
        let (manifest, bytes, _) = load(&source).unwrap();
        let sequential = temp.path().join("old sequential");
        let parallel = temp.path().join("new parallel");
        config::private_dir(&sequential).unwrap();
        config::private_dir(&parallel).unwrap();
        // Exact former copy body, retained as an independent sequential oracle.
        for file in &manifest.files {
            let dest = sequential.join(&file.path);
            config::private_dir(dest.parent().context("Invalid package parent").unwrap()).unwrap();
            fs::copy(source.join(&file.path), &dest).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &dest,
                    fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }),
                )
                .unwrap();
            }
        }
        config::private_write(&sequential.join("manifest.json"), &bytes).unwrap();
        let copied = AtomicUsize::new(0);
        let names = Mutex::new(BTreeSet::new());
        copy_files_with(
            &manifest.files,
            usize::MAX,
            |file| {
                assert!(names.lock().unwrap().insert(file.path.clone()));
                copy_file(&source, &parallel, file)?;
                copied.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();
        config::private_write(&parallel.join("manifest.json"), &bytes).unwrap();
        assert_eq!(copied.load(Ordering::SeqCst), manifest.files.len());
        assert_eq!(names.lock().unwrap().len(), manifest.files.len());
        for file in &manifest.files {
            let before = sequential.join(&file.path);
            let after = parallel.join(&file.path);
            assert_eq!(fs::read(&before).unwrap(), fs::read(&after).unwrap());
            #[cfg(windows)]
            {
                // private_dir retains the OS-created owner and sets its DACL.
                // Compare both actual policies; do not assume that an elevated
                // Windows token creates directories with its user as owner.
                assert_eq!(
                    directory_policy(before.parent().unwrap()),
                    directory_policy(after.parent().unwrap())
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                amplifier_native_platform::validate_private_directory(before.parent().unwrap())
                    .unwrap();
                amplifier_native_platform::validate_private_directory(after.parent().unwrap())
                    .unwrap();
                assert_eq!(
                    fs::metadata(&before).unwrap().permissions().mode() & 0o777,
                    fs::metadata(&after).unwrap().permissions().mode() & 0o777
                );
            }
        }
        assert_eq!(load(&sequential).unwrap().1, load(&parallel).unwrap().1);
        let home = temp.path().join("actual install");
        config::private_dir(&home).unwrap();
        fs::write(home.join("protected-state"), b"unchanged").unwrap();
        let (installed, scans) = count_verifications(|| install(&source, &home));
        assert_eq!(scans, 2, "source and copied bundle scans are both required");
        let installed = installed.unwrap();
        let installed_path = Path::new(installed["bundle"].as_str().unwrap());
        assert_eq!(load(installed_path).unwrap().1, bytes);
        assert_eq!(
            fs::read(home.join("protected-state")).unwrap(),
            b"unchanged"
        );
        // A changed support file is still rejected after the copy completes.
        fs::write(parallel.join("bridge/shared/file-0.js"), b"corrupt").unwrap();
        assert!(load(&parallel).is_err());
    }

    fn copy_fixture_files() -> Vec<File> {
        (0..16)
            .map(|index| File {
                path: index.to_string(),
                sha256: "0".repeat(64),
                bytes: 0,
                executable: false,
            })
            .collect()
    }

    #[test]
    fn copying_is_bounded_and_joins_workers_before_error_panic_or_cleanup() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        use std::time::Duration;

        // The old sequential iteration admits one callback at a time. This is
        // a concurrency oracle, not a wall-time performance assertion.
        let baseline_active = AtomicUsize::new(0);
        let baseline_maximum = AtomicUsize::new(0);
        for _file in &copy_fixture_files() {
            let active = baseline_active.fetch_add(1, Ordering::SeqCst) + 1;
            baseline_maximum.fetch_max(active, Ordering::SeqCst);
            baseline_active.fetch_sub(1, Ordering::SeqCst);
        }
        assert_eq!(baseline_maximum.load(Ordering::SeqCst), 1);

        for panic_first in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let staged = temp.path().join("staged");
            config::private_dir(&staged).unwrap();
            let worker_stage = staged.clone();
            let (entered, entries) = mpsc::channel();
            let (finished, finishes) = mpsc::channel();
            let (done, outcome) = mpsc::channel();
            let mut releases = Vec::new();
            let mut waits = Vec::new();
            for _ in 0..16 {
                let (release, wait) = mpsc::sync_channel(1);
                releases.push(release);
                waits.push(Mutex::new(wait));
            }
            let maximum = Arc::new(AtomicUsize::new(0));
            let observed_maximum = maximum.clone();
            let runner = thread::spawn(move || {
                let active = AtomicUsize::new(0);
                let result = copy_files_with(
                    &copy_fixture_files(),
                    usize::MAX,
                    |file| {
                        let index: usize = file.path.parse().unwrap();
                        let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(running, Ordering::SeqCst);
                        entered.send(index).unwrap();
                        let released = waits[index]
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(10));
                        assert!(worker_stage.is_dir(), "cleanup raced a copy worker");
                        active.fetch_sub(1, Ordering::SeqCst);
                        finished.send(index).unwrap();
                        released.context("Test controller did not release copier")?;
                        if index == 0 {
                            assert!(!panic_first, "injected copy panic");
                            anyhow::bail!("injected copy failure");
                        }
                        fs::write(worker_stage.join(&file.path), b"copied")?;
                        Ok(())
                    },
                    |_| Ok(()),
                );
                assert!(result.is_err());
                assert!(!worker_stage.join("manifest.json").exists());
                fs::remove_dir_all(&worker_stage).unwrap();
                done.send(result).unwrap();
            });
            let mut started = Vec::new();
            for _ in 0..4 {
                match entries.recv_timeout(Duration::from_secs(5)) {
                    Ok(index) => started.push(index),
                    Err(_) => break,
                }
            }
            let _ = releases[0].send(());
            let first_finished = finishes.recv_timeout(Duration::from_secs(5));
            let premature = outcome.recv_timeout(Duration::from_millis(100));
            let stage_retained_while_blocked = staged.is_dir();
            for release in &releases[1..] {
                let _ = release.send(());
            }
            runner.join().unwrap();
            let returned_early = premature.is_ok();
            let result = premature.unwrap_or_else(|_| outcome.recv().unwrap());
            started.sort_unstable();
            assert_eq!(started, [0, 4, 8, 12]);
            assert_eq!(observed_maximum.load(Ordering::SeqCst), 4);
            assert_eq!(first_finished.unwrap(), 0);
            assert!(
                !returned_early,
                "copy returned while another worker was active"
            );
            assert!(stage_retained_while_blocked);
            assert!(result.is_err());
            assert_eq!(finishes.iter().count(), 12);
            assert!(!staged.exists());
        }
        eprintln!(
            "bounded copy concurrency: retained sequential maximum=1, candidate maximum=4; not a timing comparison"
        );
    }

    #[test]
    fn copy_spawn_refusal_joins_started_worker_before_return_and_cleanup() {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let staged = temp.path().join("staged");
        config::private_dir(&staged).unwrap();
        let worker_stage = staged.clone();
        let (entered, entries) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (fail_spawn, fail_requested) = mpsc::channel();
        let (refused, refusal) = mpsc::channel();
        let (done, outcome) = mpsc::channel();
        let runner = thread::spawn(move || {
            let released = Mutex::new(released);
            let copies = AtomicUsize::new(0);
            let result = copy_files_with(
                &copy_fixture_files(),
                4,
                |file| {
                    if copies.fetch_add(1, Ordering::SeqCst) == 0 {
                        entered.send(()).unwrap();
                        released
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(10))?;
                    }
                    assert!(worker_stage.is_dir(), "cleanup raced the admitted worker");
                    fs::write(worker_stage.join(&file.path), b"copied")?;
                    Ok(())
                },
                |index| {
                    if index == 1 {
                        fail_requested
                            .recv_timeout(Duration::from_secs(10))
                            .unwrap();
                        refused.send(()).unwrap();
                        return Err(std::io::Error::other("injected copy spawn refusal"));
                    }
                    Ok(())
                },
            );
            let copies = copies.load(Ordering::SeqCst);
            assert!(!worker_stage.join("manifest.json").exists());
            fs::remove_dir_all(&worker_stage).unwrap();
            done.send((result, copies)).unwrap();
        });
        let started = entries.recv_timeout(Duration::from_secs(5));
        let _ = fail_spawn.send(());
        let spawn_refused = refusal.recv_timeout(Duration::from_secs(5));
        let premature = outcome.recv_timeout(Duration::from_millis(100));
        let stage_retained_while_blocked = staged.is_dir();
        let _ = release.send(());
        runner.join().unwrap();
        assert!(started.is_ok() && spawn_refused.is_ok());
        assert!(
            premature.is_err(),
            "spawn refusal returned before joining the worker"
        );
        let (result, copied) = outcome.recv().unwrap();
        assert_eq!(copied, 4, "only the first admitted chunk may copy");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Cannot start bundle copy worker")
        );
        assert!(stage_retained_while_blocked);
        assert!(!staged.exists());
    }

    #[test]
    fn copying_refuses_non_directory_parent_without_manifest_or_outside_change() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fixture(&source);
        let (manifest, _, _) = load(&source).unwrap();
        let staged = temp.path().join("staged");
        config::private_dir(&staged).unwrap();
        fs::write(staged.join("bridge"), b"refuse this ordinary file").unwrap();
        let protected = temp.path().join("protected");
        fs::write(&protected, b"unchanged").unwrap();
        assert!(
            copy_files_with(
                &manifest.files,
                4,
                |file| copy_file(&source, &staged, file),
                |_| Ok(())
            )
            .is_err()
        );
        assert!(!staged.join("manifest.json").exists());
        assert_eq!(
            fs::read(staged.join("bridge")).unwrap(),
            b"refuse this ordinary file"
        );
        assert_eq!(fs::read(&protected).unwrap(), b"unchanged");
    }

    #[test]
    fn verifies_every_manifest_file_across_parallel_regions() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        let manifest_path = tmp.path().join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let contents = b"complete verified dependency";
        for index in 0..20 {
            let name = format!("bridge/dependency-{index}.js");
            fs::write(tmp.path().join(&name), contents).unwrap();
            manifest["files"].as_array_mut().unwrap().push(json!({
                "path": name, "sha256": hex(&Sha256::digest(contents)),
                "bytes": contents.len(), "executable": false
            }));
        }
        fs::write(&manifest_path, manifest.to_string()).unwrap();
        verify(tmp.path()).unwrap();
        let parsed: Manifest = serde_json::from_value(manifest).unwrap();
        for index in [0, 7, 13, 19] {
            let path = tmp.path().join(format!("bridge/dependency-{index}.js"));
            let mut corrupt = contents.to_vec();
            corrupt[0] ^= 1;
            fs::write(&path, &corrupt).unwrap();
            // Force all four regions even on a single-core test runner. The
            // actual filesystem, size check and SHA verifier remain in use.
            assert!(
                verify_files_with(&parsed.files, 4, |file| { verify_file(tmp.path(), file) })
                    .is_err()
            );
            assert!(verify(tmp.path()).is_err());
            fs::write(path, contents).unwrap();
        }
        fs::remove_file(tmp.path().join("bridge/dependency-19.js")).unwrap();
        assert!(verify(tmp.path()).is_err());
    }

    #[test]
    fn verification_is_bounded_and_joins_other_workers_before_error_or_panic() {
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        use std::time::Duration;

        for panic_first in [false, true] {
            let files: Vec<File> = (0..16)
                .map(|index| File {
                    path: index.to_string(),
                    sha256: "0".repeat(64),
                    bytes: 0,
                    executable: false,
                })
                .collect();
            let (entered, entries) = mpsc::channel();
            let (finished, finishes) = mpsc::channel();
            let (done, outcome) = mpsc::channel();
            let mut releases = Vec::new();
            let mut waits = Vec::new();
            for _ in 0..16 {
                let (release, wait) = mpsc::sync_channel(1);
                releases.push(release);
                waits.push(Mutex::new(wait));
            }
            let maximum = Arc::new(AtomicUsize::new(0));
            let observed_maximum = maximum.clone();
            let runner = thread::spawn(move || {
                let active = AtomicUsize::new(0);
                let result = verify_files_with(&files, usize::MAX, |file| {
                    let index: usize = file.path.parse().unwrap();
                    let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(running, Ordering::SeqCst);
                    entered.send(index).unwrap();
                    let released = waits[index]
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                    active.fetch_sub(1, Ordering::SeqCst);
                    finished.send(index).unwrap();
                    released.context("Test controller did not release verifier")?;
                    if index == 0 {
                        assert!(!panic_first, "injected verification panic");
                        anyhow::bail!("injected content verification failure");
                    }
                    Ok(())
                });
                done.send(result).unwrap();
            });
            let mut started = Vec::new();
            for _ in 0..4 {
                match entries.recv_timeout(Duration::from_secs(5)) {
                    Ok(index) => started.push(index),
                    Err(_) => break,
                }
            }
            // The first worker fails while every other worker remains blocked.
            let _ = releases[0].send(());
            let first_finished = finishes.recv_timeout(Duration::from_secs(5));
            let premature = outcome.recv_timeout(Duration::from_millis(100));
            // Always release all test workers before making assertions.
            for release in &releases[1..] {
                let _ = release.send(());
            }
            runner.join().unwrap();
            let returned_early = premature.is_ok();
            let result = premature.unwrap_or_else(|_| outcome.recv().unwrap());
            started.sort_unstable();
            assert_eq!(started, [0, 4, 8, 12]);
            assert_eq!(observed_maximum.load(Ordering::SeqCst), 4);
            assert_eq!(first_finished.unwrap(), 0);
            assert!(
                !returned_early,
                "Returned while another verifier was active"
            );
            assert!(result.is_err());
            assert_eq!(finishes.iter().count(), 12);
        }
    }

    #[test]
    fn refuses_traversal_and_links() {
        for bad in [
            "../outside",
            "/absolute",
            "C:/outside",
            "a\\b",
            "a//b",
            "a/./b",
            "trailing.",
            "NUL.txt",
            "folder/CoM1.bin",
        ] {
            assert!(relative(bad).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            let tmp = tempfile::tempdir().unwrap();
            fixture(tmp.path());
            fs::remove_file(tmp.path().join("bridge/live.mjs")).unwrap();
            std::os::unix::fs::symlink("companion-mcp.mjs", tmp.path().join("bridge/live.mjs"))
                .unwrap();
            assert!(verify(tmp.path()).is_err());
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn elf_requires_executable_entrypoint_but_accepts_static_pie() {
        let temp = tempfile::tempdir().unwrap();
        fixture(temp.path());
        let path = temp.path().join("agent-conversations");
        let mut pie = fs::read(&path).unwrap();
        pie[16] = 3; // ET_DYN, with no PT_INTERP: a static PIE is valid.
        fs::write(&path, &pie).unwrap();
        native_executable(&path).unwrap();
        for range in [24..32, 68..72, 96..104] {
            let mut invalid = pie.clone();
            invalid[range].fill(0);
            fs::write(&path, invalid).unwrap();
            assert!(native_executable(&path).is_err());
        }
        pie[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&path, pie).unwrap();
        assert!(native_executable(&path).is_err());
    }
}
