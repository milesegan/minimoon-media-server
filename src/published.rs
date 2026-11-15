//! What the HTTP routes serve: an immutable view of the server's state, rebuilt whenever the
//! index or the shared folders change (spec §4, §5).
//!
//! The index of every shared folder is built here in both views with its weak ETag, so
//! `/v1/server` can advertise the ETags and `/v1/index` can answer `304` without touching the
//! database. Item routes look items up in [`Published::items`], which holds every indexed item:
//! anything else is answered as if it didn't exist (spec §8, enforcement).

use crate::ids::{folder_id, item_id, weak_etag};
use crate::index::{FileRecord, Snapshot};
use crate::reader::{file_type, Reader};
use crate::settings::{AuthMode, PairedDevice, Settings, SharedFolderSettings};
use bytes::Bytes;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

/// One representation of the index: its JSON body and weak ETag.
#[derive(Debug)]
pub struct Representation {
    pub etag: String,
    pub plain: Bytes,
    gzip: OnceLock<Bytes>,
}

impl Representation {
    fn new(plain: Vec<u8>) -> Self {
        Self {
            etag: weak_etag(&plain),
            plain: Bytes::from(plain),
            gzip: OnceLock::new(),
        }
    }

    /// The body gzipped, compressed on first use and kept.
    pub fn gzipped(&self) -> Bytes {
        self.gzip
            .get_or_init(|| {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
                encoder.write_all(&self.plain).expect("writing to memory");
                Bytes::from(encoder.finish().expect("writing to memory"))
            })
            .clone()
    }
}

/// The index in both views.
#[derive(Debug)]
pub struct IndexViews {
    pub full: Representation,
    pub files: Representation,
}

/// A shared folder as the routes see it.
#[derive(Clone, Debug)]
pub struct PublishedFolder {
    pub id: Uuid,
    pub name: String,
    /// Its volume is mounted and it has a published snapshot.
    pub available: bool,
    /// The folder on disk, as configured.
    pub root: PathBuf,
}

/// An item that may be served.
#[derive(Clone, Debug)]
pub struct ServedItem {
    pub folder: Uuid,
    pub path: Arc<str>,
    pub reader: Option<Reader>,
    pub mime: Option<&'static str>,
    pub rev: u64,
}

/// What the pairing routes check.
#[derive(Clone, Debug, Default)]
pub struct AuthState {
    pub mode: AuthMode,
    pub devices: Vec<PairedDevice>,
    pub signing_key: Option<Vec<u8>>,
}

/// Everything the HTTP routes serve.
#[derive(Clone, Debug)]
pub struct Published {
    pub server_id: Uuid,
    pub name: String,
    pub folders: Arc<Vec<PublishedFolder>>,
    pub index: Arc<IndexViews>,
    pub items: Arc<HashMap<String, ServedItem>>,
    pub auth: Arc<AuthState>,
}

impl Published {
    /// Nothing published yet.
    pub fn empty(settings: &Settings) -> Self {
        Self {
            server_id: settings.server_id,
            name: settings.display_name(),
            folders: Arc::new(Vec::new()),
            index: Arc::new(build_index(settings, &[], &mut ItemCache::default()).0),
            items: Arc::new(HashMap::new()),
            auth: Arc::new(auth_state(settings)),
        }
    }

    pub fn folder(&self, id: Uuid) -> Option<&PublishedFolder> {
        self.folders.iter().find(|folder| folder.id == id)
    }
}

/// The pairing state from the settings.
pub fn auth_state(settings: &Settings) -> AuthState {
    use base64::Engine;
    AuthState {
        mode: settings.auth,
        devices: settings.pairing.devices.clone(),
        signing_key: settings.pairing.signing_key.as_deref().and_then(|key| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(key)
                .ok()
        }),
    }
}

/// One shared folder's state, as the builder needs it.
pub struct FolderView<'a> {
    pub settings: &'a SharedFolderSettings,
    pub snapshot: Option<&'a Snapshot>,
    /// Its volume check passed.
    pub volume_ok: bool,
}

