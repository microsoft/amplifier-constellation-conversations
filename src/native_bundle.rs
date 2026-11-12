//! Installation of an already acquired, immutable native bundle. This does not
//! enroll updates, change an agent, replace configuration, or start a service.
use crate::config;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
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
fn load(root: &Path) -> Result<(Manifest, Vec<u8>, String)> {
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
    for file in &m.files {
        let path = root.join(&file.path);
        ensure!(
            fs::metadata(&path)?.len() == file.bytes && digest(&path)? == file.sha256,
            "Bundle file failed verification: {}",
            file.path
        );
    }
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
pub fn install(root: &Path, home: &Path) -> Result<Value> {
    // Verify before creating installation state; extraction/download belongs to
    // the package acquisition surface, not to a remotely callable host API.
    let (m, bytes, hash) = load(root)?;
    config::private_dir(home)?;
    let home = dunce::canonicalize(home)?;
    let packages = home.join("packages");
    config::private_dir(&packages)?;
    let destination = packages.join(&hash);
    if destination.exists() {
        let (_, existing, _) = load(&destination)?;
        ensure!(existing == bytes, "Existing immutable package differs");
        return Ok(result(&destination, &m, &hash));
    }
    let staged = packages.join(format!(".install-{}", uuid::Uuid::new_v4()));
    config::private_dir(&staged)?;
    let attempt = (|| -> Result<()> {
        for file in &m.files {
            let dest = staged.join(&file.path);
            config::private_dir(dest.parent().context("Invalid package parent")?)?;
            // Copy into a private directory, then preserve executable identity.
            fs::copy(root.join(&file.path), &dest)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &dest,
                    fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }),
                )?;
            }
        }
        config::private_write(&staged.join("manifest.json"), &bytes)?;
        load(&staged)?;
        amplifier_native_platform::publish_directory_if_absent(&staged, &destination)?;
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
