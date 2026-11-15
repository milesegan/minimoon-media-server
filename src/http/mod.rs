//! The `/v1` routes (spec §3–§6).

// Handlers return a ready `Response` as their error so a failed check can be returned as is;
// boxing it would only add noise at every call site.
#![allow(clippy::result_large_err)]

pub mod art;
pub mod conditional;
pub mod content;
pub mod pairing;

use crate::library::{Library, ServerEvent, WriteLog};
use crate::published::Published;
use crate::settings::AuthMode;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::Stream;
use serde::Serialize;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

/// The protocol version this server speaks.
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;
/// Optional parts this server supports.
pub const FEATURES: &[&str] = &["art-resize", "events"];

/// What every route shares.
#[derive(Clone)]
pub struct AppState {
    pub published: Arc<RwLock<Arc<Published>>>,
    pub library: Arc<Library>,
    pub write_log: Arc<WriteLog>,
    pub downloads: Arc<content::Downloads>,
    pub pairing: Arc<pairing::Pairing>,
    pub art_cache_dir: PathBuf,
    pub software_version: String,
}

impl AppState {
    pub fn published(&self) -> Arc<Published> {
        self.published
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// A JSON response.
pub fn json_response(status: StatusCode, value: &impl Serialize) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_default();
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response()
}

/// An error in the protocol's shape: `{"error": {"code": …, "message": …}}`.
pub fn error(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(
        status,
        &serde_json::json!({"error": {"code": code, "message": message}}),
    )
}

/// Checks a request's bearer token in pairing mode. Returns the paired device's client id, or
/// `None` in open mode.
pub fn authorize(published: &Published, headers: &HeaderMap) -> Result<Option<String>, Response> {
    if published.auth.mode != AuthMode::Pairing {
        return Ok(None);
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let Some(token) = token else {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "not_paired",
            "this server requires pairing",
        ));
    };
    let hash = pairing::token_hash(token);
    match published.auth.devices.iter().find(|d| d.token_hash == hash) {
        Some(device) if device.revoked => Err(error(
            StatusCode::UNAUTHORIZED,
            "token_revoked",
            "this device was revoked",
        )),
        Some(device) => Ok(Some(device.client_id.clone())),
        None => Err(error(
            StatusCode::UNAUTHORIZED,
            "not_paired",
            "this device isn't paired",
        )),
    }
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/server", get(server_info))
        .route("/v1/index", get(library_index))
        .route("/v1/items/{id}/content", get(item_content))
        .route("/v1/items/{id}/content-url", get(pairing::content_url))
        .route("/v1/items/{id}/art", get(item_art))
        .route("/v1/items/{id}/lyrics", get(item_lyrics))
        .route("/v1/c/{id}", get(pairing::signed_content))
        .route("/v1/events", get(events))
        .route("/v1/pair", post(pairing::pair))
        .route("/v1/pair/{id}/confirm", post(pairing::confirm))
        .method_not_allowed_fallback(|| async {
            error(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "this route only answers GET and HEAD",
            )
        })
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "no such route") })
        .layer(axum::middleware::from_fn(log_request))
        .with_state(state)
}

async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let client = request
        .headers()
        .get("mmp-client")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let response = next.run(request).await;
    tracing::debug!("{method} {path} {} [{client}]", response.status().as_u16());
    response
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerInfo<'a> {
    id: Uuid,
    name: &'a str,
    auth: &'static str,
    protocol: Protocol,
    software: Software<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_folders: Option<Vec<FolderInfo<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<IndexInfo<'a>>,
}

#[derive(Serialize)]
struct Protocol {
    major: u32,
    minor: u32,
    features: &'static [&'static str],
}

#[derive(Serialize)]
struct Software<'a> {
    name: &'static str,
    version: &'a str,
}

#[derive(Serialize)]
struct FolderInfo<'a> {
    id: Uuid,
    name: &'a str,
    available: bool,
}

/// The current ETags of the full index and the file list.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexInfo<'a> {
    etag: &'a str,
    files_etag: &'a str,
}

/// `GET /v1/server`: never needs a token. An unpaired client in pairing mode gets it without
/// the folders and the index ETags.
async fn server_info(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let published = state.published();
    let paired = authorize(&published, &headers).is_ok();
    let info = ServerInfo {
        id: published.server_id,
        name: &published.name,
        auth: published.auth.mode.as_str(),
        protocol: Protocol {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            features: FEATURES,
        },
        software: Software {
            name: "minimoon-media-server",
            version: &state.software_version,
        },
        shared_folders: paired.then(|| {
            published
                .folders
                .iter()
                .map(|folder| FolderInfo {
                    id: folder.id,
                    name: &folder.name,
                    available: folder.available,
                })
                .collect()
        }),
        index: paired.then(|| IndexInfo {
            etag: &published.index.full.etag,
            files_etag: &published.index.files.etag,
        }),
    };
    let mut response = json_response(StatusCode::OK, &info);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|coding| {
            let mut parts = coding.split(';');
            let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let refused = parts.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            (name == "gzip" || name == "x-gzip" || name == "*") && !refused
        })
}

