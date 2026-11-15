//! Embedded artwork, resized images and lyrics (spec §6).
//!
//! `/art` returns an item's first embedded picture (a Matroska cover attachment or MP4 `covr`
//! atom for video); for an `image` item it's the image itself. With the `art-resize` feature,
//! `?maxSize=N` scales it to fit within N pixels as JPEG, cached in the server's data directory
//! under a key made of the item id, its content ETag, the size and the resizer's version.

use super::conditional::none_match;
use super::content::content_etag;
use super::{error, AppState};
use crate::model::Picture;
use crate::published::ServedItem;
use crate::reader::{self, ItemKind};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Bumped whenever resizing changes, so cached images are made again.
const RESIZER_VERSION: u32 = 1;
/// The largest `maxSize` accepted.
const MAX_SIZE: u32 = 4096;
/// How long reading or resizing art may take.
const ART_TIMEOUT: Duration = Duration::from_secs(30);
/// The resized-image cache is trimmed to this size.
const CACHE_LIMIT: u64 = 1024 * 1024 * 1024;
/// Trimming lists the whole cache, so it runs at most this often.
const TRIM_INTERVAL_SECS: u64 = 60;
/// When the cache was last trimmed, in seconds since the epoch.
static LAST_TRIM: AtomicU64 = AtomicU64::new(0);

struct Source {
    path: PathBuf,
    size: u64,
    mtime: std::time::SystemTime,
    mtime_ns: i64,
}

fn stat(path: PathBuf) -> Option<Source> {
    let metadata = std::fs::metadata(&path).ok()?;
    Some(Source {
        size: metadata.len(),
        mtime: metadata.modified().ok()?,
        mtime_ns: crate::scan::mtime_ns(&metadata)?,
        path,
    })
}

fn bytes_response(
    headers: &HeaderMap,
    etag: &str,
    last_modified: std::time::SystemTime,
    content_type: &str,
    body: Vec<u8>,
    head: bool,
) -> Response {
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).unwrap_or(HeaderValue::from_static("")),
    );
    response_headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&httpdate::fmt_http_date(last_modified))
            .unwrap_or(HeaderValue::from_static("")),
    );
    if none_match(headers, etag) {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
    if head {
        return (StatusCode::OK, response_headers).into_response();
    }
    (StatusCode::OK, response_headers, body).into_response()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::time::timeout(ART_TIMEOUT, tokio::task::spawn_blocking(f))
        .await
        .ok()?
        .ok()
}

fn parse_max_size(query: Option<&str>) -> Result<Option<u32>, Response> {
    let Some(value) = query
        .unwrap_or("")
        .split('&')
        .find_map(|pair| pair.strip_prefix("maxSize="))
    else {
        return Ok(None);
    };
    match value.parse::<u32>() {
        Ok(size) if (1..=MAX_SIZE).contains(&size) => Ok(Some(size)),
        _ => Err(error(
            StatusCode::BAD_REQUEST,
            "unsupported_feature",
            "maxSize must be a whole number of pixels from 1 to 4096",
        )),
    }
}

/// `GET /v1/items/{id}/art[?maxSize=N]`.
pub async fn art(
    state: &AppState,
    id: &str,
    item: &ServedItem,
    query: Option<&str>,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let max_size = match parse_max_size(query) {
        Ok(size) => size,
        Err(response) => return response,
    };
    let published = state.published();
    let path = match super::content::locate(item, published.folder(item.folder)) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let Some(source) = blocking(move || stat(path)).await.flatten() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    };
    let content = content_etag(source.size, source.mtime_ns, item.rev);
    let is_image = reader::file_type(crate::scan::split_name(&item.path)).kind == ItemKind::Image;

    let Some(max_size) = max_size else {
        if is_image {
            return super::content::serve(state, item, headers, head, None).await;
        }
        let etag = format!("\"{}-art\"", content.trim_matches('"'));
        if none_match(headers, &etag) {
            return bytes_response(headers, &etag, source.mtime, "", Vec::new(), head);
        }
        let reader = item.reader;
        let path = source.path.clone();
        return match blocking(move || reader::read_art(&path, reader)).await {
            Some(Ok(Some(picture))) => bytes_response(
                headers,
                &etag,
                source.mtime,
                &picture.mime,
                picture.data,
                head,
            ),
            _ => error(
                StatusCode::NOT_FOUND,
                "not_found",
                "this item has no embedded art",
            ),
        };
    };

    let etag = format!(
        "\"{}-art{max_size}-r{RESIZER_VERSION}\"",
        content.trim_matches('"')
    );
    if none_match(headers, &etag) {
        return bytes_response(headers, &etag, source.mtime, "", Vec::new(), head);
    }
    let cache_dir = state.art_cache_dir.clone();
    let key = cache_key(id, &content, max_size);
    let reader = item.reader;
    let path = source.path.clone();
    let resized = blocking(move || -> Option<Vec<u8>> {
        let cached = cache_dir.join(format!("{key}.jpg"));
        if let Ok(bytes) = std::fs::read(&cached) {
            return Some(bytes);
        }
        let picture = if is_image {
            Picture {
                data: std::fs::read(&path).ok()?,
                mime: String::new(),
            }
        } else {
            reader::read_art(&path, reader).ok()??
        };
        let jpeg = resize(&picture.data, max_size)?;
        if crate::settings::write_atomically(&cached, &jpeg).is_ok() && trim_due() {
            trim_cache(&cache_dir);
        }
        Some(jpeg)
    })
    .await
    .flatten();
    match resized {
        Some(jpeg) => bytes_response(headers, &etag, source.mtime, "image/jpeg", jpeg, head),
        None => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "this item has no art to resize",
        ),
    }
}

