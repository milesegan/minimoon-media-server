//! File-level change watching (spec §8, change detection, and §5, `rev`).
//!
//! The watcher reports which files were written, not only which folders changed, because
//! detecting an in-place write that restores a file's mtime needs the write event itself. On
//! macOS it's the server's own FSEvents stream, so it can resume from the last processed event
//! id after a restart; the `notify` crate's FSEvents backend always starts from "now". On Linux
//! (inotify) and Windows (`ReadDirectoryChangesW`) it's `notify`.

use anyhow::Result;
use std::path::PathBuf;

/// What the watcher saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something changed at `path`. `write` is set when a file's contents were written.
    Changed { path: PathBuf, write: bool },
    /// Events were dropped or a volume changed under `path`: rescan it by size and mtime.
    Rescan { path: PathBuf },
    /// macOS: the replay of events since the stored event id has finished.
    HistoryDone,
}

/// A batch of events, with the latest FSEvents event id it covers (macOS only).
#[derive(Clone, Debug, Default)]
pub struct Batch {
    pub events: Vec<WatchEvent>,
    pub latest_event_id: Option<u64>,
}

/// Receives batches on the watcher's own thread.
pub type Sink = Box<dyn Fn(Batch) + Send + Sync + 'static>;

/// A running watcher; dropping it stops watching.
pub struct Watcher {
    #[cfg(target_os = "macos")]
    inner: macos::Stream,
    #[cfg(not(target_os = "macos"))]
    _inner: notify::RecommendedWatcher,
}

