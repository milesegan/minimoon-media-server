//! The library's scan-safety and `rev` rules, driven through its public API.

use minimoon_media_server::library::{Library, LibraryOptions, ServerEvent};
use minimoon_media_server::settings::Settings;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(20);

fn options(data: &Path) -> LibraryOptions {
    let mut options = LibraryOptions::new(data);
    options.workers = 2;
    options
}

fn index(library: &Library) -> Value {
    let published = library.published();
    let slice = &published.slices[0];
    serde_json::from_slice(&slice.index.as_ref().unwrap().full.plain).unwrap()
}

fn etags(library: &Library) -> (String, String) {
    let published = library.published();
    let index = published.slices[0].index.clone().unwrap();
    (index.full.etag.clone(), index.files.etag.clone())
}

fn item<'a>(index: &'a Value, path: &str) -> Option<&'a Value> {
    index["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == path)
}

/// Waits until `check` passes, letting the watcher and scans run.
fn eventually(library: &Library, what: &str, check: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        library.wait_until_idle(WAIT);
        let index = index(library);
        if check(&index) {
            return index;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {index}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir, Library, Uuid) {
    // Not the system temp folder: FSEvents doesn't keep history for it, which the replay test
    // needs.
    let media = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::create_dir_all(media.path().join("Album")).unwrap();
    fs::write(media.path().join("Album/01.flac"), b"not really flac").unwrap();
    fs::write(media.path().join("Album/cover.jpg"), b"jpeg").unwrap();
    let library = Library::start(options(data.path())).unwrap();
    library.ensure_default_slice().unwrap();
    let folder = library.add_shared_folder(media.path(), None).unwrap();
    (media, data, library, folder)
}

#[test]
fn publishes_a_folder_once_its_first_scan_completes() {
    let (_media, _data, library, folder) = setup();
    let index = eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    assert_eq!(index["offline"], serde_json::json!([]));
    let flac = item(&index, "Album/01.flac").unwrap();
    assert_eq!(flac["kind"], "audio");
    assert_eq!(flac["rev"], 0);
    assert_eq!(flac["sharedFolderId"], folder.to_string());
    // The fake FLAC can't be read, so it's listed with only the required fields.
    assert!(flac.get("audio").is_none());
    let status = &library.shared_folders()[0];
    assert!(status.available);
    assert_eq!(status.files, 2);
    assert_eq!(status.unreadable, 1);
}

#[test]
fn a_changed_file_keeps_its_rev_and_an_in_place_write_bumps_it() {
    let (media, _data, library, _folder) = setup();
    eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    let file = media.path().join("Album/01.flac");

    // A normal save: new size and mtime, rev unchanged.
    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file, b"new content, longer").unwrap();
    let index = eventually(&library, "the change", |index| {
        item(index, "Album/01.flac").unwrap()["size"] == 19
    });
    assert_eq!(item(&index, "Album/01.flac").unwrap()["rev"], 0);
    let (full_before, files_before) = etags(&library);

    // A tagger rewrites the file in place with the same size and puts the old mtime back.
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, b"NEW CONTENT, LONGER").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let index = eventually(&library, "the rev bump", |index| {
        item(index, "Album/01.flac").unwrap()["rev"] != 0
    });
    let rev = item(&index, "Album/01.flac").unwrap()["rev"]
        .as_u64()
        .unwrap();
    let (full_after, files_after) = etags(&library);
    assert_ne!(full_before, full_after);
    assert_ne!(files_before, files_after, "the file list carries rev");

    // The bump lives in the settings, so a restart keeps it.
    let data_dir = library.data_dir();
    library.shutdown();
    drop(library);
    let settings = Settings::load_or_create(&data_dir).unwrap();
    assert_eq!(settings.revs.counter, rev);
    let library = Library::start(options(&data_dir)).unwrap();
    let index = eventually(&library, "reload", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    assert_eq!(item(&index, "Album/01.flac").unwrap()["rev"], rev);
}

#[test]
fn a_rebuilt_index_keeps_ids_and_revs() {
    let (media, _data, library, _folder) = setup();
    let before = eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    let file = media.path().join("Album/01.flac");
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, b"NOT REALLY FLAC").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let bumped = eventually(&library, "the rev bump", |index| {
        item(index, "Album/01.flac").unwrap()["rev"] != 0
    });
    let data_dir = library.data_dir();
    library.shutdown();
    drop(library);
    for name in ["index.sqlite", "index.sqlite-wal", "index.sqlite-shm"] {
        let _ = fs::remove_file(data_dir.join(name));
    }
    let library = Library::start(options(&data_dir)).unwrap();
    // After a rebuild, the folder is offline with no items until its scan completes…
    let rebuilt = eventually(&library, "rescan after rebuild", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    // …and then the same ids and revs come back.
    assert_eq!(
        item(&rebuilt, "Album/01.flac").unwrap()["id"],
        item(&before, "Album/01.flac").unwrap()["id"]
    );
    assert_eq!(
        item(&rebuilt, "Album/01.flac").unwrap()["rev"],
        item(&bumped, "Album/01.flac").unwrap()["rev"]
    );
}

