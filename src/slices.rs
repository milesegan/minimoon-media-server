//! Slice definitions: their stored format, strict parsing, and evaluation (spec §8, "Slice
//! definitions").
//!
//! A definition is a typed syntax tree. v1 implements `all` and `selection`; anything else, such
//! as a rule-based node written by a newer server, doesn't parse, and that slice is listed as
//! unavailable rather than published with a guess or dropped. Each slice is parsed on its own and
//! stored verbatim, so writing the file back never drops or alters a definition this server
//! couldn't read.

use crate::settings::write_atomically;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The slice file format this server reads and writes.
pub const FORMAT_VERSION: u32 = 1;

/// The `kind` hint in `/v1/server`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SliceKind {
    Music,
    Video,
    Mixed,
}

impl SliceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Video => "video",
            Self::Mixed => "mixed",
        }
    }
}

/// A path in the unified tree: a shared folder plus components within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreePath {
    pub folder: Uuid,
    pub path: Vec<String>,
}

impl TreePath {
    /// The components joined with `/`, as index paths are written.
    pub fn joined(&self) -> String {
        self.path.join("/")
    }

    fn validate(&self) -> Result<()> {
        for component in &self.path {
            if component.is_empty() || component == "." || component == ".." {
                bail!("invalid path component {component:?}");
            }
            if component.contains('/') || component.contains('\0') {
                bail!("path component {component:?} contains a separator");
            }
        }
        Ok(())
    }
}

/// A slice definition. Only the v1 node types exist here; any other type fails to parse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SliceExpr {
    /// Every file in every shared folder.
    All,
    /// Whole folders, and single files with their companions.
    Selection {
        folders: Vec<TreePath>,
        files: Vec<TreePath>,
    },
}

impl SliceExpr {
    fn validate(&self) -> Result<()> {
        if let Self::Selection { folders, files } = self {
            for path in folders.iter().chain(files) {
                path.validate()?;
            }
            if files.iter().any(|file| file.path.is_empty()) {
                bail!("a selected file needs a path");
            }
        }
        Ok(())
    }
}

/// A slice this server understood.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slice {
    /// Created with the slice, never reused; the protocol's slice id.
    pub id: Uuid,
    pub name: String,
    pub kind: SliceKind,
    pub published: bool,
    pub definition: SliceExpr,
}

/// The fields of a slice that are always read, even when its definition isn't understood.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SliceHeader {
    id: Uuid,
    name: String,
    kind: Value,
    published: bool,
}

/// A slice as listed, whether or not its definition parsed.
#[derive(Clone, Debug, PartialEq)]
pub struct SliceEntry {
    pub id: Uuid,
    pub name: String,
    /// The kind as stored; an unknown value from a newer server is passed through.
    pub kind: String,
    pub published: bool,
    /// The parsed slice, or why it couldn't be parsed. A slice that didn't parse serves nothing.
    pub parsed: Result<Slice, String>,
    /// The slice exactly as stored, written back unchanged if this server didn't parse it.
    raw: Value,
}

impl SliceEntry {
    /// Whether the slice can be served: its definition parsed.
    pub fn available(&self) -> bool {
        self.parsed.is_ok()
    }
}

/// The slice file: `slices.json` in the data directory.
#[derive(Clone, Debug, PartialEq)]
pub struct SliceFile {
    pub format_version: u32,
    pub slices: Vec<SliceEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSliceFile {
    format_version: u32,
    slices: Vec<Value>,
}

impl SliceFile {
    pub fn path_in(data_dir: &Path) -> PathBuf {
        data_dir.join("slices.json")
    }

    /// Loads the slice file. A missing file gives an empty one.
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path_in(data_dir);
        match std::fs::read(&path) {
            Ok(bytes) => {
                Self::parse(&bytes).with_context(|| format!("can't parse {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                format_version: FORMAT_VERSION,
                slices: Vec::new(),
            }),
            Err(error) => Err(error).with_context(|| format!("can't read {}", path.display())),
        }
    }

