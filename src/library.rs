//! The library: shared folders, scans, metadata reads, `rev` bumps and publication (spec §5 and
//! §8).
//!
//! A driver thread owns the scanning state machine. Walks and metadata reads run on worker
//! threads and report back over a channel, so the shared state is locked only for quick
//! transitions and for rebuilding what's published. The HTTP routes never lock it: they read
//! the latest [`Published`] view.
//!
//! Scan safety (spec §5):
//! - before and after every walk, the folder's volume is checked; a failed check marks the
//!   folder unavailable and keeps its last snapshot;
//! - whatever a walk can't read is treated as not there;
//! - a completed scan, with the metadata of every new or changed file read, replaces the
//!   folder's snapshot in one database transaction and one publication;
//! - a folder with no snapshot yet is unavailable until its first scan completes.

use crate::index::{Change, FileRecord, IndexStore, ReadStatus, Snapshot};
use crate::model::ReadError;
use crate::published::{self, FolderView, ItemCache, Published};
use crate::reader::{self, file_type, reader_version, Reader};
use crate::scan::{self, ScanError, ScanResult};
use crate::settings::{Settings, SharedFolderSettings};
use crate::volume::{self, VolumeIdentity};
use crate::watch::{Batch, WatchEvent, Watcher};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Temporary read failures are retried this many times before a file counts as unreadable.
const MAX_READ_ATTEMPTS: u32 = 5;
/// The first retry waits this long; each later one waits four times longer.
const FIRST_RETRY: Duration = Duration::from_secs(60);
/// Background re-reads (reader upgrades, retries) are done this many files at a time, each
/// batch published on its own.
const BACKGROUND_BATCH: usize = 256;
/// After a change event, wait this long for more before rescanning…
const DEBOUNCE: Duration = Duration::from_secs(2);
/// …but never longer than this after the first event.
const DEBOUNCE_CAP: Duration = Duration::from_secs(10);
/// How often each folder's volume is checked between scans.
const VOLUME_CHECK_INTERVAL: Duration = Duration::from_secs(15);
/// After a folder becomes unavailable during a scan, the next attempt waits this long.
const ERROR_RETRY: Duration = Duration::from_secs(60);
/// The stored FSEvents event id is written at most this often.
const EVENT_ID_SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// How the library runs.
#[derive(Clone, Debug)]
pub struct LibraryOptions {
    /// The server's data directory: settings, the index and caches.
    pub data_dir: PathBuf,
    /// A metadata read taking longer than this counts as a temporary failure.
    pub read_timeout: Duration,
    /// A full rescan of every folder this often, on top of watching.
    pub rescan_interval: Duration,
    /// Metadata reader threads.
    pub workers: usize,
    /// Watch folders for changes. Off only in tests that drive scans themselves.
    pub watch: bool,
}

impl LibraryOptions {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            read_timeout: Duration::from_secs(60),
            rescan_interval: Duration::from_secs(30 * 60),
            workers: 4,
            watch: true,
        }
    }
}

/// A change clients may want to hear about (the `events` feature, spec §5 "Push").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerEvent {
    /// The index changed: its new ETags for the full index and the file list.
    IndexChanged { etag: String, files_etag: String },
    FolderAvailability {
        shared_folder_id: Uuid,
        available: bool,
    },
    /// A shared folder was added, removed or renamed.
    SharedFoldersChanged,
}

/// When the watcher last reported a write to each file, for the mid-transfer check (spec §6).
#[derive(Default)]
pub struct WriteLog {
    seq: AtomicU64,
    last: Mutex<HashMap<PathBuf, (u64, Instant)>>,
}

impl WriteLog {
    fn record(&self, path: &Path) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.len() > 50_000 {
            let cutoff = Instant::now() - Duration::from_secs(24 * 60 * 60);
            last.retain(|_, (_, at)| *at > cutoff);
        }
        last.insert(path.to_path_buf(), (seq, Instant::now()));
    }

    /// The sequence number of the last write reported for a file (canonical path), or 0.
    pub fn last_write(&self, path: &Path) -> u64 {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .map_or(0, |(seq, _)| *seq)
    }
}

/// A shared folder's state, for the desktop app.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedFolderStatus {
    pub id: Uuid,
    pub name: String,
    pub path: PathBuf,
    /// Mounted and published at least once.
    pub available: bool,
    /// Why it's unavailable, when it is.
    pub unavailable_reason: Option<String>,
    /// Indexed at least once. Until then nothing in it is published.
    pub indexed: bool,
    pub scanning: bool,
    /// What the current scan is doing.
    pub activity: IndexActivity,
    /// Files still published with metadata from an older reader, waiting to be re-read in the
    /// background. Approximate while a re-read is under way (a scan may re-read some of them
    /// first); 0 while the folder's volume is missing.
    pub outdated: usize,
    /// How many files were outdated when the current re-read started, for progress displays; 0
    /// when none is under way.
    pub outdated_total: usize,
    /// Folders or files the last scan couldn't read; the folder's previous snapshot is kept.
    pub scan_errors: Vec<ScanError>,
    /// Consecutive scans that found the folder unavailable partway through.
    pub failed_scans: u32,
    pub files: usize,
    /// Files whose metadata read failed temporarily and will be retried.
    pub temporary_failures: usize,
    pub unreadable: usize,
}

/// What a shared folder's scan is doing, for progress displays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndexActivity {
    #[default]
    Idle,
    /// Listing files: names, sizes and modification dates.
    Listing,
    /// Reading the metadata of new and changed files. Nothing the scan found is published until
    /// every read is done.
    Reading { done: usize, total: usize },
}

/// The reason a folder that has never been indexed is unavailable, when its volume is fine.
pub const NOT_INDEXED_YET: &str = "not scanned yet";

enum Msg {
    Watch(Batch),
    ScanDone {
        folder: Uuid,
        token: u64,
        result: std::result::Result<ScanResult, String>,
    },
    ReadDone {
        id: u64,
        result: std::result::Result<Option<String>, ReadError>,
    },
    Kick,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Scan(u64),
    Background,
}

/// An outstanding metadata read.
struct Job {
    folder: Uuid,
    path: String,
    size: u64,
    mtime_ns: i64,
    ctime_ns: Option<i64>,
    reader: Option<Reader>,
    origin: Origin,
    /// The record it replaces, when the file's content hasn't changed (a background re-read).
    unchanged: Option<FileRecord>,
}