#[test]
fn removals_are_published_but_an_unmounted_volume_keeps_the_snapshot() {
    let (media, data, library, folder) = setup();
    eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    fs::remove_file(media.path().join("Album/cover.jpg")).unwrap();
    eventually(&library, "the removal", |index| {
        index["items"].as_array().unwrap().len() == 1
    });

    // Pretend the folder's drive was unplugged: it now looks like it's on another volume.
    library.shutdown();
    drop(library);
    let mut settings = Settings::load_or_create(data.path()).unwrap();
    settings.shared_folders[0].volume.mount_point = "/Volumes/Unplugged".into();
    settings.save(data.path()).unwrap();
    let library = Library::start(options(data.path())).unwrap();
    fs::remove_file(media.path().join("Album/01.flac")).unwrap();
    library.rescan(None);
    library.wait_until_idle(WAIT);
    let index = index(&library);
    assert_eq!(index["offline"], serde_json::json!([folder.to_string()]));
    assert_eq!(
        index["items"].as_array().unwrap().len(),
        1,
        "the last snapshot is still served"
    );
    let published = library.published();
    assert!(!published.folders[0].available);
    assert!(!library.shared_folders()[0].available);
}

#[cfg(unix)]
#[test]
fn a_scan_error_publishes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root can read anything
    }
    let (media, _data, library, _folder) = setup();
    fs::create_dir(media.path().join("Locked")).unwrap();
    fs::write(media.path().join("Locked/a.flac"), b"x").unwrap();
    eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 3
    });
    fs::set_permissions(
        media.path().join("Locked"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    fs::remove_file(media.path().join("Album/cover.jpg")).unwrap();
    library.rescan(None);
    std::thread::sleep(Duration::from_millis(300));
    // A failed scan counts as settled, so a caller can report its errors.
    assert!(library.wait_until_idle(WAIT));
    let index = index(&library);
    // Neither the unreadable folder's file nor the real removal is published.
    assert_eq!(index["items"].as_array().unwrap().len(), 3);
    let status = &library.shared_folders()[0];
    assert!(!status.scan_errors.is_empty());
    assert_eq!(status.scan_errors[0].path, "Locked");

    // The user chooses to publish without the unreadable folder.
    library
        .set_excluded(status.id, vec![vec!["Locked".into()]])
        .unwrap();
    let index = eventually(&library, "publishing without Locked", |index| {
        index["items"].as_array().unwrap().len() == 1
    });
    assert!(item(&index, "Album/01.flac").is_some());
    fs::set_permissions(
        media.path().join("Locked"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

#[test]
fn slice_changes_reach_subscribers() {
    let (_media, _data, library, folder) = setup();
    eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    let mut events = library.subscribe();
    let id = library
        .create_slice(
            "Covers",
            None,
            minimoon_media_server::slices::SliceExpr::Selection {
                folders: vec![],
                files: vec![minimoon_media_server::slices::TreePath {
                    folder,
                    path: vec!["Album".into(), "01.flac".into()],
                }],
            },
            true,
        )
        .unwrap();
    library.wait_until_idle(WAIT);
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(seen.contains(&ServerEvent::SlicesChanged), "{seen:?}");
    assert!(seen.iter().any(
        |event| matches!(event, ServerEvent::SliceChanged { slice_id, .. } if *slice_id == id)
    ));
    // Selecting the file brought its folder's cover along.
    let published = library.published();
    let slice = published.slice(id).unwrap();
    let body: Value = serde_json::from_slice(&slice.index.as_ref().unwrap().full.plain).unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        library.companions(folder, "Album/01.flac"),
        vec!["Album/cover.jpg"]
    );
}

/// macOS: a write made while the server was stopped is replayed from the stored FSEvents event
/// id, so it still bumps `rev` even though the tagger restored the mtime.
#[cfg(target_os = "macos")]
#[test]
fn replays_writes_made_while_stopped() {
    let (media, _data, library, _folder) = setup();
    eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    // Let the stored event id catch up with the scan's own events.
    let data_dir = library.data_dir();
    let deadline = Instant::now() + WAIT;
    while Settings::load_or_create(&data_dir)
        .unwrap()
        .fsevents_event_id
        .is_none()
    {
        assert!(Instant::now() < deadline, "no event id stored");
        std::thread::sleep(Duration::from_millis(100));
    }
    library.shutdown();
    drop(library);

    let file = media.path().join("Album/01.flac");
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, b"NOT REALLY FLAC").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    let library = Library::start(options(&data_dir)).unwrap();
    eventually(&library, "the replayed write", |index| {
        item(index, "Album/01.flac").is_some_and(|item| item["rev"] != 0)
    });
}

/// A write that a scan caught midway and whose mtime was then put back ends with the original
/// size and mtime but different bytes, so it needs a new `rev`.
#[test]
fn a_write_whose_mtime_goes_back_bumps_rev() {
    let (media, _data, library, _folder) = setup();
    eventually(&library, "first scan", |index| {
        index["items"].as_array().unwrap().len() == 2
    });
    let file = media.path().join("Album/01.flac");
    let original = fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file, b"NOT REALLY FLAC").unwrap();
    let original_ms = original
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    eventually(&library, "the mid-write state", |index| {
        item(index, "Album/01.flac").unwrap()["mtime"].as_u64() != Some(original_ms)
    });
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(original)
        .unwrap();
    let index = eventually(&library, "the restored mtime", |index| {
        item(index, "Album/01.flac").unwrap()["mtime"].as_u64() == Some(original_ms)
    });
    assert_ne!(item(&index, "Album/01.flac").unwrap()["rev"], 0);
}