/// `GET /v1/index[?view=full|files]`: every item in every shared folder.
async fn library_index(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let published = state.published();
    if let Err(response) = authorize(&published, &headers) {
        return response;
    }
    let mut view = "full";
    for pair in query.as_deref().unwrap_or("").split('&') {
        if let Some(value) = pair.strip_prefix("view=") {
            view = match value {
                "full" => "full",
                "files" => "files",
                _ => {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "unsupported_feature",
                        "view must be full or files",
                    )
                }
            };
        }
    }
    let index = &published.index;
    let representation = if view == "files" {
        &index.files
    } else {
        &index.full
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::ETAG,
        HeaderValue::from_str(&representation.etag).expect("ETags are ASCII"),
    );
    response_headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if conditional::none_match(&headers, &representation.etag) {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = if accepts_gzip(&headers) {
        response_headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        let representation = if view == "files" {
            index.files.gzipped()
        } else {
            let index = index.clone();
            match tokio::task::spawn_blocking(move || index.full.gzipped()).await {
                Ok(body) => body,
                Err(_) => representation.plain.clone(),
            }
        };
        representation
    } else {
        representation.plain.clone()
    };
    response_headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
    if method == Method::HEAD {
        return (StatusCode::OK, response_headers).into_response();
    }
    (StatusCode::OK, response_headers, body).into_response()
}

async fn item_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let published = state.published();
    let client = match authorize(&published, &headers) {
        Ok(client) => client,
        Err(response) => return response,
    };
    let Some(item) = published.items.get(&id).cloned() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    };
    content::serve(&state, &item, &headers, method == Method::HEAD, client).await
}

async fn item_art(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let published = state.published();
    if let Err(response) = authorize(&published, &headers) {
        return response;
    }
    let Some(item) = published.items.get(&id).cloned() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    };
    art::art(
        &state,
        &id,
        &item,
        query.as_deref(),
        &headers,
        method == Method::HEAD,
    )
    .await
}

async fn item_lyrics(
    State(state): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let published = state.published();
    if let Err(response) = authorize(&published, &headers) {
        return response;
    }
    let Some(item) = published.items.get(&id).cloned() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    };
    art::lyrics(&state, &item, &headers, method == Method::HEAD).await
}

fn sse_event(event: &ServerEvent) -> Event {
    match event {
        ServerEvent::IndexChanged { etag, files_etag } => Event::default()
            .event("index-changed")
            .data(serde_json::json!({"etag": etag, "filesEtag": files_etag}).to_string()),
        ServerEvent::FolderAvailability {
            shared_folder_id,
            available,
        } => Event::default().event("folder-availability").data(
            serde_json::json!({"sharedFolderId": shared_folder_id, "available": available})
                .to_string(),
        ),
        ServerEvent::SharedFoldersChanged => {
            Event::default().event("shared-folders-changed").data("{}")
        }
    }
}

/// `GET /v1/events`: Server-Sent Events. Events are hints; a client that falls behind gets
/// `shared-folders-changed` so it re-checks everything.
///
/// The stream closes when its device is revoked or the server pauses.
async fn events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let published = state.published();
    let client = match authorize(&published, &headers) {
        Ok(client) => client,
        Err(response) => return response,
    };
    let receiver = state.library.subscribe();
    let guard = state.downloads.start_stream(client.clone());
    if let Err(response) = content::still_paired(&state, client.as_deref()) {
        return response;
    }
    let closed = guard.token().clone().cancelled_owned();
    let stream = {
        use futures_util::StreamExt;
        event_stream(receiver).take_until(closed).map(move |event| {
            // Held for as long as the stream lives, so it stays registered.
            let _ = &guard;
            event
        })
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(20)))
        .into_response()
}

fn event_stream(
    receiver: tokio::sync::broadcast::Receiver<ServerEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
    use tokio_stream::StreamExt;
    tokio_stream::wrappers::BroadcastStream::new(receiver).map(|item| {
        Ok(match item {
            Ok(event) => sse_event(&event),
            Err(BroadcastStreamRecvError::Lagged(_)) => {
                sse_event(&ServerEvent::SharedFoldersChanged)
            }
        })
    })
}
