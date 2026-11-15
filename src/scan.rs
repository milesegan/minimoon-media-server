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
    /// Folders or files that couldn't be listed or stat-ed, so treated as not there. Kept only
    /// to report.
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

/// Walks a shared folder. Symbolic links are never followed, wherever they point (spec §9): a
/// link is skipped like a hidden file, so nothing outside the folder can be published through
/// one. To share a drive or another folder, the user adds it as a shared folder of its own.
pub fn scan(root: &Path) -> ScanResult {
    let mut result = ScanResult::default();
    let mut visited = HashSet::new();
    if let Some(key) = dir_key(root) {
        visited.insert(key);
    }
    let mut walker = Walker {
        visited,
        result: &mut result,
    };
    walker.walk(root, "");
    result.files.sort_by(|a, b| a.path.cmp(&b.path));
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
    /// Folders already walked. Without links a folder can still be reached twice through a
    /// bind mount or a hard-linked folder; this keeps such a loop from recursing forever.
    #[cfg(unix)]
    visited: HashSet<(u64, u64)>,
    #[cfg(not(unix))]
    visited: HashSet<PathBuf>,
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
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    self.error(&child_relative, error);
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                tracing::debug!("not following the link {path:?}: links are never followed");
                continue;
            }
            if has_hidden_attribute(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                let Some(key) = dir_key(&path) else {
                    self.error(&child_relative, "can't identify folder");
                    continue;
                };
                if !self.visited.insert(key) {
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
}

/// Checks an index path's components: no empty, `.`, `..` or hidden part (spec §9).
fn valid_components(relative: &str) -> Option<std::str::Split<'_, char>> {
    if relative.is_empty() {
        return None;
    }
    for component in relative.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || is_hidden_name(component)
            || (cfg!(windows) && component.contains('\\'))
        {
            return None;
        }
    }
    Some(relative.split('/'))
}

/// Resolves an index path inside a shared folder to a file on disk, refusing traversal, hidden
/// names, and any part that's a symbolic link (spec §9). Returns `None` for paths that don't
/// resolve to a regular file. Content is opened with [`open_inside`], which also refuses links
/// at the moment it opens, so a link swapped in after this check isn't followed (on Unix).
pub fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for component in valid_components(relative)? {
        path.push(component);
        if std::fs::symlink_metadata(&path)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    std::fs::symlink_metadata(&path)
        .ok()?
        .is_file()
        .then_some(path)
}

/// Opens a file inside a shared folder without following a symbolic link anywhere below the
/// folder. On Unix each part is opened relative to the last with `O_NOFOLLOW`, so the check and
/// the open are one step and a link swapped in meanwhile makes the open fail. Elsewhere it's
/// [`resolve`] followed by an open, which leaves a short window for someone who can write in
/// the folder.
#[cfg(unix)]
pub fn open_inside(root: &Path, relative: &str) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let not_found = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let c_string = |bytes: &[u8]| CString::new(bytes).map_err(|_| not_found());
    let components: Vec<&str> = valid_components(relative).ok_or_else(not_found)?.collect();
    let root = c_string(root.as_os_str().as_bytes())?;
    // SAFETY: a valid C string; the descriptor is owned immediately below.
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fd is a freshly opened descriptor that nothing else owns.
    let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
    for (n, component) in components.iter().enumerate() {
        let last = n + 1 == components.len();
        let name = c_string(component.as_bytes())?;
        // O_NONBLOCK so a FIFO in the folder can't stall the open; it doesn't affect reading a
        // regular file, and anything else is refused below.
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if last {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: dir is an open directory and name a valid C string.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: as above.
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if last {
            let file = std::fs::File::from(opened);
            if !file.metadata()?.is_file() {
                return Err(not_found());
            }
            return Ok(file);
        }
        dir = opened;
    }
    Err(not_found())
}

#[cfg(not(unix))]
pub fn open_inside(root: &Path, relative: &str) -> std::io::Result<std::fs::File> {
    let path = resolve(root, relative).ok_or(std::io::ErrorKind::NotFound)?;
    std::fs::File::open(path)
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
        let result = scan(root);
        assert!(result.errors.is_empty());
        let paths: Vec<_> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.flac", "b/2.flac"]);
        assert_eq!(result.files[1].size, 2);
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_links_inside_or_outside_the_folder() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.flac"), b"s").unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/x.flac"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real/x.flac"), dir.path().join("y.flac"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        // A link into a drive that isn't mounted is skipped too, not a scan error: nothing
        // behind a link is ever published, so nothing behind one can look deleted.
        std::os::unix::fs::symlink("/Volumes/Not Mounted/Jazz", dir.path().join("Jazz")).unwrap();
        let result = scan(dir.path());
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let paths: Vec<_> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["real/x.flac"]);
        for path in ["out/secret.flac", "alias/x.flac", "y.flac"] {
            assert!(resolve(dir.path(), path).is_none(), "{path}");
            assert!(open_inside(dir.path(), path).is_err(), "{path}");
        }
        assert!(resolve(dir.path(), "real/x.flac").is_some());
        let mut contents = String::new();
        std::io::Read::read_to_string(
            &mut open_inside(dir.path(), "real/x.flac").unwrap(),
            &mut contents,
        )
        .unwrap();
        assert_eq!(contents, "x");
    }

    #[cfg(unix)]
    #[test]
    fn open_inside_refuses_a_link_swapped_in_after_resolving() {
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("real")).unwrap();
        fs::write(outside.path().join("real/x.flac"), b"secret").unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        fs::write(dir.path().join("real/x.flac"), b"x").unwrap();
        assert!(resolve(dir.path(), "real/x.flac").is_some());
        // Between the check and the open, the folder is replaced with a link out of it.
        fs::remove_dir_all(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("real"), dir.path().join("real")).unwrap();
        assert!(open_inside(dir.path(), "real/x.flac").is_err());
    }

    #[test]
    fn resolve_rejects_traversal_and_hidden_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.flac"), b"x").unwrap();
        fs::write(dir.path().join(".h.flac"), b"x").unwrap();
        assert!(resolve(dir.path(), "a.flac").is_some());
        assert!(open_inside(dir.path(), "a.flac").is_ok());
        for path in ["../a.flac", ".h.flac", "", "a.flac/", "./a.flac"] {
            assert!(resolve(dir.path(), path).is_none(), "{path:?}");
            assert!(open_inside(dir.path(), path).is_err(), "{path:?}");
        }
    }
}
