//! Walking a shared folder: the file list a scan publishes (spec §5, "Scans never publish a
//! guess", and §9 on hidden files and symlinks).
//!
//! A scan reads only names, sizes and modification dates. Metadata is read afterwards, and a
//! failure to read it is not a scan error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// One file found by a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedFile {
    /// Relative to the shared folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch, at full precision.
    pub mtime_ns: i64,
    /// Status-change time in nanoseconds (Unix only). Unlike the mtime, no program can set it
    /// back, so it tells a fresh write event from a repeated one (see [`ctime_ns`]).
    pub ctime_ns: Option<i64>,
}

/// What a scan found.
#[derive(Debug, Default)]
pub struct ScanResult {
    /// Every file, sorted by path.
    pub files: Vec<ScannedFile>,
    /// Folders or files that couldn't be listed or stat-ed. Any error means the scan publishes
    /// nothing.
    pub errors: Vec<ScanError>,
    /// Names skipped because they aren't valid UTF-8.
    pub skipped_non_utf8: Vec<PathBuf>,
}

/// A failure to list a folder or to read a file's size and modification date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    /// Relative to the shared folder, `/`-separated; empty for the folder itself.
    pub path: String,
    pub message: String,
}

/// How a scan treats symbolic links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanOptions {
    /// Follow links that point outside the shared folder. Links inside it are always followed.
    pub follow_external_symlinks: bool,
}

/// Whether a file or folder name is hidden from clients: dot files, and the metadata files
/// Windows and macOS leave in media folders.
pub fn is_hidden_name(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower == "desktop.ini"
        || lower == "thumbs.db"
        || lower == "$recycle.bin"
        || lower == "system volume information"
        || lower.starts_with("albumartsmall")
        || lower.starts_with("albumart_{")
}

#[cfg(windows)]
fn has_hidden_attribute(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    metadata.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(not(windows))]
fn has_hidden_attribute(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Modification time in nanoseconds since the Unix epoch.
pub fn mtime_ns(metadata: &std::fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    Some(match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_nanos()).unwrap_or(i64::MAX),
    })
}

/// Status-change time in nanoseconds since the Unix epoch.
///
/// Any write, and any change to a file's attributes, advances it, and it can't be set to an
/// earlier value. FSEvents can report the same file's `ItemModified` flag again in later events,
/// so a write event counts only when the ctime moved too.
#[cfg(unix)]
pub fn ctime_ns(metadata: &std::fs::Metadata) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    metadata
        .ctime()
        .checked_mul(1_000_000_000)?
        .checked_add(metadata.ctime_nsec())
}

#[cfg(not(unix))]
pub fn ctime_ns(_metadata: &std::fs::Metadata) -> Option<i64> {
    None
}

/// The file name part of an index path.
pub fn split_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Milliseconds since the epoch, as the index and content ETags carry it.
pub fn ns_to_ms(ns: i64) -> i64 {
    ns.div_euclid(1_000_000)
}

/// Walks a shared folder. `excluded` subfolders (path components) are skipped entirely.
pub fn scan(root: &Path, options: ScanOptions, excluded: &[Vec<String>]) -> ScanResult {
    let mut result = ScanResult::default();
    let canonical_root = match root.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            result.errors.push(ScanError {
                path: String::new(),
                message: error.to_string(),
            });
            return result;
        }
    };
    let excluded: HashSet<String> = excluded.iter().map(|path| path.join("/")).collect();
    let mut visited = HashSet::new();
    if let Some(key) = dir_key(&canonical_root) {
        visited.insert(key);
    }
    let mut walker = Walker {
        canonical_root: &canonical_root,
        options,
        excluded: &excluded,
        visited,
        deferred: Vec::new(),
        result: &mut result,
    };
    walker.walk(root, "");
    // Linked folders are walked after every real one, so a link never claims a folder that's
    // also reachable directly, whatever order the folders are listed in.
    while let Some((dir, relative)) = walker.deferred.pop() {
        walker.walk_linked(&dir, &relative);
    }
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
    result.files.dedup_by(|a, b| a.path == b.path);
    result
}