struct WorkItem {
    id: u64,
    path: PathBuf,
    reader: Option<Reader>,
}

#[derive(Default)]
struct QueueState {
    scan: VecDeque<WorkItem>,
    background: VecDeque<WorkItem>,
    running: HashMap<u64, Instant>,
    /// Reads that timed out. Their workers were replaced, so they exit when the read returns.
    abandoned: HashSet<u64>,
    shutdown: bool,
}

/// Reads waiting for a worker. Scan reads go before background ones.
#[derive(Default)]
struct JobQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl JobQueue {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, item: WorkItem, origin: Origin) {
        let mut state = self.lock();
        match origin {
            Origin::Scan(_) => state.scan.push_back(item),
            Origin::Background => state.background.push_back(item),
        }
        drop(state);
        self.ready.notify_one();
    }
}

fn spawn_worker(queue: Arc<JobQueue>, tx: Sender<Msg>) {
    let _ = std::thread::Builder::new()
        .name("mmp-reader".into())
        .spawn(move || loop {
            let item = {
                let mut state = queue.lock();
                loop {
                    if state.shutdown {
                        return;
                    }
                    if let Some(item) = state
                        .scan
                        .pop_front()
                        .or_else(|| state.background.pop_front())
                    {
                        state.running.insert(item.id, Instant::now());
                        break item;
                    }
                    state = queue.ready.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            let result = reader::read_metadata(&item.path, item.reader);
            {
                let mut state = queue.lock();
                state.running.remove(&item.id);
                if state.abandoned.remove(&item.id) {
                    return;
                }
            }
            if tx
                .send(Msg::ReadDone {
                    id: item.id,
                    result,
                })
                .is_err()
            {
                return;
            }
        });
}

/// What a scan found and what it still has to read before publishing.
struct Plan {
    token: u64,
    scanned: Vec<scan::ScannedFile>,
    removed: Vec<String>,
    pending: HashSet<u64>,
    /// How many files the plan reads.
    reads: usize,
    results: HashMap<String, FileRecord>,
    bumped: bool,
    /// Subfolders and files the walk couldn't read, so treated as not there.
    errors: Vec<ScanError>,
}

enum Phase {
    Idle,
    Walking { token: u64, writes: HashSet<String> },
    Reading(Plan),
}

struct FolderState {
    canonical_root: Option<PathBuf>,
    snapshot: Option<Snapshot>,
    volume_ok: bool,
    unavailable: Option<String>,
    phase: Phase,
    scan_due: Option<Instant>,
    debounce_cap: Option<Instant>,
    last_scan: Option<Instant>,
    last_volume_check: Option<Instant>,
    scan_errors: Vec<ScanError>,
    failures: u32,
    pending_writes: HashSet<String>,
    background: HashSet<u64>,
    /// Finished background reads: the path, the new record, and the record it was read from.
    background_results: Vec<(String, FileRecord, FileRecord)>,
    /// Files read by an older reader, counted when the current background batch started.
    outdated: usize,
    /// `outdated` when the current re-read started.
    outdated_total: usize,
}

impl FolderState {
    fn new(settings: &SharedFolderSettings, snapshot: Option<Snapshot>) -> Self {
        Self {
            canonical_root: settings.path.canonicalize().ok(),
            snapshot,
            volume_ok: false,
            unavailable: None,
            phase: Phase::Idle,
            scan_due: Some(Instant::now()),
            debounce_cap: None,
            last_scan: None,
            last_volume_check: None,
            scan_errors: Vec::new(),
            failures: 0,
            pending_writes: HashSet::new(),
            background: HashSet::new(),
            background_results: Vec::new(),
            outdated: 0,
            outdated_total: 0,
        }
    }

    fn idle(&self) -> bool {
        matches!(self.phase, Phase::Idle)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The state the driver thread and the API share.
struct Core {
    options: LibraryOptions,
    settings: Settings,
    store: IndexStore,
    folders: HashMap<Uuid, FolderState>,
    cache: ItemCache,
    jobs: HashMap<u64, Job>,
    next_id: u64,
    queue: Arc<JobQueue>,
    tx: Sender<Msg>,
    publish_due: bool,
    published: Arc<RwLock<Arc<Published>>>,
    events: broadcast::Sender<ServerEvent>,
    write_log: Arc<WriteLog>,
    watcher: Option<Watcher>,
    watched_roots: Vec<PathBuf>,
    /// The latest FSEvents event id seen, or the current id when watching started.
    event_id_candidate: Option<u64>,
    event_id_saved_at: Option<Instant>,
}

impl Core {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn save_settings(&self) {
        if let Err(error) = self.settings.save(&self.options.data_dir) {
            tracing::error!("can't save settings: {error:#}");
        }
    }

    // MARK: Driver

    /// How long the driver may sleep before something is due.
    fn next_wakeup(&self) -> Duration {
        let now = Instant::now();
        let mut next = now + Duration::from_secs(5);
        for state in self.folders.values() {
            if let Some(due) = state.scan_due {
                next = next.min(due);
            }
        }
        if !self.jobs.is_empty() {
            next = next.min(now + Duration::from_secs(1));
        }
        if self.publish_due {
            next = now;
        }
        next.saturating_duration_since(now)
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Watch(batch) => self.handle_watch(batch),
            Msg::ScanDone {
                folder,
                token,
                result,
            } => self.handle_scan_done(folder, token, result),
            Msg::ReadDone { id, result } => self.handle_read_done(id, result),
            Msg::Kick | Msg::Shutdown => {}
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.update_watcher();
        let ids: Vec<Uuid> = self.settings.shared_folders.iter().map(|f| f.id).collect();
        for id in &ids {
            self.maybe_start_scan(*id, now);
        }
        self.check_timeouts(now);
        for id in &ids {
            self.maybe_start_background(*id);
        }
        if self.publish_due {
            self.publish();
        }
        self.maybe_save_event_id(now);
    }

    // MARK: Watching

    fn update_watcher(&mut self) {
        if !self.options.watch {
            return;
        }
        let roots: Vec<PathBuf> = self
            .settings
            .shared_folders
            .iter()
            .filter_map(|folder| self.folders.get(&folder.id)?.canonical_root.clone())
            .collect();
        if roots == self.watched_roots && (self.watcher.is_some() || roots.is_empty()) {
            return;
        }
        self.watcher = None;
        self.watched_roots = roots.clone();
        if roots.is_empty() {
            return;
        }
        let since = self.event_id_candidate.or(self.settings.fsevents_event_id);
        if since.is_none() {
            self.event_id_candidate = Watcher::current_event_id();
        }
        let tx = self.tx.clone();
        let write_log = self.write_log.clone();
        let sink = Box::new(move |batch: Batch| {
            for event in &batch.events {
                if let WatchEvent::Changed { path, write: true } = event {
                    write_log.record(path);
                }
            }
            let _ = tx.send(Msg::Watch(batch));
        });
        match Watcher::start(&roots, since, sink) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(error) => tracing::error!("can't watch the shared folders: {error:#}"),
        }
    }

    /// The shared folder a watched path is in, and its index path ("" for the folder itself).
    fn locate(&self, path: &Path) -> Option<(Uuid, String)> {
        for folder in &self.settings.shared_folders {
            let Some(root) = self
                .folders
                .get(&folder.id)
                .and_then(|s| s.canonical_root.as_ref())
            else {
                continue;
            };
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let mut components = Vec::new();
            for component in relative.components() {
                let name = component.as_os_str().to_str()?;
                if scan::is_hidden_name(name) {
                    return None;
                }
                components.push(name);
            }
            return Some((folder.id, components.join("/")));
        }
        None
    }

    fn schedule_scan(&mut self, folder: Uuid, delay: Duration) {
        let now = Instant::now();
        if let Some(state) = self.folders.get_mut(&folder) {
            // Each event pushes the scan back by `delay`, but never past the cap set by the
            // first event since the last scan started.
            let cap = *state.debounce_cap.get_or_insert(now + DEBOUNCE_CAP);
            state.scan_due = Some((now + delay).min(cap));
        }
    }

    fn handle_watch(&mut self, batch: Batch) {
        if let Some(id) = batch.latest_event_id {
            self.event_id_candidate = Some(self.event_id_candidate.map_or(id, |c| c.max(id)));
        }
        for event in batch.events {
            match event {
                WatchEvent::Changed { path, write } => {
                    if let Some((folder, relative)) = self.locate(&path) {
                        if write && !relative.is_empty() {
                            if let Some(state) = self.folders.get_mut(&folder) {
                                state.pending_writes.insert(relative);
                            }
                        }
                        self.schedule_scan(folder, DEBOUNCE);
                    }
                }
                WatchEvent::Rescan { path } => {
                    let affected: Vec<Uuid> = self
                        .settings
                        .shared_folders
                        .iter()
                        .filter(|folder| {
                            self.folders
                                .get(&folder.id)
                                .and_then(|s| s.canonical_root.as_ref())
                                .is_some_and(|root| {
                                    root.starts_with(&path) || path.starts_with(root)
                                })
                        })
                        .map(|folder| folder.id)
                        .collect();
                    for folder in affected {
                        tracing::warn!(
                            "file events were dropped under {}; rescanning by size and mtime, \
                             which can't see writes that restored a file's mtime",
                            path.display()
                        );
                        if let Some(state) = self.folders.get_mut(&folder) {
                            state.last_volume_check = None;
                        }
                        self.schedule_scan(folder, Duration::ZERO);
                    }
                }
                WatchEvent::HistoryDone => {}
            }
        }
    }

    fn maybe_save_event_id(&mut self, now: Instant) {
        let Some(candidate) = self.event_id_candidate else {
            return;
        };
        if self.settings.fsevents_event_id == Some(candidate) {
            return;
        }
        if self
            .event_id_saved_at
            .is_some_and(|at| now.duration_since(at) < EVENT_ID_SAVE_INTERVAL)
        {
            return;
        }
        // Only when every write seen so far has been consumed by a completed scan, so a stored
        // id never skips a write that hasn't been processed.
        let settled = self.folders.values().all(|state| {
            state.idle()
                && state.pending_writes.is_empty()
                && state.scan_due.is_none_or(|due| due > now + DEBOUNCE_CAP)
        });
        if settled {
            self.settings.fsevents_event_id = Some(candidate);
            self.event_id_saved_at = Some(now);
            self.save_settings();
        }
    }

    // MARK: Scanning

    /// The folders whose volume check is due, marked as checked. The checks themselves run
    /// without the lock, because statfs on a hung network mount can block for a long time.
    fn due_volume_checks(&mut self, now: Instant) -> Vec<(Uuid, PathBuf, VolumeIdentity)> {
        let mut due = Vec::new();
        for folder in &self.settings.shared_folders {
            let Some(state) = self.folders.get_mut(&folder.id) else {
                continue;
            };
            if !state.idle()
                || state
                    .last_volume_check
                    .is_some_and(|at| now.duration_since(at) < VOLUME_CHECK_INTERVAL)
            {
                continue;
            }
            state.last_volume_check = Some(now);
            due.push((folder.id, folder.path.clone(), folder.volume.clone()));
        }
        due
    }

    fn apply_volume_check(&mut self, id: Uuid, result: Result<(), volume::Unavailable>) {
        let Some(path) = self.settings.folder(id).map(|f| f.path.clone()) else {
            return;
        };
        let Some(state) = self.folders.get_mut(&id) else {
            return;
        };
        if !state.idle() {
            // A scan started meanwhile and checks the volume itself.
            return;
        }
        let ok = result.is_ok();
        if ok != state.volume_ok {
            if ok {
                tracing::info!("{} is available", path.display());
                state.canonical_root = path.canonicalize().ok();
                state.scan_due = Some(Instant::now());
            } else {
                tracing::warn!(
                    "{} is unavailable: {}",
                    path.display(),
                    result.as_ref().unwrap_err()
                );
            }
            state.volume_ok = ok;
            self.publish_due = true;
        }
        state.unavailable = result.err().map(|reason| reason.to_string());
    }

    fn maybe_start_scan(&mut self, id: Uuid, now: Instant) {
        let interval = self.options.rescan_interval;
        let Some(settings) = self.settings.folder(id).cloned() else {
            return;
        };
        let token = self.next_id();
        let tx = self.tx.clone();
        let Some(state) = self.folders.get_mut(&id) else {
            return;
        };
        if !state.idle() {
            return;
        }
        let periodic = state
            .last_scan
            .is_some_and(|at| now.duration_since(at) >= interval);
        if !periodic && state.scan_due.is_none_or(|due| due > now) {
            return;
        }
        state.scan_due = None;
        state.debounce_cap = None;
        state.last_scan = Some(now);
        if !state.volume_ok {
            // Checked again every VOLUME_CHECK_INTERVAL; a folder coming back is scanned then.
            return;
        }
        let writes = std::mem::take(&mut state.pending_writes);
        state.phase = Phase::Walking { token, writes };
        let root = settings.path.clone();
        let identity = settings.volume.clone();
        let _ = std::thread::Builder::new()
            .name("mmp-scan".into())
            .spawn(move || {
                let result = walk(&root, &identity);
                let _ = tx.send(Msg::ScanDone {
                    folder: id,
                    token,
                    result,
                });
            });
    }

    fn handle_scan_done(
        &mut self,
        id: Uuid,
        token: u64,
        result: std::result::Result<ScanResult, String>,
    ) {
        let Some(state) = self.folders.get_mut(&id) else {
            return;
        };
        let writes = match &mut state.phase {
            Phase::Walking { token: t, writes } if *t == token => std::mem::take(writes),
            _ => return,
        };
        state.phase = Phase::Idle;
        let result = match result {
            Ok(result) => result,
            Err(unavailable) => {
                // Nothing is published; the writes wait for the next scan.
                tracing::warn!("folder became unavailable during a scan: {unavailable}");
                state.pending_writes.extend(writes);
                state.failures += 1;
                state.scan_due = Some(Instant::now() + ERROR_RETRY);
                state.volume_ok = false;
                state.unavailable = Some(unavailable);
                state.last_volume_check = Some(Instant::now());
                self.publish_due = true;
                return;
            }
        };
        if !result.errors.is_empty() {
            tracing::warn!(
                "{} part(s) of a shared folder couldn't be read, so they aren't shared",
                result.errors.len()
            );
        }
        let empty = Snapshot::new();
        let previous = state.snapshot.as_ref().unwrap_or(&empty);
        let scanned_paths: HashSet<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
        let removed: Vec<String> = previous
            .keys()
            .filter(|path| !scanned_paths.contains(path.as_str()))
            .cloned()
            .collect();
        let mut to_read = Vec::new();
        let mut bumps = Vec::new();
        for file in &result.files {
            match previous.get(&file.path) {
                None => to_read.push(file.clone()),
                Some(record) if record.size == file.size && record.mtime_ns == file.mtime_ns => {
                    let ctime_moved = match (record.ctime_ns, file.ctime_ns) {
                        (Some(before), Some(now)) => before != now,
                        _ => true,
                    };
                    // The record's ctime is deliberately not refreshed when only attributes
                    // changed: a write event can arrive after the scan that first sees its new
                    // ctime (a replay after a restart, say), and it must still find the ctime
                    // moved. The cost is an occasional extra bump when FSEvents repeats an old
                    // write flag alongside a later attribute change.
                    if writes.contains(&file.path) && ctime_moved {
                        // Written in place with size and mtime unchanged: a new revision.
                        bumps.push(file.path.clone());
                        to_read.push(file.clone());
                    }
                }
                Some(record) => {
                    // The file changed but its mtime didn't move forward: a write a scan caught
                    // midway, whose mtime was then put back. Its size and mtime may now be ones
                    // clients already hold for other bytes, so it needs a new revision.
                    if file.mtime_ns <= record.mtime_ns {
                        bumps.push(file.path.clone());
                    }
                    to_read.push(file.clone());
                }
            }
        }
        drop(scanned_paths);
        for path in &bumps {
            let rev = self.settings.bump_rev(id, path);
            tracing::info!("in-place write detected; rev {rev} for {path}");
        }
        if !bumps.is_empty() {
            if let Err(error) = self.settings.save(&self.options.data_dir) {
                tracing::error!("can't save settings: {error:#}");
            }
        }
        let mut plan = Plan {
            token,
            scanned: result.files,
            removed,
            pending: HashSet::new(),
            reads: to_read.len(),
            results: HashMap::new(),
            bumped: !bumps.is_empty(),
            errors: result.errors,
        };
        let root = self
            .settings
            .folder(id)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        for file in to_read {
            let reader = file_type(scan::split_name(&file.path)).reader;
            if reader.is_none() {
                plan.results.insert(
                    file.path.clone(),
                    FileRecord {
                        size: file.size,
                        mtime_ns: file.mtime_ns,
                        ctime_ns: file.ctime_ns,
                        reader_version: 0,
                        status: ReadStatus::Read,
                        meta: None,
                    },
                );
                continue;
            }
            let job_id = self.next_id();
            plan.pending.insert(job_id);
            self.queue.push(
                WorkItem {
                    id: job_id,
                    path: root.join(&file.path),
                    reader,
                },
                Origin::Scan(token),
            );
            self.jobs.insert(
                job_id,
                Job {
                    folder: id,
                    path: file.path.clone(),
                    size: file.size,
                    mtime_ns: file.mtime_ns,
                    ctime_ns: file.ctime_ns,
                    reader,
                    origin: Origin::Scan(token),
                    unchanged: None,
                },
            );
        }
        let Some(state) = self.folders.get_mut(&id) else {
            return;
        };
        let done = plan.pending.is_empty();
        state.phase = Phase::Reading(plan);
        if done {
            self.finish_scan(id);
        }
    }

    fn finish_scan(&mut self, id: Uuid) {
        let Some(state) = self.folders.get_mut(&id) else {
            return;
        };
        let Phase::Reading(plan) = std::mem::replace(&mut state.phase, Phase::Idle) else {
            return;
        };
        let mut snapshot = state.snapshot.take().unwrap_or_default();
        let mut changes = Vec::with_capacity(plan.removed.len() + plan.results.len());
        for path in plan.removed {
            snapshot.remove(&path);
            changes.push(Change::Delete(path));
        }
        for (path, record) in plan.results {
            changes.push(Change::Upsert(path.clone(), record.clone()));
            snapshot.insert(path, record);
        }
        debug_assert_eq!(snapshot.len(), plan.scanned.len());
        if let Err(error) = self.store.apply(id, &changes, now_ms()) {
            tracing::error!("can't write the index: {error:#}");
        }
        state.failures = 0;
        state.scan_errors = plan.errors;
        let changed = !changes.is_empty() || state.snapshot_was_missing();
        state.snapshot = Some(snapshot);
        let snapshot = state.snapshot.as_ref().expect("just set");
        let pruned = self
            .settings
            .prune_revs(id, |path| snapshot.contains_key(path));
        if pruned {
            self.save_settings();
        }
        if changed || plan.bumped || pruned {
            self.publish_due = true;
        }
    }

    // MARK: Metadata reads

    fn check_timeouts(&mut self, now: Instant) {
        let timeout = self.options.read_timeout;
        let expired: Vec<u64> = {
            let state = self.queue.lock();
            state
                .running
                .iter()
                .filter(|(_, started)| now.duration_since(**started) > timeout)
                .map(|(id, _)| *id)
                .collect()
        };
        for id in expired {
            if self.jobs.contains_key(&id) {
                tracing::warn!("a metadata read timed out");
                // The stuck thread finishes on its own time and then exits; replace it so reads
                // keep going.
                {
                    let mut state = self.queue.lock();
                    state.running.remove(&id);
                    state.abandoned.insert(id);
                }
                spawn_worker(self.queue.clone(), self.tx.clone());
                self.handle_read_done(id, Err(ReadError::Temporary("timed out".into())));
            }
        }
    }

    fn record_for(job: &Job, result: std::result::Result<Option<String>, ReadError>) -> FileRecord {
        let version = reader_version(job.reader);
        let base = FileRecord {
            size: job.size,
            mtime_ns: job.mtime_ns,
            ctime_ns: job.ctime_ns,
            reader_version: version,
            status: ReadStatus::Read,
            meta: None,
        };
        match result {
            Ok(meta) => FileRecord {
                meta: meta.map(Arc::from),
                ..base
            },
            Err(ReadError::Unreadable(message)) => {
                tracing::debug!("unreadable {}: {message}", job.path);
                FileRecord {
                    status: ReadStatus::Unreadable,
                    ..base
                }
            }
            Err(ReadError::Temporary(message)) => {
                tracing::debug!("temporary read failure {}: {message}", job.path);
                // A failed retry of an unchanged file keeps its previous metadata; recovering
                // later is never taken as a change.
                let (attempts, meta, previous_version) = match &job.unchanged {
                    Some(previous) => (
                        match previous.status {
                            ReadStatus::Temporary { attempts, .. } => attempts,
                            _ => 0,
                        },
                        previous.meta.clone(),
                        previous.reader_version,
                    ),
                    None => (0, None, version),
                };
                let attempts = attempts + 1;
                if attempts >= MAX_READ_ATTEMPTS {
                    FileRecord {
                        status: ReadStatus::Unreadable,
                        meta,
                        ..base
                    }
                } else {
                    let wait = FIRST_RETRY * 4u32.pow(attempts - 1);
                    FileRecord {
                        reader_version: previous_version,
                        status: ReadStatus::Temporary {
                            attempts,
                            retry_at_ms: now_ms()
                                + i64::try_from(wait.as_millis()).unwrap_or(i64::MAX),
                        },
                        meta,
                        ..base
                    }
                }
            }
        }
    }

    fn handle_read_done(
        &mut self,
        id: u64,
        result: std::result::Result<Option<String>, ReadError>,
    ) {
        let Some(job) = self.jobs.remove(&id) else {
            return;
        };
        let record = Self::record_for(&job, result);
        let Some(state) = self.folders.get_mut(&job.folder) else {
            return;
        };
        match job.origin {
            Origin::Scan(token) => {
                let Phase::Reading(plan) = &mut state.phase else {
                    return;
                };
                if plan.token != token {
                    return;
                }
                plan.pending.remove(&id);
                plan.results.insert(job.path, record);
                if plan.pending.is_empty() {
                    self.finish_scan(job.folder);
                }
            }
            Origin::Background => {
                state.background.remove(&id);
                if let Some(original) = job.unchanged {
                    state.background_results.push((job.path, record, original));
                }
                if state.background.is_empty() {
                    self.finish_background(job.folder);
                }
            }
        }
    }

    fn maybe_start_background(&mut self, id: Uuid) {
        let now = now_ms();
        let Some(root) = self.settings.folder(id).map(|f| f.path.clone()) else {
            return;
        };
        let Some(state) = self.folders.get(&id) else {
            return;
        };
        if !state.idle() || !state.volume_ok || !state.background.is_empty() {
            return;
        }
        let Some(snapshot) = &state.snapshot else {
            return;
        };
        // Outdated files are counted for progress displays. A full count walks the whole
        // snapshot, so it's taken only when no re-read is under way; during one, each batch
        // stops once it's full and `finish_background` subtracts what it read. A pass that
        // finishes without filling a batch has seen every file, so its count is exact.
        let counting = state.outdated == 0;
        let mut complete = true;
        let mut outdated = 0;
        let mut wanted: Vec<(String, FileRecord, Reader)> = Vec::new();
        for (path, record) in snapshot {
            if wanted.len() == BACKGROUND_BATCH && !counting {
                complete = false;
                break;
            }
            let Some(reader) = file_type(scan::split_name(path)).reader else {
                continue;
            };
            let due = match record.status {
                ReadStatus::Temporary { retry_at_ms, .. } => retry_at_ms <= now,
                _ => {
                    let old = record.reader_version < reader_version(Some(reader));
                    outdated += usize::from(old);
                    old
                }
            };
            if due && wanted.len() < BACKGROUND_BATCH {
                wanted.push((path.clone(), record.clone(), reader));
            }
        }
        if complete {
            if let Some(state) = self.folders.get_mut(&id) {
                state.outdated = outdated;
                state.outdated_total = if outdated == 0 {
                    0
                } else {
                    state.outdated_total.max(outdated)
                };
            }
        }
        for (path, record, reader) in wanted {
            let job_id = self.next_id();
            self.queue.push(
                WorkItem {
                    id: job_id,
                    path: root.join(&path),
                    reader: Some(reader),
                },
                Origin::Background,
            );
            self.jobs.insert(
                job_id,
                Job {
                    folder: id,
                    path,
                    size: record.size,
                    mtime_ns: record.mtime_ns,
                    ctime_ns: record.ctime_ns,
                    reader: Some(reader),
                    origin: Origin::Background,
                    unchanged: Some(record),
                },
            );
            if let Some(state) = self.folders.get_mut(&id) {
                state.background.insert(job_id);
            }
        }
    }

    fn finish_background(&mut self, id: Uuid) {
        let Some(state) = self.folders.get_mut(&id) else {
            return;
        };
        let results = std::mem::take(&mut state.background_results);
        // Re-read either here or, if a scan replaced the record meanwhile, by that scan.
        state.outdated = state
            .outdated
            .saturating_sub(results.iter().filter(|r| was_outdated(r)).count());
        let Some(snapshot) = state.snapshot.as_mut() else {
            return;
        };
        let mut changes = Vec::new();
        for (path, record, original) in results {
            // Only if nothing replaced the record while it was being read: a scan may have
            // re-read the file after an in-place write with the same size and mtime.
            let Some(current) = snapshot.get_mut(&path) else {
                continue;
            };
            if *current != original {
                continue;
            }
            if *current != record {
                *current = record.clone();
                changes.push(Change::Upsert(path, record));
            }
        }
        if changes.is_empty() {
            return;
        }
        if let Err(error) = self.store.apply(id, &changes, now_ms()) {
            tracing::error!("can't write the index: {error:#}");
        }
        self.publish_due = true;
    }

    // MARK: Publishing

    fn publish(&mut self) {
        self.publish_due = false;
        let views: Vec<FolderView<'_>> = self
            .settings
            .shared_folders
            .iter()
            .map(|folder| {
                let state = self.folders.get(&folder.id);
                FolderView {
                    settings: folder,
                    snapshot: state.and_then(|s| s.snapshot.as_ref()),
                    volume_ok: state.is_some_and(|s| s.volume_ok),
                }
            })
            .collect();
        let new = published::build(&self.settings, &views, &mut self.cache);
        swap_and_announce(&self.published, &self.events, new);
    }

    /// Replaces only the pairing state in what's published, without rebuilding indexes.
    fn publish_auth(&mut self) {
        let mut current = (**self.published.read().unwrap_or_else(|e| e.into_inner())).clone();
        current.auth = Arc::new(published::auth_state(&self.settings));
        current.name = self.settings.display_name();
        *self.published.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(current);
    }

    fn folder_status(&self, folder: &SharedFolderSettings) -> SharedFolderStatus {
        let state = self.folders.get(&folder.id);
        let snapshot = state.and_then(|s| s.snapshot.as_ref());
        let (temporary, unreadable) = snapshot.map_or((0, 0), |snapshot| {
            snapshot
                .values()
                .fold((0, 0), |(t, u), record| match record.status {
                    ReadStatus::Temporary { .. } => (t + 1, u),
                    ReadStatus::Unreadable => (t, u + 1),
                    ReadStatus::Read => (t, u),
                })
        });
        SharedFolderStatus {
            id: folder.id,
            name: folder.name.clone(),
            path: folder.path.clone(),
            available: state.is_some_and(|s| s.volume_ok && s.snapshot.is_some()),
            unavailable_reason: state.and_then(|s| s.unavailable.clone()).or_else(|| {
                (state.is_some_and(|s| s.snapshot.is_none())).then(|| NOT_INDEXED_YET.to_string())
            }),
            indexed: snapshot.is_some(),
            scanning: state.is_some_and(|s| !s.idle()),
            activity: match state.map(|s| &s.phase) {
                None | Some(Phase::Idle) => IndexActivity::Idle,
                Some(Phase::Walking { .. }) => IndexActivity::Listing,
                Some(Phase::Reading(plan)) => IndexActivity::Reading {
                    done: plan.reads - plan.pending.len(),
                    total: plan.reads,
                },
            },
            // Nothing is re-read while the folder's volume is missing, so nothing is reported.
            outdated: state.filter(|s| s.volume_ok).map_or(0, |s| {
                // Reads finished in the current batch count as done, though they're published
                // only once the whole batch is.
                let read = s
                    .background_results
                    .iter()
                    .filter(|r| was_outdated(r))
                    .count();
                s.outdated.saturating_sub(read)
            }),
            outdated_total: state
                .filter(|s| s.volume_ok)
                .map_or(0, |s| s.outdated_total),
            scan_errors: state.map(|s| s.scan_errors.clone()).unwrap_or_default(),
            failed_scans: state.map_or(0, |s| s.failures),
            files: snapshot.map_or(0, |s| s.len()),
            temporary_failures: temporary,
            unreadable,
        }
    }
}

/// Whether a finished background read was for a reader upgrade, not a retry: a record that
/// wasn't a temporary failure was only re-read because its reader was outdated.
fn was_outdated((_, _, original): &(String, FileRecord, FileRecord)) -> bool {
    !matches!(original.status, ReadStatus::Temporary { .. })
}

impl FolderState {
    fn snapshot_was_missing(&self) -> bool {
        self.snapshot.is_none()
    }
}

/// Lists a folder after checking its volume, and checks the volume again afterwards, so a drive
/// that dropped partway through never looks like deleted files.
fn walk(root: &Path, identity: &VolumeIdentity) -> std::result::Result<ScanResult, String> {
    volume::check(root, identity).map_err(|reason| reason.to_string())?;
    let result = scan::scan(root);
    volume::check(root, identity).map_err(|reason| reason.to_string())?;
    Ok(result)
}

/// Makes `new` what the routes serve, then sends the events the change implies: only once the
/// new view is visible, so a client acting on an event finds the ETag it names.
fn swap_and_announce(
    published: &RwLock<Arc<Published>>,
    events: &broadcast::Sender<ServerEvent>,
    new: Published,
) {
    let new = Arc::new(new);
    let old = std::mem::replace(
        &mut *published.write().unwrap_or_else(|e| e.into_inner()),
        new.clone(),
    );
    for event in changes_between(&old, &new) {
        let _ = events.send(event);
    }
}

/// The events a change in what's published implies.
fn changes_between(old: &Published, new: &Published) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    for folder in new.folders.iter() {
        let was = old.folder(folder.id).map(|f| f.available);
        if was != Some(folder.available) && (was.is_some() || !folder.available) {
            events.push(ServerEvent::FolderAvailability {
                shared_folder_id: folder.id,
                available: folder.available,
            });
        }
    }
    let listing = |p: &Published| {
        p.folders
            .iter()
            .map(|f| (f.id, f.name.clone()))
            .collect::<Vec<_>>()
    };
    if listing(old) != listing(new) {
        events.push(ServerEvent::SharedFoldersChanged);
    }
    if old.index.full.etag != new.index.full.etag || old.index.files.etag != new.index.files.etag {
        events.push(ServerEvent::IndexChanged {
            etag: new.index.full.etag.clone(),
            files_etag: new.index.files.etag.clone(),
        });
    }
    events
}

/// The library and its driver thread.
pub struct Library {
    core: Arc<Mutex<Core>>,
    tx: Sender<Msg>,
    published: Arc<RwLock<Arc<Published>>>,
    events: broadcast::Sender<ServerEvent>,
    write_log: Arc<WriteLog>,
    queue: Arc<JobQueue>,
    driver: Mutex<Option<JoinHandle<()>>>,
}

impl Library {
    /// Loads the settings and index, publishes the last snapshots, and starts scanning
    /// and watching.
    pub fn start(options: LibraryOptions) -> Result<Self> {
        std::fs::create_dir_all(&options.data_dir)
            .with_context(|| format!("can't create {}", options.data_dir.display()))?;
        let settings = Settings::load_or_create(&options.data_dir)?;
        let store = IndexStore::open(&options.data_dir.join("index.sqlite"))?;
        let mut snapshots = store.load()?;
        let (tx, rx) = mpsc::channel();
        let queue = Arc::new(JobQueue::default());
        for _ in 0..options.workers.max(1) {
            spawn_worker(queue.clone(), tx.clone());
        }
        let published = Arc::new(RwLock::new(Arc::new(Published::empty(&settings))));
        let (events, _) = broadcast::channel(256);
        let write_log = Arc::new(WriteLog::default());
        let folders = settings
            .shared_folders
            .iter()
            .map(|folder| {
                (
                    folder.id,
                    FolderState::new(folder, snapshots.remove(&folder.id)),
                )
            })
            .collect();
        let mut core = Core {
            options,
            settings,
            store,
            folders,
            cache: ItemCache::default(),
            jobs: HashMap::new(),
            next_id: 0,
            queue: queue.clone(),
            tx: tx.clone(),
            publish_due: true,
            published: published.clone(),
            events: events.clone(),
            write_log: write_log.clone(),
            watcher: None,
            watched_roots: Vec::new(),
            event_id_candidate: None,
            event_id_saved_at: None,
        };
        // Folders removed from the settings while the server was stopped.
        for stale in snapshots.keys() {
            let _ = core.store.remove_folder(*stale);
        }
        let checks = core.due_volume_checks(Instant::now());
        for (id, result) in run_volume_checks(checks) {
            core.apply_volume_check(id, result);
        }
        core.tick();
        let core = Arc::new(Mutex::new(core));
        let driver_core = core.clone();
        let driver = std::thread::Builder::new()
            .name("mmp-library".into())
            .spawn(move || drive(driver_core, rx))?;
        Ok(Self {
            core,
            tx,
            published,
            events,
            write_log,
            queue,
            driver: Mutex::new(Some(driver)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn kick(&self) {
        let _ = self.tx.send(Msg::Kick);
    }

    /// What the routes serve now.
    pub fn published(&self) -> Arc<Published> {
        self.published
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn published_handle(&self) -> Arc<RwLock<Arc<Published>>> {
        self.published.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    pub fn write_log(&self) -> Arc<WriteLog> {
        self.write_log.clone()
    }

    pub fn data_dir(&self) -> PathBuf {
        self.lock().options.data_dir.clone()
    }

    /// A copy of the settings.
    pub fn settings(&self) -> Settings {
        self.lock().settings.clone()
    }

    /// Changes the settings, saves them, and republishes what depends on them. `f` returns
    /// whether anything changed.
    pub fn update_settings<R>(&self, f: impl FnOnce(&mut Settings) -> Result<R>) -> Result<R> {
        let mut core = self.lock();
        let mut settings = core.settings.clone();
        let result = f(&mut settings)?;
        if settings != core.settings {
            settings.save(&core.options.data_dir)?;
            core.settings = settings;
            core.publish_auth();
        }
        Ok(result)
    }

    /// Sets the display name (`None` for the host name).
    pub fn set_name(&self, name: Option<String>) -> Result<()> {
        self.update_settings(|settings| {
            settings.name = name.filter(|n| !n.trim().is_empty());
            Ok(())
        })
    }

    // MARK: Shared folders

    pub fn shared_folders(&self) -> Vec<SharedFolderStatus> {
        let core = self.lock();
        core.settings
            .shared_folders
            .iter()
            .map(|folder| core.folder_status(folder))
            .collect()
    }

    /// Shares a folder. Returns its new UUID, or the existing one if it's already shared.
    pub fn add_shared_folder(&self, path: &Path, name: Option<String>) -> Result<Uuid> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("can't open {}", path.display()))?;
        anyhow::ensure!(canonical.is_dir(), "{} isn't a folder", path.display());
        let mut core = self.lock();
        for folder in &core.settings.shared_folders {
            let Ok(existing) = folder.path.canonicalize() else {
                continue;
            };
            if existing == canonical {
                return Ok(folder.id);
            }
            if existing.starts_with(&canonical) || canonical.starts_with(&existing) {
                bail!(
                    "{} overlaps the shared folder {}",
                    path.display(),
                    folder.path.display()
                );
            }
        }
        let base = name.unwrap_or_else(|| {
            canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Folder".into())
        });
        let folder = SharedFolderSettings {
            id: Uuid::new_v4(),
            path: canonical.clone(),
            name: core.settings.unique_folder_name(&base),
            volume: VolumeIdentity::of(&canonical)?,
        };
        let id = folder.id;
        core.folders.insert(id, FolderState::new(&folder, None));
        core.settings.shared_folders.push(folder);
        core.save_settings();
        core.publish_due = true;
        drop(core);
        self.kick();
        Ok(id)
    }

    /// Stops sharing a folder. Its files leave the index.
    pub fn remove_shared_folder(&self, id: Uuid) -> Result<()> {
        let mut core = self.lock();
        let before = core.settings.shared_folders.len();
        core.settings
            .shared_folders
            .retain(|folder| folder.id != id);
        if core.settings.shared_folders.len() == before {
            bail!("no shared folder {id}");
        }
        core.settings.revs.files.remove(&id);
        core.folders.remove(&id);
        core.jobs.retain(|_, job| job.folder != id);
        core.store.remove_folder(id)?;
        core.save_settings();
        core.publish_due = true;
        drop(core);
        self.kick();
        Ok(())
    }

    /// Renames a shared folder's display name; only the label changes.
    pub fn rename_shared_folder(&self, id: Uuid, name: &str) -> Result<()> {
        let name = name.trim();
        anyhow::ensure!(!name.is_empty(), "a shared folder needs a name");
        let mut core = self.lock();
        let folder = core
            .settings
            .shared_folders
            .iter_mut()
            .find(|folder| folder.id == id)
            .with_context(|| format!("no shared folder {id}"))?;
        folder.name = name.to_string();
        core.save_settings();
        core.publish_due = true;
        drop(core);
        self.kick();
        Ok(())
    }

    /// Rescans one shared folder, or all of them.
    pub fn rescan(&self, folder: Option<Uuid>) {
        let mut core = self.lock();
        let ids: Vec<Uuid> = match folder {
            Some(id) => vec![id],
            None => core.settings.shared_folders.iter().map(|f| f.id).collect(),
        };
        for id in ids {
            if let Some(state) = core.folders.get_mut(&id) {
                state.last_volume_check = None;
            }
            core.schedule_scan(id, Duration::ZERO);
        }
        drop(core);
        self.kick();
    }

    /// "Reindex everything": re-reads every file's metadata in the background. It never changes
    /// `rev` or makes clients download media again.
    pub fn reindex_everything(&self) -> Result<()> {
        let mut core = self.lock();
        core.store.reset_reader_versions()?;
        for state in core.folders.values_mut() {
            // Every file is outdated now: the next batch takes a full count.
            state.outdated = 0;
            state.outdated_total = 0;
            if let Some(snapshot) = &mut state.snapshot {
                for record in snapshot.values_mut() {
                    record.reader_version = 0;
                    if matches!(record.status, ReadStatus::Temporary { .. }) {
                        record.status = ReadStatus::Read;
                    }
                }
            }
        }
        drop(core);
        self.kick();
        Ok(())
    }

    /// Whether no scan is running or due and nothing is being read. A folder whose last scan
    /// failed and whose next scan is only the delayed retry counts as settled: the caller needs
    /// to hear about the errors now.
    pub fn is_idle(&self) -> bool {
        let core = self.lock();
        let now = Instant::now();
        !core.publish_due
            && core.jobs.is_empty()
            && core.folders.values().all(|state| {
                let retry_only = state.failures > 0
                    && state.scan_due.is_some_and(|due| due > now + DEBOUNCE_CAP);
                state.idle()
                    && state.background.is_empty()
                    && (state.scan_due.is_none() || !state.volume_ok || retry_only)
            })
    }

    /// Waits until [`is_idle`](Self::is_idle) (for tests). Returns false on timeout.
    pub fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_idle() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stops the driver, the watcher and the reader threads.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(driver) = self.driver.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = driver.join();
        }
        self.queue.lock().shutdown = true;
        self.queue.ready.notify_all();
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(|e| e.into_inner())
}

fn run_volume_checks(
    checks: Vec<(Uuid, PathBuf, VolumeIdentity)>,
) -> Vec<(Uuid, Result<(), volume::Unavailable>)> {
    checks
        .into_iter()
        .map(|(id, path, identity)| (id, volume::check(&path, &identity)))
        .collect()
}

fn drive(core: Arc<Mutex<Core>>, rx: Receiver<Msg>) {
    loop {
        let wait = lock(&core).next_wakeup();
        let first = match rx.recv_timeout(wait) {
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
        };
        let checks = lock(&core).due_volume_checks(Instant::now());
        let results = run_volume_checks(checks);
        let mut core = lock(&core);
        for (id, result) in results {
            core.apply_volume_check(id, result);
        }
        if let Some(msg) = first {
            core.handle(msg);
        }
        let mut stop = false;
        while let Ok(msg) = rx.try_recv() {
            if matches!(msg, Msg::Shutdown) {
                stop = true;
                break;
            }
            core.handle(msg);
        }
        core.tick();
        if stop {
            break;
        }
    }
    let mut core = lock(&core);
    core.watcher = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::published::PublishedFolder;

    /// While a route holds the current view, no event announcing the next one may go out:
    /// the client hearing it would still be served the old ETag.
    #[test]
    fn events_go_out_only_after_the_new_view_is_visible() {
        let settings = Settings::new();
        let published = Arc::new(RwLock::new(Arc::new(Published::empty(&settings))));
        let (events, mut receiver) = broadcast::channel(16);
        let mut next = Published::empty(&settings);
        let folder = Uuid::new_v4();
        next.folders = Arc::new(vec![PublishedFolder {
            id: folder,
            name: "New".into(),
            available: false,
            root: PathBuf::from("/new"),
        }]);

        let reading = published.read().unwrap();
        let swapper = {
            let published = published.clone();
            std::thread::spawn(move || swap_and_announce(&published, &events, next))
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            receiver.try_recv().is_err(),
            "an event went out while the old view was still being served"
        );
        drop(reading);
        swapper.join().unwrap();
        assert_eq!(
            receiver.try_recv().unwrap(),
            ServerEvent::FolderAvailability {
                shared_folder_id: folder,
                available: false
            }
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            ServerEvent::SharedFoldersChanged
        );
        assert!(
            receiver.try_recv().is_err(),
            "the empty index didn't change"
        );
        assert_eq!(published.read().unwrap().folders.len(), 1);
    }
}
