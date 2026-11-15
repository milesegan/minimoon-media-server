//! The server's settings: everything that can't be rebuilt from the disks (spec §8, "Settings,
//! not index").
//!
//! Rebuilding the index from scratch never changes an identity, because the server id, the
//! shared folders' UUIDs and volume identities, the bumped `rev` values and the pairing state all
//! live here, in `settings.json` in the server's data directory.

use crate::volume::VolumeIdentity;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The settings file's current format.
const FORMAT_VERSION: u32 = 1;

/// Whether clients need a pairing code (spec §4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMode {
    /// Open: any device on the network can read the shared folders.
    #[default]
    None,
    /// New devices need a six-digit code shown on the server.
    Pairing,
}

impl AuthMode {
    /// The value of the `auth` field and TXT record.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pairing => "pairing",
        }
    }
}

/// A folder on disk the user shares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedFolderSettings {
    /// Permanent; identity, selections and availability all use it.
    pub id: Uuid,
    /// Where the folder is on disk.
    pub path: PathBuf,
    /// The display name, only a label.
    pub name: String,
    /// The volume the folder lived on when it was added.
    pub volume: VolumeIdentity,
}

/// Bumped content revisions (spec §5, `rev`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevSettings {
    /// The last value handed out. Bumps take the next one, so a value is never reused.
    pub counter: u64,
    /// Each bumped file's current `rev`, by shared folder id and then path. Files never bumped
    /// have `rev` 0 and no entry.
    pub files: BTreeMap<Uuid, BTreeMap<String, u64>>,
}

/// A device paired with the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub client_id: String,
    pub client_name: String,
    pub app: String,
    pub platform: String,
    /// SHA-256 of the bearer token, hex.
    pub token_hash: String,
    /// Bumped when the device is revoked; part of every signed URL.
    pub generation: u64,
    /// Milliseconds since the Unix epoch.
    pub paired_at: u64,
    /// A revoked device keeps its entry so its token gets `token_revoked`, not `not_paired`.
    #[serde(default)]
    pub revoked: bool,
}

/// Pairing state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingSettings {
    /// The URL-signing key, base64url. Created the first time pairing is turned on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<String>,
    #[serde(default)]
    pub devices: Vec<PairedDevice>,
}

/// Everything in `settings.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub format_version: u32,
    /// The server's UUID, persisted for the life of the install.
    pub server_id: Uuid,
    /// The display name; the host name when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub shared_folders: Vec<SharedFolderSettings>,
    #[serde(default)]
    pub revs: RevSettings,
    /// The watcher's last processed FSEvents event id (macOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsevents_event_id: Option<u64>,
    #[serde(default)]
    pub auth: AuthMode,
    #[serde(default)]
    pub pairing: PairingSettings,
    /// "Keep this computer awake while sharing".
    #[serde(default)]
    pub keep_awake: bool,
}