#[cfg(unix)]
fn dir_key(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn dir_key(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

struct Walker<'a> {
    canonical_root: &'a Path,
    options: ScanOptions,
    excluded: &'a HashSet<String>,
    #[cfg(unix)]
    visited: HashSet<(u64, u64)>,
    #[cfg(not(unix))]
    visited: HashSet<PathBuf>,
    /// Linked folders to walk once the real ones are done.
    deferred: Vec<(PathBuf, String)>,
    result: &'a mut ScanResult,
}

impl Walker<'_> {
    fn error(&mut self, path: &str, error: impl ToString) {
        self.result.errors.push(ScanError {
            path: path.to_string(),
            message: error.to_string(),
        });
    }

    fn walk(&mut self, dir: &Path, relative: &str) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // A folder that vanished between being listed and being read is gone, not an error.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !relative.is_empty() => {
                return
            }
            Err(error) => return self.error(relative, error),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.error(relative, error);
                    continue;
                }
            };
            let os_name = entry.file_name();
            let Some(name) = os_name.to_str() else {
                tracing::warn!("skipping a name that isn't UTF-8: {:?}", entry.path());
                self.result.skipped_non_utf8.push(entry.path());
                continue;
            };
            if is_hidden_name(name) {
                continue;
            }
            let child_relative = if relative.is_empty() {
                name.to_string()
            } else {
                format!("{relative}/{name}")
            };
            if self.excluded.contains(&child_relative) {
                continue;
            }
            let path = entry.path();
            let link_metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    self.error(&child_relative, error);
                    continue;
                }
            };
            let is_link = link_metadata.file_type().is_symlink();
            let metadata = if is_link {
                match self.follow(&path, &child_relative) {
                    Some(metadata) => metadata,
                    None => continue,
                }
            } else {
                link_metadata
            };
            if has_hidden_attribute(&metadata) {
                continue;
            }
            if metadata.is_dir() && is_link {
                self.deferred.push((path, child_relative));
            } else if metadata.is_dir() {
                let Some(key) = dir_key(&path) else {
                    self.error(&child_relative, "can't identify folder");
                    continue;
                };
                if !self.visited.insert(key) {
                    // A link back into a folder already walked: skip the loop.
                    continue;
                }
                self.walk(&path, &child_relative);
            } else if metadata.is_file() {
                let Some(mtime_ns) = mtime_ns(&metadata) else {
                    self.error(&child_relative, "no modification date");
                    continue;
                };
                self.result.files.push(ScannedFile {
                    path: child_relative,
                    size: metadata.len(),
                    mtime_ns,
                    ctime_ns: ctime_ns(&metadata),
                });
            }
        }
    }

    /// Walks a linked folder unless its target was already walked.
    fn walk_linked(&mut self, dir: &Path, relative: &str) {
        let Some(key) = dir_key(dir) else {
            self.error(relative, "can't open the linked folder");
            return;
        };
        if self.visited.insert(key) {
            self.walk(dir, relative);
        }
    }

    /// The metadata of a link's target, when the link may be followed.
    ///
    /// A link whose target can't be reached is a scan error, not a skipped entry: a link into a
    /// drive that's been unplugged must not make its files look deleted. Only a link that's
    /// plainly dangling (its target's folder exists but the target doesn't) is skipped.
    fn follow(&mut self, link: &Path, relative: &str) -> Option<std::fs::Metadata> {
        let target = match link.canonicalize() {
            Ok(target) => target,
            Err(error) => {
                if !is_dangling(link, &error) {
                    self.error(relative, format!("can't follow the link: {error}"));
                }
                return None;
            }
        };
        if !target.starts_with(self.canonical_root) && !self.options.follow_external_symlinks {
            tracing::debug!("not following {link:?}: it points outside the shared folder");
            return None;
        }
        match std::fs::metadata(&target) {
            Ok(metadata) => Some(metadata),
            Err(error) => {
                self.error(relative, format!("can't follow the link: {error}"));
                None
            }
        }
    }
}