impl FolderView<'_> {
    /// Available to clients: mounted, and published at least once.
    pub fn available(&self) -> bool {
        self.volume_ok && self.snapshot.is_some()
    }
}

/// An item's JSON in both views, kept between builds while the record and `rev` don't change.
struct CachedItem {
    size: u64,
    mtime_ns: i64,
    rev: u64,
    meta: Option<Arc<str>>,
    full: Arc<str>,
    files: Arc<str>,
}

/// Keeps item JSON between builds, so a rebuild after a small change mostly concatenates.
#[derive(Default)]
pub struct ItemCache {
    items: HashMap<(Uuid, String), CachedItem>,
}

impl ItemCache {
    /// Drops entries for folders that no longer exist and paths no longer indexed.
    fn retain(&mut self, live: &HashSet<(Uuid, String)>) {
        self.items.retain(|key, _| live.contains(key));
    }

    fn get(&mut self, folder: Uuid, path: &str, record: &FileRecord, rev: u64) -> &CachedItem {
        let key = (folder, path.to_string());
        let fresh = self.items.get(&key).is_some_and(|cached| {
            cached.size == record.size
                && cached.mtime_ns == record.mtime_ns
                && cached.rev == rev
                && match (&cached.meta, &record.meta) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                    (None, None) => true,
                    _ => false,
                }
        });
        if !fresh {
            let (full, files) = item_json(folder, path, record, rev);
            self.items.insert(
                key.clone(),
                CachedItem {
                    size: record.size,
                    mtime_ns: record.mtime_ns,
                    rev,
                    meta: record.meta.clone(),
                    full: Arc::from(full),
                    files: Arc::from(files),
                },
            );
        }
        &self.items[&key]
    }
}

/// Splits an index path into its folder part and file name.
fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(slash) => (&path[..slash], &path[slash + 1..]),
        None => ("", path),
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

/// An item's JSON in the full view and in the file list.
fn item_json(folder: Uuid, path: &str, record: &FileRecord, rev: u64) -> (String, String) {
    let id = item_id(folder, path);
    let (dir, name) = split_path(path);
    let file_type = file_type(name);
    let mtime = crate::scan::ns_to_ms(record.mtime_ns);
    let mut full = format!(
        "{{\"id\":{},\"sharedFolderId\":\"{folder}\",\"parentId\":{},\"name\":{},\"path\":{},\"kind\":\"{}\"",
        json_string(&id),
        json_string(&folder_id(folder, dir)),
        json_string(name),
        json_string(path),
        file_type.kind.as_str(),
    );
    if let Some(mime) = file_type.mime {
        full.push_str(&format!(",\"mime\":{}", json_string(mime)));
    }
    full.push_str(&format!(
        ",\"size\":{},\"mtime\":{mtime},\"rev\":{rev}",
        record.size
    ));
    if let Some(meta) = record.meta.as_deref() {
        // `meta` is a JSON object; merge its members into the item.
        let inner = meta
            .trim()
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .unwrap_or("")
            .trim();
        if !inner.is_empty() {
            full.push(',');
            full.push_str(inner);
        }
    }
    full.push('}');
    let files = format!(
        "{{\"id\":{},\"sharedFolderId\":\"{folder}\",\"path\":{},\"size\":{},\"mtime\":{mtime},\"rev\":{rev}}}",
        json_string(&id),
        json_string(path),
        record.size,
    );
    (full, files)
}