impl Settings {
    /// Fresh settings for a new install.
    pub fn new() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            server_id: Uuid::new_v4(),
            name: None,
            shared_folders: Vec::new(),
            revs: RevSettings::default(),
            fsevents_event_id: None,
            auth: AuthMode::None,
            pairing: PairingSettings::default(),
            keep_awake: false,
        }
    }

    /// The settings file inside a data directory.
    pub fn path_in(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    /// Loads the settings, creating and saving fresh ones when there are none yet.
    ///
    /// A file that exists but doesn't parse is an error, never silently replaced: replacing it
    /// would give the server a new id and new shared-folder UUIDs, which clients would see as a
    /// different server with different files.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = Self::path_in(data_dir);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let settings: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("can't parse {}", path.display()))?;
                anyhow::ensure!(
                    settings.format_version <= FORMAT_VERSION,
                    "{} was written by a newer server (format {})",
                    path.display(),
                    settings.format_version
                );
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let settings = Self::new();
                settings.save(data_dir)?;
                Ok(settings)
            }
            Err(error) => Err(error).with_context(|| format!("can't read {}", path.display())),
        }
    }

    /// Writes the settings atomically.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        write_atomically(&Self::path_in(data_dir), &bytes)
    }

    /// The display name: the configured one, or the host name.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(default_server_name)
    }

    /// A shared folder by id.
    pub fn folder(&self, id: Uuid) -> Option<&SharedFolderSettings> {
        self.shared_folders.iter().find(|folder| folder.id == id)
    }

    /// A file's current `rev`: its bumped value, or 0.
    pub fn rev(&self, folder: Uuid, path: &str) -> u64 {
        self.revs
            .files
            .get(&folder)
            .and_then(|files| files.get(path))
            .copied()
            .unwrap_or(0)
    }

    /// Gives a file the next `rev` from the server-wide counter and returns it.
    pub fn bump_rev(&mut self, folder: Uuid, path: &str) -> u64 {
        self.revs.counter += 1;
        let rev = self.revs.counter;
        self.revs
            .files
            .entry(folder)
            .or_default()
            .insert(path.to_string(), rev);
        rev
    }

    /// Drops `rev` entries for files a completed scan didn't find. Returns whether any went.
    pub fn prune_revs(&mut self, folder: Uuid, exists: impl Fn(&str) -> bool) -> bool {
        let Some(files) = self.revs.files.get_mut(&folder) else {
            return false;
        };
        let before = files.len();
        files.retain(|path, _| exists(path));
        let changed = files.len() != before;
        if files.is_empty() {
            self.revs.files.remove(&folder);
        }
        changed
    }

    /// A display name for a new shared folder that doesn't clash with an existing one
    /// (`Music`, `Music 2`, …).
    pub fn unique_folder_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() {
            "Folder"
        } else {
            base
        };
        let taken = |name: &str| self.shared_folders.iter().any(|f| f.name == name);
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|name| !taken(name))
            .expect("an unused name exists")
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

/// The host name, without a `.local` suffix.
pub fn default_server_name() -> String {
    let host = gethostname::gethostname().to_string_lossy().into_owned();
    host.strip_suffix(".local").unwrap_or(&host).to_string()
}

/// Writes a file by writing a sibling temporary file and renaming it over the target, so a
/// crash never leaves a half-written file.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().context("settings path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    let temp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("settings"),
        std::process::id()
    ));
    {
        let mut file = std::fs::File::create(&temp)
            .with_context(|| format!("can't write {}", temp.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path).with_context(|| format!("can't replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumps_never_reuse_a_value() {
        let mut settings = Settings::new();
        let folder = Uuid::new_v4();
        assert_eq!(settings.rev(folder, "a.flac"), 0);
        assert_eq!(settings.bump_rev(folder, "a.flac"), 1);
        assert_eq!(settings.bump_rev(folder, "b.flac"), 2);
        settings.prune_revs(folder, |path| path != "a.flac");
        assert_eq!(settings.rev(folder, "a.flac"), 0);
        // The counter keeps going after a prune, so a file that comes back with changed
        // content can't get a value an old copy already had.
        assert_eq!(settings.bump_rev(folder, "a.flac"), 3);
    }

    #[test]
    fn round_trips_and_keeps_the_server_id() {
        let dir = tempfile::tempdir().unwrap();
        let created = Settings::load_or_create(dir.path()).unwrap();
        let loaded = Settings::load_or_create(dir.path()).unwrap();
        assert_eq!(created.server_id, loaded.server_id);
    }

    #[test]
    fn refuses_to_replace_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Settings::path_in(dir.path()), b"{not json").unwrap();
        assert!(Settings::load_or_create(dir.path()).is_err());
    }

    #[test]
    fn numbers_clashing_folder_names() {
        let mut settings = Settings::new();
        for name in ["Music", "Music 2"] {
            settings.shared_folders.push(SharedFolderSettings {
                id: Uuid::new_v4(),
                path: PathBuf::from("/x"),
                name: name.into(),
                volume: VolumeIdentity::default(),
            });
        }
        assert_eq!(settings.unique_folder_name("Music"), "Music 3");
        assert_eq!(settings.unique_folder_name("Movies"), "Movies");
    }
}