/// Whether a link that failed to resolve is dangling: its target is missing but the folder its
/// target would be in exists. A missing folder may be an unmounted volume, so that isn't
/// dangling.
fn is_dangling(link: &Path, error: &std::io::Error) -> bool {
    if error.kind() != std::io::ErrorKind::NotFound {
        return false;
    }
    let Ok(target) = std::fs::read_link(link) else {
        return false;
    };
    let target = if target.is_absolute() {
        target
    } else {
        link.parent().map(|dir| dir.join(&target)).unwrap_or(target)
    };
    target.parent().is_some_and(|dir| dir.is_dir())
}

/// Resolves an index path inside a shared folder to a file on disk, refusing anything that
/// leaves the folder (spec §9). Returns `None` for paths that don't resolve to a file.
pub fn resolve(root: &Path, relative: &str, options: ScanOptions) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || is_hidden_name(component)
        {
            return None;
        }
        if component.contains('\\') && cfg!(windows) {
            return None;
        }
        path.push(component);
    }
    let canonical = path.canonicalize().ok()?;
    if !options.follow_external_symlinks {
        let canonical_root = root.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
    }
    canonical.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lists_visible_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("b/.hidden")).unwrap();
        fs::write(root.join("b/2.flac"), b"22").unwrap();
        fs::write(root.join("a.flac"), b"1").unwrap();
        fs::write(root.join(".DS_Store"), b"x").unwrap();
        fs::write(root.join("desktop.ini"), b"x").unwrap();
        fs::write(root.join("b/.hidden/x.flac"), b"x").unwrap();
        let result = scan(root, ScanOptions::default(), &[]);
        assert!(result.errors.is_empty());
        let paths: Vec<_> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.flac", "b/2.flac"]);
        assert_eq!(result.files[1].size, 2);
    }

    #[test]
    fn skips_excluded_folders() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("a/b/x.flac"), b"x").unwrap();
        fs::write(dir.path().join("a/y.flac"), b"y").unwrap();
        let result = scan(
            dir.path(),
            ScanOptions::default(),
            &[vec!["a".into(), "b".into()]],
        );
        let paths: Vec<_> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a/y.flac"]);
    }

    #[cfg(unix)]
    #[test]
    fn follows_internal_links_but_not_external_ones() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.flac"), b"s").unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/x.flac"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        // A loop back to the root is skipped.
        std::os::unix::fs::symlink(dir.path(), dir.path().join("real/loop")).unwrap();
        let result = scan(dir.path(), ScanOptions::default(), &[]);
        let paths: Vec<_> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["real/x.flac"]);
        assert!(resolve(dir.path(), "out/secret.flac", ScanOptions::default()).is_none());
        let external = ScanOptions {
            follow_external_symlinks: true,
        };
        let result = scan(dir.path(), external, &[]);
        assert!(result.files.iter().any(|f| f.path == "out/secret.flac"));
        assert!(resolve(dir.path(), "out/secret.flac", external).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_a_missing_volume_is_a_scan_error_but_a_dangling_one_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("x.flac"), b"x").unwrap();
        std::os::unix::fs::symlink("/Volumes/Not Mounted/Jazz", dir.path().join("Jazz")).unwrap();
        let external = ScanOptions {
            follow_external_symlinks: true,
        };
        let result = scan(dir.path(), external, &[]);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].path, "Jazz");

        fs::remove_file(dir.path().join("Jazz")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("deleted.flac"), dir.path().join("old.flac"))
            .unwrap();
        let result = scan(dir.path(), external, &[]);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.files.len(), 1);
    }

    #[test]
    fn resolve_rejects_traversal_and_hidden_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.flac"), b"x").unwrap();
        fs::write(dir.path().join(".h.flac"), b"x").unwrap();
        let options = ScanOptions::default();
        assert!(resolve(dir.path(), "a.flac", options).is_some());
        assert!(resolve(dir.path(), "../a.flac", options).is_none());
        assert!(resolve(dir.path(), ".h.flac", options).is_none());
        assert!(resolve(dir.path(), "", options).is_none());
    }
}
