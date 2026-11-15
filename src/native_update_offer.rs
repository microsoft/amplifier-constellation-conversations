//! A downloaded application's private, unverified suggestion to update an
//! existing installation. An offer never authorizes verification or execution.
use crate::{config::Config, desktop};
use amplifier_native_platform as platform;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 16 * 1024;
const MAX_OFFERS: usize = 128;
const CURRENT: &str = "current-offer.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Offer {
    schema: u32,
    pub(crate) id: String,
    pub(crate) installation_id: String,
    pub(crate) package_folder: PathBuf,
    pub(crate) launcher: PathBuf,
    pub(crate) target_version: String,
    pub(crate) created_at_ms: u64,
    pub(crate) verified: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Current {
    schema: u32,
    installation_id: String,
    offer_id: String,
}

fn id(value: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(value)?.to_string() == value,
        "Use a canonical update offer ID"
    );
    Ok(())
}

fn version(value: &str) -> Option<[u64; 3]> {
    if value.len() > 64 {
        return None;
    }
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut result = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || !part.bytes().all(|v| v.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        result[index] = part.parse().ok()?;
    }
    Some(result)
}

pub(crate) fn newer(candidate: &str, selected: &str) -> bool {
    matches!((version(candidate), version(selected)), (Some(a), Some(b)) if a > b)
}

fn ordinary(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(metadata.is_file(), "Expected an ordinary update offer");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Update offer is a reparse point"
        );
    }
    Ok(metadata)
}

fn bounded<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        ordinary(path)?.len() <= MAX_BYTES as u64,
        "Update offer exceeds its bound"
    );
    let mut options = fs::OpenOptions::new();
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
    let file = options.open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.len() <= MAX_BYTES as u64
            && crate::native_check_diagnostics::private_file_policy(&file),
        "Invalid update offer file"
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            before.file_attributes() & 0x400 == 0,
            "Opened update offer is a reparse point"
        );
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    ensure!(
        bytes.len() <= MAX_BYTES
            && before.len() == after.len()
            && before.modified()? == after.modified()?
            && bytes.len() as u64 == after.len(),
        "Update offer changed while reading"
    );
    ordinary(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn directory(home: &Path, create: bool) -> Result<PathBuf> {
    platform::validate_private_directory(home)?;
    let root = home.join("native-browser-update");
    let offers = root.join("offers");
    for path in [&root, &offers] {
        if create && !path.exists() && !path.is_symlink() {
            platform::private_create_dir(path)?;
        }
        // Never repair the policy of an existing directory to accept an offer.
        platform::validate_private_directory(path)?;
    }
    Ok(offers)
}

fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|v| matches!(v, Component::CurDir | Component::ParentDir))
}

fn validate(home: &Path, value: &Offer, expected: &str) -> Result<()> {
    id(expected)?;
    ensure!(
        value.schema == 1
            && value.id == expected
            && value.installation_id == Config::load(home)?.id
            && !value.verified
            && version(&value.target_version).is_some()
            && value.created_at_ms > 0
            && absolute(&value.package_folder)
            && absolute(&value.launcher),
        "Update offer does not match this installation"
    );
    let selected = desktop::selected_package_paths(&value.package_folder);
    ensure!(
        selected.launcher == value.launcher,
        "Update offer launcher layout changed"
    );
    Ok(())
}

/// Browser input is only a UUID. The package/launcher paths are fixed by the
/// native desktop entrypoint and remain unverified until the full update check.
pub(crate) fn read_offer(home: &Path, offer_id: &str) -> Result<Offer> {
    id(offer_id)?;
    let value: Offer = bounded(&directory(home, false)?.join(format!("{offer_id}.json")))?;
    validate(home, &value, offer_id)?;
    Ok(value)
}

pub(crate) fn latest_offer(home: &Path) -> Result<Option<Offer>> {
    let root = home.join("native-browser-update");
    if !root.exists() && !root.is_symlink() {
        return Ok(None);
    }
    platform::validate_private_directory(&root)?;
    let offers = root.join("offers");
    if !offers.exists() && !offers.is_symlink() {
        return Ok(None);
    }
    let path = directory(home, false)?.join(CURRENT);
    if !path.exists() && !path.is_symlink() {
        return Ok(None);
    }
    let current: Current = bounded(&path)?;
    ensure!(
        current.schema == 1 && current.installation_id == Config::load(home)?.id,
        "Current offer belongs to a different installation"
    );
    Ok(Some(read_offer(home, &current.offer_id)?))
}

