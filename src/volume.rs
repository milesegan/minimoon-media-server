//! Volume identity: detecting a shared folder whose drive is gone (spec §5, "Unavailable shared
//! folders").
//!
//! An unplugged drive or an unmounted network volume usually leaves an empty mount point behind
//! on the startup disk, so emptiness can't be trusted. Instead, when a folder is added the server
//! records the mount point of the volume it lives on, and before every scan it checks that the
//! folder is still on a volume mounted there. Device numbers aren't used, because macOS hands out
//! a different one when the same drive is mounted again.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Where a shared folder's volume is mounted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeIdentity {
    /// The mount point of the volume holding the folder, e.g. `/Volumes/Media`, or `/` for the
    /// root file system. Empty when it couldn't be determined.
    pub mount_point: String,
}

impl VolumeIdentity {
    /// The identity of the volume a folder is on now.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            mount_point: mount_point(path)?,
        })
    }
}

/// Why a shared folder is unavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The folder doesn't exist or isn't a directory.
    Missing,
    /// The folder exists but can't be listed.
    Unreadable(String),
    /// The folder is now on a different volume: its own drive isn't mounted.
    WrongVolume { expected: String, found: String },
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "the folder is missing"),
            Self::Unreadable(error) => write!(f, "the folder can't be read: {error}"),
            Self::WrongVolume { expected, found } => write!(
                f,
                "the folder's volume isn't mounted (expected {expected}, found {found})"
            ),
        }
    }
}

/// Checks that a folder exists, can be listed, and is on the volume it was added on.
pub fn check(path: &Path, expected: &VolumeIdentity) -> Result<(), Unavailable> {
    let metadata = std::fs::metadata(path).map_err(|_| Unavailable::Missing)?;
    if !metadata.is_dir() {
        return Err(Unavailable::Missing);
    }
    std::fs::read_dir(path).map_err(|error| Unavailable::Unreadable(error.to_string()))?;
    if expected.mount_point.is_empty() {
        return Ok(());
    }
    let found = mount_point(path).map_err(|error| Unavailable::Unreadable(error.to_string()))?;
    if found != expected.mount_point {
        return Err(Unavailable::WrongVolume {
            expected: expected.mount_point.clone(),
            found,
        });
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mount_point(path: &Path) -> std::io::Result<String> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: c_path is a valid NUL-terminated string and stat is a writable statfs.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statfs fills f_mntonname with a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) };
    Ok(name.to_string_lossy().into_owned())
}

/// Walks up from the folder until the device number changes; the last directory on the
/// folder's device is its mount point.
#[cfg(all(unix, not(target_os = "macos")))]
fn mount_point(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let path = path.canonicalize()?;
    let device = std::fs::metadata(&path)?.dev();
    let mut mount = path.as_path();
    while let Some(parent) = mount.parent() {
        if std::fs::metadata(parent)?.dev() != device {
            break;
        }
        mount = parent;
    }
    Ok(mount.to_string_lossy().into_owned())
}

/// On Windows the drive or UNC share prefix stands in for the mount point.
#[cfg(windows)]
fn mount_point(path: &Path) -> std::io::Result<String> {
    use std::path::Component;
    let path = path.canonicalize()?;
    Ok(match path.components().next() {
        Some(Component::Prefix(prefix)) => prefix.as_os_str().to_string_lossy().into_owned(),
        _ => String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_on_its_own_volume_is_available() {
        let dir = tempfile::tempdir().unwrap();
        let identity = VolumeIdentity::of(dir.path()).unwrap();
        assert!(!identity.mount_point.is_empty());
        assert_eq!(check(dir.path(), &identity), Ok(()));
    }

    #[test]
    fn a_folder_on_another_volume_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let identity = VolumeIdentity {
            mount_point: "/Volumes/Not Mounted".into(),
        };
        assert!(matches!(
            check(dir.path(), &identity),
            Err(Unavailable::WrongVolume { .. })
        ));
    }

    #[test]
    fn a_missing_folder_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let identity = VolumeIdentity::of(dir.path()).unwrap();
        assert_eq!(
            check(&dir.path().join("gone"), &identity),
            Err(Unavailable::Missing)
        );
    }
}