fn cache_key(id: &str, content_etag: &str, max_size: u32) -> String {
    let digest = Sha256::digest(format!(
        "{id}\n{content_etag}\n{max_size}\n{RESIZER_VERSION}"
    ));
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Scales an image to fit within `max_size` pixels (never enlarging it) and encodes it as JPEG.
fn resize(data: &[u8], max_size: u32) -> Option<Vec<u8>> {
    let image = image::load_from_memory(data).ok()?;
    let image = if image.width() > max_size || image.height() > max_size {
        image.resize(max_size, max_size, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    rgb.write_with_encoder(encoder).ok()?;
    Some(out)
}

/// Whether a trim is due, claiming it if so.
fn trim_due() -> bool {
    let now = super::content::unix_now();
    let last = LAST_TRIM.load(Ordering::Relaxed);
    now.saturating_sub(last) >= TRIM_INTERVAL_SECS
        && LAST_TRIM
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

/// Deletes the oldest cached images while the cache is over its limit.
fn trim_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            Some((metadata.modified().ok()?, metadata.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    if total <= CACHE_LIMIT {
        return;
    }
    files.sort();
    for (_, size, path) in files {
        if total <= CACHE_LIMIT * 9 / 10 {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

/// `GET /v1/items/{id}/lyrics`: embedded lyrics as plain text.
pub async fn lyrics(
    state: &AppState,
    item: &ServedItem,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let published = state.published();
    let path = match super::content::locate(item, published.folder(item.folder)) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let Some(source) = blocking(move || stat(path)).await.flatten() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    };
    let content = content_etag(source.size, source.mtime_ns, item.rev);
    let etag = format!("\"{}-lyrics\"", content.trim_matches('"'));
    let reader = item.reader;
    let path = source.path.clone();
    match blocking(move || reader::read_lyrics(&path, reader)).await {
        Some(Ok(Some(text))) => bytes_response(
            headers,
            &etag,
            source.mtime,
            "text/plain; charset=utf-8",
            text.into_bytes(),
            head,
        ),
        _ => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "this item has no embedded lyrics",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizes_to_fit_without_enlarging() {
        let image = image::RgbImage::from_pixel(800, 400, image::Rgb([200, 10, 10]));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let jpeg = resize(png.get_ref(), 200).unwrap();
        let out = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((out.width(), out.height()), (200, 100));
        let same = image::load_from_memory(&resize(png.get_ref(), 1000).unwrap()).unwrap();
        assert_eq!((same.width(), same.height()), (800, 400));
    }

    #[test]
    fn cache_keys_cover_every_input() {
        let base = cache_key("i", "\"1-2-0\"", 600);
        assert_ne!(base, cache_key("j", "\"1-2-0\"", 600));
        assert_ne!(base, cache_key("i", "\"1-2-1\"", 600));
        assert_ne!(base, cache_key("i", "\"1-2-0\"", 300));
    }

    #[test]
    fn validates_max_size() {
        assert_eq!(parse_max_size(Some("maxSize=600")).unwrap(), Some(600));
        assert_eq!(parse_max_size(None).unwrap(), None);
        assert!(parse_max_size(Some("maxSize=0")).is_err());
        assert!(parse_max_size(Some("maxSize=big")).is_err());
    }
}