    /// Parses the file, each slice on its own.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let raw: RawSliceFile = serde_json::from_slice(bytes)?;
        let newer = raw.format_version > FORMAT_VERSION;
        let mut slices = Vec::new();
        for value in raw.slices {
            let header: SliceHeader = match serde_json::from_value(value.clone()) {
                Ok(header) => header,
                Err(error) => {
                    // Without an id the slice can't be listed at all; keep it for writing back.
                    tracing::warn!("slice without a readable id, name, kind or published: {error}");
                    slices.push(SliceEntry {
                        id: Uuid::nil(),
                        name: String::new(),
                        kind: String::new(),
                        published: false,
                        parsed: Err(error.to_string()),
                        raw: value,
                    });
                    continue;
                }
            };
            let parsed = if newer {
                Err(format!(
                    "slice file format {} is newer than this server",
                    raw.format_version
                ))
            } else {
                serde_json::from_value::<Slice>(value.clone())
                    .map_err(|error| error.to_string())
                    .and_then(|slice| {
                        slice
                            .definition
                            .validate()
                            .map_err(|error| error.to_string())?;
                        // serde doesn't reject unknown fields everywhere in a tagged enum (a
                        // unit variant ignores them), so require the definition to round-trip
                        // exactly: anything this server would drop means it didn't understand it.
                        let round_trip = serde_json::to_value(&slice.definition)
                            .map_err(|error| error.to_string())?;
                        if Some(&round_trip) != value.get("definition") {
                            return Err("the definition has fields this server doesn't know".into());
                        }
                        Ok(slice)
                    })
            };
            slices.push(SliceEntry {
                id: header.id,
                name: header.name,
                kind: header.kind.as_str().map(str::to_string).unwrap_or_default(),
                published: header.published,
                parsed,
                raw: value,
            });
        }
        Ok(Self {
            format_version: raw.format_version,
            slices,
        })
    }

    /// Whether this server may write the file: not when a newer server wrote it.
    pub fn writable(&self) -> bool {
        self.format_version <= FORMAT_VERSION
    }

    /// Serializes the file: parsed slices from their current values, the rest verbatim.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let slices = self
            .slices
            .iter()
            .map(|entry| match &entry.parsed {
                Ok(slice) => serde_json::to_value(slice).map_err(anyhow::Error::from),
                Err(_) => Ok(entry.raw.clone()),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(serde_json::to_vec_pretty(&RawSliceFile {
            format_version: self.format_version.max(FORMAT_VERSION),
            slices,
        })?)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        if !self.writable() {
            bail!("the slice file was written by a newer server and can't be changed");
        }
        write_atomically(&Self::path_in(data_dir), &self.to_bytes()?)
    }

    /// A slice by id.
    pub fn get(&self, id: Uuid) -> Option<&SliceEntry> {
        self.slices.iter().find(|entry| entry.id == id)
    }

    /// Adds or replaces a parsed slice.
    pub fn upsert(&mut self, slice: Slice) -> Result<()> {
        if !self.writable() {
            bail!("the slice file was written by a newer server and can't be changed");
        }
        slice.definition.validate()?;
        let raw = serde_json::to_value(&slice)?;
        let entry = SliceEntry {
            id: slice.id,
            name: slice.name.clone(),
            kind: slice.kind.as_str().to_string(),
            published: slice.published,
            parsed: Ok(slice),
            raw,
        };
        match self
            .slices
            .iter_mut()
            .find(|existing| existing.id == entry.id)
        {
            Some(existing) => {
                if existing.parsed.is_err() {
                    bail!("this slice needs a newer version of the server to edit");
                }
                *existing = entry;
            }
            None => self.slices.push(entry),
        }
        Ok(())
    }

    /// Removes a slice. Unparsed slices can be removed too.
    pub fn remove(&mut self, id: Uuid) -> Result<bool> {
        if !self.writable() {
            bail!("the slice file was written by a newer server and can't be changed");
        }
        let before = self.slices.len();
        self.slices.retain(|entry| entry.id != id);
        Ok(self.slices.len() != before)
    }
}

// MARK: - Companion files

/// Subtitle extensions that can be companions.
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt", "sup", "idx", "sub"];
/// Lyrics extensions that can be companions.
const LYRICS_EXTENSIONS: &[&str] = &["lrc"];
/// Folder cover names and extensions.
const COVER_NAMES: &[&str] = &["cover", "folder", "front"];
const COVER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// The part of a file name before its last `.`, or the whole name.
fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => name,
        Some(dot) => &name[..dot],
    }
}

fn extension(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    (dot > 0).then(|| name[dot + 1..].to_ascii_lowercase())
}

/// Whether `candidate` (a file name in the same folder) is a companion of the selected file
/// `selected`, or the folder's cover image.
///
/// Subtitles and lyrics are named the selected file's stem plus any further `.`-separated parts
/// (`Movie.srt`, `Movie.en.forced.srt`); covers are `cover`, `folder` or `front`.
pub fn is_companion(selected: &str, candidate: &str) -> bool {
    if candidate == selected {
        return false;
    }
    let Some(ext) = extension(candidate) else {
        return false;
    };
    if COVER_EXTENSIONS.contains(&ext.as_str())
        && COVER_NAMES
            .iter()
            .any(|name| stem(candidate).eq_ignore_ascii_case(name))
    {
        return true;
    }
    if !SUBTITLE_EXTENSIONS.contains(&ext.as_str()) && !LYRICS_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }
    let selected_stem = stem(selected);
    let candidate_stem = stem(candidate);
    candidate_stem == selected_stem
        || candidate_stem
            .strip_prefix(selected_stem)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// The file names in `siblings` (one folder's files) that are companions of `selected`.
