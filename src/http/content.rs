//! File bytes: `GET /v1/items/{id}/content` and signed URLs (spec §6).
//!
//! - one representation per file, never content-coded, with a strong ETag built from the facts
//!   clients compare in the index: `"<size>-<mtime ms>-<rev>"`;
//! - single ranges in all three forms, `If-Range`, `If-None-Match`;
//! - the mid-transfer check: before the final chunk is sent, the file's size and mtime (full
//!   precision) are checked again, along with whether the watcher reported a write to it; if
//!   anything changed the connection is closed without that chunk, so the client sees an
//!   incomplete transfer rather than a silently mixed file (RFC 9112 §6.3).

use super::conditional::{if_range_allows, none_match, parse_range, RangeRequest};
use super::{error, AppState};
use crate::library::WriteLog;
use crate::published::{PublishedFolder, ServedItem};
use crate::scan;
use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// Bytes read from disk per chunk.
const CHUNK: usize = 256 * 1024;

/// Content responses and event streams open now, so revoking a device or pausing the server
/// closes them (spec §4), and the sleep assertion knows when something is playing (spec §8).
pub struct Downloads {
    next: AtomicU64,
    active: Mutex<HashMap<u64, Open>>,
    activity: Arc<crate::sleep::Activity>,
}

struct Open {
    client: Option<String>,
    token: CancellationToken,
    /// A content download, which keeps the host awake, rather than an event stream.
    download: bool,
}

impl Downloads {
    pub fn new(activity: Arc<crate::sleep::Activity>) -> Self {
        Self {
            next: AtomicU64::new(0),
            active: Mutex::new(HashMap::new()),
            activity,
        }
    }

    fn start(self: &Arc<Self>, client: Option<String>) -> DownloadGuard {
        self.register(client, true)
    }

    /// Registers an event stream, closed like a download but not keeping the host awake.
    pub fn start_stream(self: &Arc<Self>, client: Option<String>) -> DownloadGuard {
        self.register(client, false)
    }

    fn register(self: &Arc<Self>, client: Option<String>, download: bool) -> DownloadGuard {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id,
                Open {
                    client,
                    token: token.clone(),
                    download,
                },
            );
        if download {
            self.activity.started();
        }
        DownloadGuard {
            downloads: self.clone(),
            id,
            token,
            download,
        }
    }

    /// Closes every download a device has open.
    pub fn cancel_client(&self, client: &str) -> usize {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        let mut count = 0;
        for open in active.values() {
            if open.client.as_deref() == Some(client) {
                open.token.cancel();
                count += open.download as usize;
            }
        }
        count
    }

    /// Closes every download and event stream (pausing or stopping the server).
    pub fn cancel_all(&self) {
        for open in self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            open.token.cancel();
        }
    }

    /// Content downloads open now.
    pub fn count(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|open| open.download)
            .count()
    }
}

pub struct DownloadGuard {
    downloads: Arc<Downloads>,
    id: u64,
    token: CancellationToken,
    download: bool,
}

impl DownloadGuard {
    /// Cancelled when the device is revoked or the server pauses.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        self.downloads
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        if self.download {
            self.downloads.activity.finished();
        }
    }
}

/// A file opened for a response, with the facts its validators come from.
struct Opened {
    file: std::fs::File,
    size: u64,
    mtime: SystemTime,
    mtime_ns: i64,
    canonical: PathBuf,
}

fn open(path: PathBuf) -> std::io::Result<Opened> {
    let file = std::fs::File::open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::ErrorKind::NotFound.into());
    }
    Ok(Opened {
        size: metadata.len(),
        mtime: metadata.modified()?,
        mtime_ns: scan::mtime_ns(&metadata).unwrap_or(0),
        canonical: path.canonicalize().unwrap_or(path),
        file,
    })
}

/// The strong content ETag (spec §6).
pub fn content_etag(size: u64, mtime_ns: i64, rev: u64) -> String {
    format!("\"{size}-{}-{rev}\"", scan::ns_to_ms(mtime_ns))
}

/// Resolves an item to its file, answering `503 folder_unavailable` or `404` when it can't be
/// served.
pub fn locate(
    state: &AppState,
    item: &ServedItem,
    folder: Option<&PublishedFolder>,
) -> Result<PathBuf, Response> {
    let Some(folder) = folder else {
        return Err(error(StatusCode::NOT_FOUND, "not_found", "no such item"));
    };
    if !folder.available {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "folder_unavailable",
            "the item's shared folder is unavailable",
        ));
    }
    scan::resolve(&folder.root, &item.path, state.scan_options)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "not_found", "no such item"))
}