impl Watcher {
    /// Starts watching `roots` recursively. On macOS, `since` resumes from a stored event id and
    /// replays what happened while the server wasn't running, as far as the system kept history.
    pub fn start(roots: &[PathBuf], since: Option<u64>, sink: Sink) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                inner: macos::Stream::start(roots, since, sink)?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = since;
            Ok(Self {
                _inner: portable::start(roots, sink)?,
            })
        }
    }

    /// The current FSEvents event id, to store when there's nothing left to process (macOS).
    pub fn current_event_id() -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: a plain query with no arguments.
            Some(unsafe { macos::fs::FSEventsGetCurrentEventId() })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Watcher {
    fn drop(&mut self) {
        self.inner.stop();
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{Batch, Sink, WatchEvent};
    use anyhow::{bail, Result};
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::runloop::{
        kCFRunLoopDefaultMode, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun, CFRunLoopStop,
    };
    use core_foundation::string::CFString;
    use std::ffi::{c_void, CStr};
    use std::os::raw::c_char;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    /// The parts of the FSEvents API the watcher uses (CoreServices, `FSEvents.h`).
    #[allow(non_upper_case_globals, non_snake_case)]
    pub(crate) mod fs {
        use core_foundation::array::CFArrayRef;
        use core_foundation::runloop::CFRunLoopRef;
        use core_foundation::string::CFStringRef;
        use std::ffi::c_void;

        pub type FSEventStreamRef = *mut c_void;
        pub type FSEventStreamEventFlags = u32;
        pub type FSEventStreamEventId = u64;
        pub type FSEventStreamCallback = extern "C" fn(
            FSEventStreamRef,
            *mut c_void,
            usize,
            *mut c_void,
            *const FSEventStreamEventFlags,
            *const FSEventStreamEventId,
        );

        #[repr(C)]
        pub struct FSEventStreamContext {
            pub version: isize,
            pub info: *mut c_void,
            pub retain: *const c_void,
            pub release: *const c_void,
            pub copy_description: *const c_void,
        }

        pub const kFSEventStreamEventIdSinceNow: FSEventStreamEventId = u64::MAX;
        pub const kFSEventStreamCreateFlagNoDefer: u32 = 0x02;
        pub const kFSEventStreamCreateFlagWatchRoot: u32 = 0x04;
        pub const kFSEventStreamCreateFlagFileEvents: u32 = 0x10;
        pub const kFSEventStreamEventFlagMustScanSubDirs: u32 = 0x01;
        #[cfg(test)]
        pub const kFSEventStreamEventFlagUserDropped: u32 = 0x02;
        pub const kFSEventStreamEventFlagHistoryDone: u32 = 0x10;
        pub const kFSEventStreamEventFlagRootChanged: u32 = 0x20;
        pub const kFSEventStreamEventFlagMount: u32 = 0x40;
        pub const kFSEventStreamEventFlagUnmount: u32 = 0x80;
        #[cfg(test)]
        pub const kFSEventStreamEventFlagItemInodeMetaMod: u32 = 0x400;
        pub const kFSEventStreamEventFlagItemModified: u32 = 0x1000;
        pub const kFSEventStreamEventFlagItemIsFile: u32 = 0x10000;

        #[link(name = "CoreServices", kind = "framework")]
        extern "C" {
            pub fn FSEventStreamCreate(
                allocator: *const c_void,
                callback: FSEventStreamCallback,
                context: *const FSEventStreamContext,
                paths_to_watch: CFArrayRef,
                since_when: FSEventStreamEventId,
                latency: f64,
                flags: u32,
            ) -> FSEventStreamRef;
            pub fn FSEventStreamScheduleWithRunLoop(
                stream: FSEventStreamRef,
                run_loop: CFRunLoopRef,
                run_loop_mode: CFStringRef,
            );
            pub fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
            pub fn FSEventStreamStop(stream: FSEventStreamRef);
            pub fn FSEventStreamInvalidate(stream: FSEventStreamRef);
            pub fn FSEventStreamRelease(stream: FSEventStreamRef);
            pub fn FSEventsGetCurrentEventId() -> FSEventStreamEventId;
        }
    }

    /// Seconds FSEvents waits to coalesce events.
    const LATENCY: f64 = 1.0;

    struct RunLoop(CFRunLoopRef);
    // SAFETY: CFRunLoopStop may be called from any thread; the pointer is only used for that.
    unsafe impl Send for RunLoop {}

    pub struct Stream {
        run_loop: Option<RunLoop>,
        thread: Option<JoinHandle<()>>,
    }

    impl Stream {
        pub fn start(roots: &[PathBuf], since: Option<u64>, sink: Sink) -> Result<Self> {
            let roots: Vec<String> = roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect();
            let (ready_tx, ready_rx) = mpsc::channel::<Result<RunLoop, String>>();
            let thread = std::thread::Builder::new()
                .name("mmp-fsevents".into())
                .spawn(move || run(roots, since, sink, ready_tx))?;
            match ready_rx.recv() {
                Ok(Ok(run_loop)) => Ok(Self {
                    run_loop: Some(run_loop),
                    thread: Some(thread),
                }),
                Ok(Err(message)) => {
                    let _ = thread.join();
                    bail!(message)
                }
                Err(_) => bail!("the FSEvents thread stopped before starting"),
            }
        }

        pub fn stop(&mut self) {
            if let Some(run_loop) = self.run_loop.take() {
                // SAFETY: the run loop belongs to the stream thread, which is still running it.
                unsafe { CFRunLoopStop(run_loop.0) };
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn run(
        roots: Vec<String>,
        since: Option<u64>,
        sink: Sink,
        ready: mpsc::Sender<Result<RunLoop, String>>,
    ) {
        let paths: Vec<CFString> = roots.iter().map(|root| CFString::new(root)).collect();
        let paths = CFArray::from_CFTypes(&paths);
        let info = Box::into_raw(Box::new(sink));
        let context = fs::FSEventStreamContext {
            version: 0,
            info: info.cast::<c_void>(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        let flags = fs::kFSEventStreamCreateFlagFileEvents
            | fs::kFSEventStreamCreateFlagNoDefer
            | fs::kFSEventStreamCreateFlagWatchRoot;
        // SAFETY: every argument is valid for the call; the context's info outlives the stream,
        // because it's freed only after the stream is released below.
        let stream = unsafe {
            fs::FSEventStreamCreate(
                std::ptr::null(),
                callback,
                &context,
                paths.as_concrete_TypeRef(),
                since.unwrap_or(fs::kFSEventStreamEventIdSinceNow),
                LATENCY,
                flags,
            )
        };
        if stream.is_null() {
            // SAFETY: info came from Box::into_raw and nothing else holds it.
            drop(unsafe { Box::from_raw(info) });
            let _ = ready.send(Err("can't create an FSEvents stream".into()));
            return;
        }
        // SAFETY: the stream is valid and scheduled on this thread's run loop, which runs below.
        unsafe {
            let run_loop = CFRunLoopGetCurrent();
            fs::FSEventStreamScheduleWithRunLoop(stream, run_loop, kCFRunLoopDefaultMode);
            if fs::FSEventStreamStart(stream) == 0 {
                fs::FSEventStreamInvalidate(stream);
                fs::FSEventStreamRelease(stream);
                drop(Box::from_raw(info));
                let _ = ready.send(Err("can't start the FSEvents stream".into()));
                return;
            }
            let _ = ready.send(Ok(RunLoop(run_loop)));
            CFRunLoopRun();
            fs::FSEventStreamStop(stream);
            fs::FSEventStreamInvalidate(stream);
            fs::FSEventStreamRelease(stream);
            drop(Box::from_raw(info));
        }
    }

    extern "C" fn callback(
        _stream: fs::FSEventStreamRef,
        info: *mut c_void,
        count: usize,
        paths: *mut c_void,
        flags: *const fs::FSEventStreamEventFlags,
        ids: *const fs::FSEventStreamEventId,
    ) {
        // SAFETY: FSEvents passes `count` C strings, flags and ids, and the info pointer given
        // at creation, which stays valid while the stream exists.
        let (sink, paths, flags, ids) = unsafe {
            (
                &*(info as *const Sink),
                std::slice::from_raw_parts(paths as *const *const c_char, count),
                std::slice::from_raw_parts(flags, count),
                std::slice::from_raw_parts(ids, count),
            )
        };
        let mut batch = Batch::default();
        for ((path, &flag), &id) in paths.iter().zip(flags).zip(ids) {
            // SAFETY: each path is a NUL-terminated string owned by FSEvents for this call.
            let path = PathBuf::from(
                unsafe { CStr::from_ptr(*path) }
                    .to_string_lossy()
                    .into_owned(),
            );
            batch.latest_event_id = Some(
                batch
                    .latest_event_id
                    .map_or(id, |latest: u64| latest.max(id)),
            );
            batch.events.push(event_for(path, flag));
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(batch)));
    }

    /// Maps one FSEvents event to a watch event.
    pub(super) fn event_for(path: PathBuf, flag: u32) -> WatchEvent {
        if flag & fs::kFSEventStreamEventFlagHistoryDone != 0 {
            return WatchEvent::HistoryDone;
        }
        if flag
            & (fs::kFSEventStreamEventFlagMustScanSubDirs
                | fs::kFSEventStreamEventFlagRootChanged
                | fs::kFSEventStreamEventFlagMount
                | fs::kFSEventStreamEventFlagUnmount)
            != 0
        {
            return WatchEvent::Rescan { path };
        }
        let write = flag & fs::kFSEventStreamEventFlagItemModified != 0
            && flag & fs::kFSEventStreamEventFlagItemIsFile != 0;
        WatchEvent::Changed { path, write }
    }
}

#[cfg(not(target_os = "macos"))]
mod portable {
    use super::{Batch, Sink, WatchEvent};
    use anyhow::Result;
    use notify::event::{AccessKind, AccessMode, ModifyKind};
    use notify::{Event, EventKind, RecursiveMode, Watcher as _};
    use std::path::PathBuf;

    pub fn start(roots: &[PathBuf], sink: Sink) -> Result<notify::RecommendedWatcher> {
        let all_roots = roots.to_vec();
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let batch = match result {
                Ok(event) => Batch {
                    events: events_for(&event, &all_roots),
                    latest_event_id: None,
                },
                Err(error) => {
                    tracing::warn!("file watcher error, rescanning: {error}");
                    Batch {
                        events: all_roots
                            .iter()
                            .map(|root| WatchEvent::Rescan { path: root.clone() })
                            .collect(),
                        latest_event_id: None,
                    }
                }
            };
            if !batch.events.is_empty() {
                sink(batch);
            }
        })?;
        for root in roots {
            if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                tracing::warn!("can't watch {}: {error}", root.display());
            }
        }
        Ok(watcher)
    }

    fn is_write(kind: &EventKind) -> bool {
        match kind {
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
            EventKind::Modify(ModifyKind::Data(_)) => true,
            // ReadDirectoryChangesW reports a last-write change as a plain modification.
            EventKind::Modify(ModifyKind::Any) => cfg!(windows),
            _ => false,
        }
    }

    fn events_for(event: &Event, roots: &[PathBuf]) -> Vec<WatchEvent> {
        if event.need_rescan() {
            let paths = if event.paths.is_empty() {
                roots.to_vec()
            } else {
                event.paths.clone()
            };
            return paths
                .into_iter()
                .map(|path| WatchEvent::Rescan { path })
                .collect();
        }
        if matches!(
            event.kind,
            EventKind::Access(AccessKind::Open(_) | AccessKind::Read)
        ) || matches!(
            event.kind,
            EventKind::Access(AccessKind::Close(AccessMode::Read))
        ) {
            return Vec::new();
        }
        let write = is_write(&event.kind);
        event
            .paths
            .iter()
            .map(|path| WatchEvent::Changed {
                path: path.clone(),
                write,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[cfg(target_os = "macos")]
    #[test]
    fn maps_fsevents_flags() {
        use macos::fs;
        let path = PathBuf::from("/x/a.flac");
        assert_eq!(
            macos::event_for(
                path.clone(),
                fs::kFSEventStreamEventFlagItemModified | fs::kFSEventStreamEventFlagItemIsFile
            ),
            WatchEvent::Changed {
                path: path.clone(),
                write: true
            }
        );
        assert_eq!(
            macos::event_for(
                path.clone(),
                fs::kFSEventStreamEventFlagItemInodeMetaMod | fs::kFSEventStreamEventFlagItemIsFile
            ),
            WatchEvent::Changed {
                path: path.clone(),
                write: false
            }
        );
        assert_eq!(
            macos::event_for(
                path.clone(),
                fs::kFSEventStreamEventFlagMustScanSubDirs | fs::kFSEventStreamEventFlagUserDropped
            ),
            WatchEvent::Rescan { path }
        );
    }

    /// A real write that restores the old mtime is still reported as a write.
    #[test]
    fn reports_an_in_place_write_that_restores_the_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("a.flac");
        std::fs::write(&file, b"before").unwrap();
        let original = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(300));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let _watcher = Watcher::start(
            std::slice::from_ref(&root),
            None,
            Box::new(move |batch| sink_seen.lock().unwrap().extend(batch.events)),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));

        std::fs::write(&file, b"after!").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(original)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            original
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = seen.lock().unwrap().iter().any(
                |event| matches!(event, WatchEvent::Changed { path, write: true } if path == &file),
            );
            if found {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no write event: {:?}",
                seen.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