pub fn companions<'a>(selected: &str, siblings: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    siblings
        .into_iter()
        .filter(|candidate| is_companion(selected, candidate))
        .collect()
}

/// Splits an index path into its folder part and file name.
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(slash) => (&path[..slash], &path[slash + 1..]),
        None => ("", path),
    }
}

/// Evaluates a definition against one shared folder's files (paths sorted, `/`-separated) and
/// returns the paths it selects, in order.
pub fn evaluate<'a>(expr: &SliceExpr, folder: Uuid, paths: &[&'a str]) -> Vec<&'a str> {
    match expr {
        SliceExpr::All => paths.to_vec(),
        SliceExpr::Selection { folders, files } => {
            let mut selected = vec![false; paths.len()];
            for tree in folders.iter().filter(|tree| tree.folder == folder) {
                if tree.path.is_empty() {
                    selected.iter_mut().for_each(|s| *s = true);
                    break;
                }
                let prefix = format!("{}/", tree.joined());
                let start = paths.partition_point(|path| *path < prefix.as_str());
                for (index, path) in paths.iter().enumerate().skip(start) {
                    if !path.starts_with(&prefix) {
                        break;
                    }
                    selected[index] = true;
                }
            }
            for tree in files.iter().filter(|tree| tree.folder == folder) {
                let wanted = tree.joined();
                let Ok(found) = paths.binary_search(&wanted.as_str()) else {
                    continue;
                };
                selected[found] = true;
                let (dir, name) = split_path(&wanted);
                let dir_prefix = if dir.is_empty() {
                    String::new()
                } else {
                    format!("{dir}/")
                };
                let start = paths.partition_point(|path| *path < dir_prefix.as_str());
                for (index, path) in paths.iter().enumerate().skip(start) {
                    let Some(rest) = path.strip_prefix(&dir_prefix) else {
                        break;
                    };
                    if !rest.contains('/') && is_companion(name, rest) {
                        selected[index] = true;
                    }
                }
            }
            paths
                .iter()
                .zip(selected)
                .filter_map(|(path, keep)| keep.then_some(*path))
                .collect()
        }
    }
}

// MARK: - Migration from the Sync app

/// A preset saved by the Sync app: path strings in which a trailing `/` marks a folder.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPreset {
    pub name: String,
    pub entries: Vec<String>,
    pub active: bool,
}