/// Answers a content request for an item.
pub async fn serve(
    state: &AppState,
    item: &ServedItem,
    headers: &HeaderMap,
    head: bool,
    client: Option<String>,
) -> Response {
    let published = state.published();
    let path = match locate(state, item, published.folder(item.folder)) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let opened = match tokio::task::spawn_blocking(move || open(path)).await {
        Ok(Ok(opened)) => opened,
        _ => return error(StatusCode::NOT_FOUND, "not_found", "no such item"),
    };
    let etag = content_etag(opened.size, opened.mtime_ns, item.rev);
    let last_modified = httpdate::fmt_http_date(opened.mtime);
    let content_type = item.mime.unwrap_or("application/octet-stream");

    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(header::ETAG, header_value(&etag));
    response_headers.insert(header::LAST_MODIFIED, header_value(&last_modified));

    // `If-Modified-Since` isn't honoured: the mtime can't show a change `rev` records.
    if none_match(headers, &etag) {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }

    let range = if if_range_allows(headers, &etag) {
        parse_range(headers, opened.size)
    } else {
        RangeRequest::Whole
    };
    let (status, start, length) = match range {
        RangeRequest::Whole => (StatusCode::OK, 0, opened.size),
        RangeRequest::Range { start, end } => {
            response_headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {start}-{end}/{}", opened.size)),
            );
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
        RangeRequest::Unsatisfiable => {
            response_headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes */{}", opened.size)),
            );
            let mut response = error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                "the range starts past the end of the file",
            );
            response.headers_mut().extend(response_headers);
            return response;
        }
    };
    response_headers.insert(header::CONTENT_TYPE, header_value(content_type));
    response_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));

    if head || length == 0 {
        let mut response = (status, response_headers).into_response();
        if !head {
            *response.body_mut() = Body::empty();
        }
        return response;
    }

    let guard = state.downloads.start(client.clone());
    // A revocation between the auth check and registering would have missed this download;
    // check again now that revoking would cancel it.
    if let Err(response) = still_paired(state, client.as_deref()) {
        return response;
    }
    let body = stream_file(opened, start, length, state.write_log.clone(), guard);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

/// Whether a device is still paired, checked after registering a response so that a revocation
/// in between can't be missed. `None` (open mode) always passes.
pub fn still_paired(state: &AppState, client: Option<&str>) -> Result<(), Response> {
    let Some(client) = client else {
        return Ok(());
    };
    let paired = state
        .published()
        .auth
        .devices
        .iter()
        .any(|device| device.client_id == client && !device.revoked);
    if paired {
        Ok(())
    } else {
        Err(error(
            StatusCode::UNAUTHORIZED,
            "token_revoked",
            "this device was revoked",
        ))
    }
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Why a transfer was cut short.
#[derive(Debug)]
struct Aborted(&'static str);

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Aborted {}

/// A content body being streamed.
struct Streaming {
    opened: Opened,
    position: u64,
    remaining: u64,
    write_log: Arc<WriteLog>,
    started_write: u64,
    guard: DownloadGuard,
}

/// Reads the next chunk, checking before the final one that the file didn't change since the
/// response started.
fn next_chunk(mut state: Streaming) -> (Streaming, Result<Bytes, Aborted>) {
    let size = usize::try_from(state.remaining.min(CHUNK as u64)).unwrap_or(CHUNK);
    let mut buffer = vec![0u8; size];
    let read = state
        .opened
        .file
        .seek(SeekFrom::Start(state.position))
        .and_then(|_| state.opened.file.read_exact(&mut buffer));
    if read.is_err() {
        // The file got shorter underneath us.
        return (state, Err(Aborted("the file changed during the transfer")));
    }
    state.position += size as u64;
    state.remaining -= size as u64;
    if state.remaining == 0
        && changed_since_start(&state.opened, &state.write_log, state.started_write)
    {
        tracing::info!(
            "{} changed during a transfer; closing without its last chunk",
            state.opened.canonical.display()
        );
        return (state, Err(Aborted("the file changed during the transfer")));
    }
    (state, Ok(Bytes::from(buffer)))
}

/// Streams `length` bytes from `start`.
///
/// Each chunk is read on the blocking pool only while it's being read, so a client that stops
/// reading holds no thread, and a cancelled download (a revoked device) fails on its next chunk.
fn stream_file(
    opened: Opened,
    start: u64,
    length: u64,
    write_log: Arc<WriteLog>,
    guard: DownloadGuard,
) -> Body {
    let started_write = write_log.last_write(&opened.canonical);
    let state = Streaming {
        opened,
        position: start,
        remaining: length,
        write_log,
        started_write,
        guard,
    };
    let stream = futures_util::stream::unfold(Some(state), |state| async move {
        let state = state?;
        if state.remaining == 0 {
            return None;
        }
        if state.guard.token.is_cancelled() {
            return Some((Err(Aborted("download closed")), None));
        }
        match tokio::task::spawn_blocking(move || next_chunk(state)).await {
            Ok((state, Ok(bytes))) => Some((Ok(bytes), Some(state))),
            Ok((_, Err(aborted))) => Some((Err(aborted), None)),
            Err(_) => Some((Err(Aborted("the read failed")), None)),
        }
    });
    Body::from_stream(stream)
}

fn changed_since_start(opened: &Opened, write_log: &WriteLog, started_write: u64) -> bool {
    let Ok(metadata) = opened.file.metadata() else {
        return true;
    };
    metadata.len() != opened.size
        || scan::mtime_ns(&metadata) != Some(opened.mtime_ns)
        || write_log.last_write(&opened.canonical) != started_write
}

/// Seconds since the epoch, for signed URL expiry.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}