/// Builds what the routes serve from the settings and every folder's snapshot.
pub fn build(settings: &Settings, folders: &[FolderView<'_>], cache: &mut ItemCache) -> Published {
    let published_folders: Vec<PublishedFolder> = folders
        .iter()
        .map(|view| PublishedFolder {
            id: view.settings.id,
            name: view.settings.name.clone(),
            available: view.available(),
            root: view.settings.path.clone(),
        })
        .collect();

    let mut live = HashSet::new();
    for view in folders {
        if let Some(snapshot) = view.snapshot {
            for path in snapshot.keys() {
                live.insert((view.settings.id, path.clone()));
            }
        }
    }
    cache.retain(&live);

    let (index, items) = build_index(settings, folders, cache);
    Published {
        server_id: settings.server_id,
        name: settings.display_name(),
        folders: Arc::new(published_folders),
        index: Arc::new(index),
        items: Arc::new(items),
        auth: Arc::new(auth_state(settings)),
    }
}

/// The index of every shared folder in both views, and the items it lists by id.
fn build_index(
    settings: &Settings,
    folders: &[FolderView<'_>],
    cache: &mut ItemCache,
) -> (IndexViews, HashMap<String, ServedItem>) {
    let mut folder_json: Vec<String> = Vec::new();
    let mut full_items: Vec<Arc<str>> = Vec::new();
    let mut file_items: Vec<Arc<str>> = Vec::new();
    let mut offline: Vec<String> = Vec::new();
    let mut served: HashMap<String, ServedItem> = HashMap::new();

    for view in folders {
        let folder = view.settings.id;
        if !view.available() {
            offline.push(folder.to_string());
        }
        let Some(snapshot) = view.snapshot else {
            continue;
        };
        if snapshot.is_empty() {
            continue;
        }
        // Every ancestor folder of every item, up to and including the shared folder.
        let mut dirs: BTreeSet<&str> = BTreeSet::new();
        dirs.insert("");
        for path in snapshot.keys() {
            let mut dir = split_path(path).0;
            while !dir.is_empty() && dirs.insert(dir) {
                dir = split_path(dir).0;
            }
        }
        for dir in dirs {
            let mut json = format!(
                "{{\"id\":{},\"sharedFolderId\":\"{folder}\"",
                json_string(&folder_id(folder, dir))
            );
            if dir.is_empty() {
                json.push_str(&format!(
                    ",\"name\":{},\"path\":\"\"}}",
                    json_string(&view.settings.name)
                ));
            } else {
                let (parent, name) = split_path(dir);
                json.push_str(&format!(
                    ",\"parentId\":{},\"name\":{},\"path\":{}}}",
                    json_string(&folder_id(folder, parent)),
                    json_string(name),
                    json_string(dir)
                ));
            }
            folder_json.push(json);
        }
        for (path, record) in snapshot {
            let rev = settings.rev(folder, path);
            let cached = cache.get(folder, path, record, rev);
            full_items.push(cached.full.clone());
            file_items.push(cached.files.clone());
            let file_type = file_type(split_path(path).1);
            served.insert(
                item_id(folder, path),
                ServedItem {
                    folder,
                    path: Arc::from(path.as_str()),
                    reader: file_type.reader,
                    mime: file_type.mime,
                    rev,
                },
            );
        }
    }

    let offline_json = format!(
        "[{}]",
        offline
            .iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let full = join_body(
        &[
            ("folders", &folder_json_refs(&folder_json)),
            ("items", &full_items),
        ],
        &offline_json,
    );
    let files = join_body(&[("items", &file_items)], &offline_json);
    let views = IndexViews {
        full: Representation::new(full),
        files: Representation::new(files),
    };
    (views, served)
}

fn folder_json_refs(folders: &[String]) -> Vec<Arc<str>> {
    folders
        .iter()
        .map(|json| Arc::from(json.as_str()))
        .collect()
}

fn join_body(arrays: &[(&str, &Vec<Arc<str>>)], offline: &str) -> Vec<u8> {
    let size: usize = arrays
        .iter()
        .map(|(_, values)| values.iter().map(|v| v.len() + 1).sum::<usize>())
        .sum();
    let mut body = Vec::with_capacity(size + offline.len() + 64);
    body.push(b'{');
    for (name, values) in arrays {
        body.extend_from_slice(format!("\"{name}\":[").as_bytes());
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                body.push(b',');
            }
            body.extend_from_slice(value.as_bytes());
        }
        body.extend_from_slice(b"],");
    }
    body.extend_from_slice(format!("\"offline\":{offline}}}").as_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::ReadStatus;
    use crate::volume::VolumeIdentity;
    use serde_json::Value;

    fn folder_settings(name: &str) -> SharedFolderSettings {
        SharedFolderSettings {
            id: Uuid::new_v4(),
            path: PathBuf::from("/music"),
            name: name.into(),
            volume: VolumeIdentity::default(),
        }
    }

    fn record(meta: Option<&str>) -> FileRecord {
        FileRecord {
            size: 10,
            mtime_ns: 1_710_000_000_123_999_999,
            ctime_ns: None,
            reader_version: 1,
            status: ReadStatus::Read,
            meta: meta.map(Arc::from),
        }
    }

    #[test]
    fn builds_both_views_with_folders_and_offline() {
        let mut settings = Settings::new();
        let music = folder_settings("Music");
        let movies = folder_settings("Movies");
        settings.shared_folders = vec![music.clone(), movies.clone()];
        settings.bump_rev(music.id, "Jazz/b.flac");
        let mut snapshot = Snapshot::new();
        snapshot.insert(
            "Jazz/a.flac".into(),
            record(Some(
                r#"{"audio":{"channels":2},"tags":{"compilation":false}}"#,
            )),
        );
        snapshot.insert("Jazz/b.flac".into(), record(None));
        snapshot.insert("Rock/c.flac".into(), record(None));
        let views = [
            FolderView {
                settings: &music,
                snapshot: Some(&snapshot),
                volume_ok: true,
            },
            FolderView {
                settings: &movies,
                snapshot: None,
                volume_ok: true,
            },
        ];
        let published = build(&settings, &views, &mut ItemCache::default());

        let body: Value = serde_json::from_slice(&published.index.full.plain).unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 3);
        // The root, Jazz and Rock.
        assert_eq!(body["folders"].as_array().unwrap().len(), 3);
        // Movies has never been scanned: it's offline with no items.
        assert_eq!(body["offline"], serde_json::json!([movies.id.to_string()]));
        let a = &body["items"][0];
        assert_eq!(a["kind"], "audio");
        assert_eq!(a["mtime"], 1_710_000_000_123i64);
        assert_eq!(a["audio"]["channels"], 2);
        assert_eq!(a["rev"], 0);
        assert_eq!(body["items"][1]["rev"], 1);
        let root = body["folders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "")
            .unwrap();
        assert_eq!(root["name"], "Music");
        assert!(root.get("parentId").is_none());
        assert_eq!(a["parentId"], body["folders"][1]["id"]);

        let files: Value = serde_json::from_slice(&published.index.files.plain).unwrap();
        assert!(files.get("folders").is_none());
        assert_eq!(
            files["items"][0].as_object().unwrap().len(),
            6,
            "id, sharedFolderId, path, size, mtime, rev"
        );
        assert_eq!(files["offline"], body["offline"]);

        // Every indexed item is served.
        assert_eq!(published.items.len(), 3);
        assert_ne!(published.index.full.etag, published.index.files.etag);
    }

    #[test]
    fn etags_follow_content_but_metadata_spares_the_file_list() {
        let mut settings = Settings::new();
        let music = folder_settings("Music");
        settings.shared_folders = vec![music.clone()];
        let build_with = |meta: Option<&str>, cache: &mut ItemCache| {
            let mut snapshot = Snapshot::new();
            snapshot.insert("a.flac".into(), record(meta));
            let views = [FolderView {
                settings: &music,
                snapshot: Some(&snapshot),
                volume_ok: true,
            }];
            let published = build(&settings, &views, cache);
            (
                published.index.full.etag.clone(),
                published.index.files.etag.clone(),
            )
        };
        let mut cache = ItemCache::default();
        let (full_a, files_a) = build_with(None, &mut cache);
        let (full_b, files_b) = build_with(Some(r#"{"tags":{"title":"x"}}"#), &mut cache);
        assert_ne!(full_a, full_b);
        assert_eq!(files_a, files_b);
        let (full_c, _) = build_with(Some(r#"{"tags":{"title":"x"}}"#), &mut cache);
        assert_eq!(full_b, full_c);
    }

    #[test]
    fn an_empty_server_serves_an_empty_index() {
        let settings = Settings::new();
        let published = Published::empty(&settings);
        let body: Value = serde_json::from_slice(&published.index.full.plain).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"folders": [], "items": [], "offline": []})
        );
        assert!(published.items.is_empty());
    }
}