/// Converts the Sync app's presets into slices (spec §8, "Migration from the Sync app"): folder
/// entries become `folders`, the rest `files`, an empty preset becomes `all`, and only the
/// active preset is published.
pub fn migrate_presets(folder: Uuid, presets: &[SyncPreset]) -> Vec<Slice> {
    presets
        .iter()
        .map(|preset| {
            let mut folders = Vec::new();
            let mut files = Vec::new();
            for entry in &preset.entries {
                let is_folder = entry.ends_with('/');
                let path: Vec<String> = entry
                    .split('/')
                    .filter(|component| !component.is_empty())
                    .map(str::to_string)
                    .collect();
                let tree = TreePath { folder, path };
                if is_folder {
                    folders.push(tree);
                } else if !tree.path.is_empty() {
                    files.push(tree);
                }
            }
            let definition = if folders.is_empty() && files.is_empty() {
                SliceExpr::All
            } else {
                SliceExpr::Selection { folders, files }
            };
            Slice {
                id: Uuid::new_v4(),
                name: preset.name.clone(),
                kind: SliceKind::Mixed,
                published: preset.active,
                definition,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOLDER: &str = "a41f0000-0000-4000-8000-000000000000";

    fn folder() -> Uuid {
        FOLDER.parse().unwrap()
    }

    #[test]
    fn parses_the_spec_example() {
        let json = format!(
            r#"{{"formatVersion": 1, "slices": [
              {{"id": "6b0e0000-0000-4000-8000-000000000000", "name": "Everything", "kind": "mixed",
                "published": true, "definition": {{"type": "all"}}}},
              {{"id": "2f9a0000-0000-4000-8000-000000000000", "name": "Road Trip", "kind": "music",
                "published": true,
                "definition": {{"type": "selection",
                  "folders": [{{"folder": "{FOLDER}", "path": ["Jazz"]}}],
                  "files": [{{"folder": "{FOLDER}", "path": ["Misc", "song.flac"]}}]}}}}]}}"#
        );
        let file = SliceFile::parse(json.as_bytes()).unwrap();
        assert!(file.slices.iter().all(SliceEntry::available));
        assert_eq!(
            file.slices[1].parsed.as_ref().unwrap().kind,
            SliceKind::Music
        );
    }

    #[test]
    fn a_newer_definition_is_unavailable_and_written_back_verbatim() {
        let json = r#"{"formatVersion": 1, "slices": [
          {"id": "6b0e0000-0000-4000-8000-000000000000", "name": "Jazz", "kind": "music",
           "published": true, "definition": {"type": "and", "children": []}},
          {"id": "2f9a0000-0000-4000-8000-000000000000", "name": "All", "kind": "mixed",
           "published": true, "definition": {"type": "all", "extra": 1}}]}"#;
        let file = SliceFile::parse(json.as_bytes()).unwrap();
        assert!(!file.slices[0].available());
        assert_eq!(file.slices[0].name, "Jazz");
        assert!(file.slices[0].published);
        // An unknown field is rejected too.
        assert!(!file.slices[1].available());
        let written: Value = serde_json::from_slice(&file.to_bytes().unwrap()).unwrap();
        let original: Value = serde_json::from_str(json).unwrap();
        assert_eq!(written["slices"], original["slices"]);
    }

    #[test]
    fn a_newer_format_makes_every_slice_unavailable_and_read_only() {
        let json = r#"{"formatVersion": 2, "slices": [
          {"id": "6b0e0000-0000-4000-8000-000000000000", "name": "All", "kind": "mixed",
           "published": true, "definition": {"type": "all"}}]}"#;
        let mut file = SliceFile::parse(json.as_bytes()).unwrap();
        assert!(!file.slices[0].available());
        assert!(!file.writable());
        assert!(file.remove(file.slices[0].id).is_err());
    }

    #[test]
    fn companions_follow_the_naming_rules() {
        assert!(is_companion("Movie.mkv", "Movie.srt"));
        assert!(is_companion("Movie.mkv", "Movie.en.forced.srt"));
        assert!(is_companion("Movie.mkv", "Movie.idx"));
        assert!(is_companion("Movie.mkv", "Movie.sub"));
        assert!(is_companion("Movie.mkv", "Movie.EN.SRT"));
        assert!(is_companion("song.flac", "song.lrc"));
        assert!(is_companion("song.flac", "Cover.JPG"));
        assert!(is_companion("song.flac", "folder.webp"));
        assert!(!is_companion("Movie.mkv", "Movie.notes.txt"));
        assert!(!is_companion("Movie.mkv", "Movie.original.mkv"));
        assert!(!is_companion("Movie.mkv", "Movie2.srt"));
        assert!(!is_companion("Movie.mkv", "Other.srt"));
        assert!(!is_companion("song.flac", "back.jpg"));
    }

    #[test]
    fn evaluates_selections_with_companions() {
        let paths = [
            "Jazz/a.flac",
            "Jazz/sub/b.flac",
            "Jazzy/c.flac",
            "Misc/cover.jpg",
            "Misc/other.flac",
            "Misc/song.flac",
            "Misc/song.lrc",
            "Misc/sub/song.lrc",
            "top.flac",
        ];
        let expr = SliceExpr::Selection {
            folders: vec![TreePath {
                folder: folder(),
                path: vec!["Jazz".into()],
            }],
            files: vec![
                TreePath {
                    folder: folder(),
                    path: vec!["Misc".into(), "song.flac".into()],
                },
                TreePath {
                    folder: folder(),
                    path: vec!["Missing".into(), "x.flac".into()],
                },
            ],
        };
        assert_eq!(
            evaluate(&expr, folder(), &paths),
            vec![
                "Jazz/a.flac",
                "Jazz/sub/b.flac",
                "Misc/cover.jpg",
                "Misc/song.flac",
                "Misc/song.lrc"
            ]
        );
        // Another shared folder's selections don't apply.
        assert!(evaluate(&expr, Uuid::new_v4(), &paths).is_empty());
    }

    #[test]
    fn selecting_a_whole_shared_folder_takes_everything() {
        let paths = ["a.flac", "b/c.flac"];
        let expr = SliceExpr::Selection {
            folders: vec![TreePath {
                folder: folder(),
                path: vec![],
            }],
            files: vec![],
        };
        assert_eq!(evaluate(&expr, folder(), &paths), paths.to_vec());
    }

    #[test]
    fn migrates_sync_presets() {
        let presets = [
            SyncPreset {
                name: "Road Trip".into(),
                entries: vec!["Jazz/".into(), "Misc/song.flac".into()],
                active: true,
            },
            SyncPreset {
                name: "Empty".into(),
                entries: vec![],
                active: false,
            },
        ];
        let slices = migrate_presets(folder(), &presets);
        assert!(slices[0].published);
        assert_eq!(
            slices[0].definition,
            SliceExpr::Selection {
                folders: vec![TreePath {
                    folder: folder(),
                    path: vec!["Jazz".into()]
                }],
                files: vec![TreePath {
                    folder: folder(),
                    path: vec!["Misc".into(), "song.flac".into()]
                }],
            }
        );
        assert!(!slices[1].published);
        assert_eq!(slices[1].definition, SliceExpr::All);
    }
}