pub(crate) fn create_offer(
    home: &Path,
    package_folder: PathBuf,
    launcher: PathBuf,
    target_version: &str,
) -> Result<Offer> {
    desktop::selected_package_folder(&package_folder)?;
    ordinary(&launcher)?;
    let value = Offer {
        schema: 1,
        id: uuid::Uuid::new_v4().to_string(),
        installation_id: Config::load(home)?.id,
        package_folder,
        launcher,
        target_version: target_version.to_owned(),
        created_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_millis()
            .try_into()?,
        verified: false,
    };
    validate(home, &value, &value.id)?;
    let directory = directory(home, true)?;
    let mut count = 0;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.file_name() == CURRENT {
            ordinary(&entry.path())?;
            continue;
        }
        count += 1;
        ensure!(
            count <= MAX_OFFERS,
            "Too many retained update offers; use the existing update page"
        );
        let existing: Offer = bounded(&entry.path())?;
        validate(
            home,
            &existing,
            entry
                .path()
                .file_stem()
                .and_then(|v| v.to_str())
                .context("Invalid offer filename")?,
        )?;
        if existing.package_folder == value.package_folder
            && existing.launcher == value.launcher
            && existing.target_version == value.target_version
        {
            publish_current(&directory, &existing)?;
            return Ok(existing);
        }
    }
    ensure!(
        count < MAX_OFFERS,
        "Too many retained update offers; use the existing update page"
    );
    let bytes = serde_json::to_vec(&value)?;
    ensure!(bytes.len() <= MAX_BYTES, "Update offer exceeds its bound");
    platform::private_create(&directory.join(format!("{}.json", value.id)), &bytes)?;
    publish_current(&directory, &value)?;
    Ok(value)
}

fn publish_current(directory: &Path, offer: &Offer) -> Result<()> {
    let path = directory.join(CURRENT);
    if path.exists() || path.is_symlink() {
        let previous: Current = bounded(&path)?;
        ensure!(
            previous.schema == 1 && previous.installation_id == offer.installation_id,
            "Current offer belongs to a different installation"
        );
        id(&previous.offer_id)?;
    }
    platform::private_write_data(
        &path,
        &serde_json::to_vec(&Current {
            schema: 1,
            installation_id: offer.installation_id.clone(),
            offer_id: offer.id.clone(),
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        platform::private_create_dir(&home).unwrap();
        Config::local(&home, "fixture", 31471)
            .unwrap()
            .save(&home)
            .unwrap();
        let package = temp.path().join("download café");
        fs::create_dir(&package).unwrap();
        let launcher = package.join(crate::platform::executable("agent-conversations"));
        fs::write(&launcher, b"deliberately unverified package").unwrap();
        (temp, home, package, launcher)
    }

    #[test]
    fn only_strict_newer_release_versions_offer_an_update() {
        assert!(newer("0.14.0", "0.13.99"));
        assert!(newer("0.13.10", "0.13.9"));
        for (a, b) in [
            ("0.13.4", "0.13.4"),
            ("0.13.3", "0.13.4"),
            ("0.13.5-dev", "0.13.4"),
            ("00.13.5", "0.13.4"),
            ("0.13.5", "unknown"),
        ] {
            assert!(!newer(a, b));
        }
    }

    #[test]
    fn offer_is_private_installation_bound_unverified_and_reused() {
        let (_temp, home, package, launcher) = fixture();
        assert!(latest_offer(&home).unwrap().is_none());
        let offer = create_offer(&home, package.clone(), launcher.clone(), "0.13.5").unwrap();
        assert!(!read_offer(&home, &offer.id).unwrap().verified);
        assert_eq!(latest_offer(&home).unwrap().unwrap().id, offer.id);
        assert_eq!(
            create_offer(&home, package, launcher, "0.13.5").unwrap().id,
            offer.id
        );
        assert!(!home.join("native-installation.json").exists());
        assert!(!home.join("native-update-owner.json").exists());
        assert!(read_offer(&home, "../config").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(
                    directory(&home, false)
                        .unwrap()
                        .join(format!("{}.json", offer.id))
                )
                .unwrap()
                .permissions()
                .mode()
                    & 0o077,
                0
            );
        }
        let mut config = Config::load(&home).unwrap();
        config.id = uuid::Uuid::new_v4().to_string();
        config.save(&home).unwrap();
        assert!(read_offer(&home, &offer.id).is_err());
        assert!(latest_offer(&home).is_err());
    }

    #[test]
    fn excess_data_and_modified_launcher_or_verified_claim_refuse() {
        let (_temp, home, package, launcher) = fixture();
        let offer = create_offer(&home, package, launcher, "0.13.5").unwrap();
        let path = directory(&home, false)
            .unwrap()
            .join(format!("{}.json", offer.id));
        let mut value = serde_json::to_value(&offer).unwrap();
        value["verified"] = true.into();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_offer(&home, &offer.id).is_err());
        value["verified"] = false.into();
        value["launcher"] = "/arbitrary-command".into();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_offer(&home, &offer.id).is_err());
        fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(read_offer(&home, &offer.id).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn linked_offer_or_nonprivate_parent_is_preserved_and_refused() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (temp, home, package, launcher) = fixture();
        let offer = create_offer(&home, package, launcher, "0.13.5").unwrap();
        let dir = directory(&home, false).unwrap();
        let path = dir.join(format!("{}.json", offer.id));
        let other = temp.path().join("retained");
        fs::rename(&path, &other).unwrap();
        let original = fs::read(&other).unwrap();
        symlink(&other, &path).unwrap();
        assert!(read_offer(&home, &offer.id).is_err());
        assert_eq!(fs::read(&other).unwrap(), original);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(latest_offer(&home).is_err());
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
