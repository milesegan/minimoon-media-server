//! Pairing, tokens and signed media URLs (spec §4, protocol 1.1).
//!
//! The server shows a six-digit code; the client sends it back once and gets a bearer token,
//! stored hashed in the settings. Players that can't set headers get signed URLs, bound to the
//! device and its generation number and checked on every request, so a revoked device's URLs
//! fail on their next request.

use super::{error, json_response, AppState};
use crate::settings::PairedDevice;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, RawQuery, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// How long a pairing code stays valid.
const PAIRING_LIFETIME: Duration = Duration::from_secs(300);
/// Wrong codes allowed before a pairing locks.
const MAX_WRONG_CODES: u32 = 5;
/// Pairing requests allowed per source address in `RATE_WINDOW`.
const RATE_LIMIT: usize = 10;
const RATE_WINDOW: Duration = Duration::from_secs(600);
/// How long a signed URL lasts.
pub const SIGNED_URL_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

/// A pairing waiting for its code, shown to the user by the desktop app or the CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingRequest {
    pub pairing_id: String,
    pub code: String,
    pub client_name: String,
    pub app: String,
    pub platform: String,
    pub expires_in: Duration,
}

struct Pending {
    request: PairingRequest,
    expires: Instant,
    wrong: u32,
}

/// Pairings in progress and the per-address rate limit.
pub struct Pairing {
    pending: Mutex<HashMap<String, Pending>>,
    recent: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    requests: broadcast::Sender<PairingRequest>,
}

impl Default for Pairing {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            recent: Mutex::new(HashMap::new()),
            requests: broadcast::channel(16).0,
        }
    }
}

impl Pairing {
    /// New pairing codes to show to the user.
    pub fn subscribe(&self) -> broadcast::Receiver<PairingRequest> {
        self.requests.subscribe()
    }

    fn allow(&self, address: IpAddr) -> bool {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        recent.retain(|_, times| {
            times.retain(|at| now.duration_since(*at) < RATE_WINDOW);
            !times.is_empty()
        });
        let times = recent.entry(address).or_default();
        if times.len() >= RATE_LIMIT {
            return false;
        }
        times.push(now);
        true
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    rand::random()
}

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 of a token, hex: what the settings store.
pub fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A new URL-signing key.
pub fn new_signing_key() -> String {
    base64url(&random_bytes::<32>())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PairBody {
    client_name: String,
    app: String,
    platform: String,
}

/// `POST /v1/pair`.
pub async fn pair(
    State(state): State<AppState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    body: Bytes,
) -> Response {
    let published = state.published();
    if published.auth.mode != crate::settings::AuthMode::Pairing {
        return error(
            StatusCode::BAD_REQUEST,
            "unsupported_feature",
            "this server doesn't require pairing",
        );
    }
    if !state.pairing.allow(address.ip()) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "pairing_locked",
            "too many pairing attempts from this address",
        );
    }
    let body: PairBody = serde_json::from_slice(&body).unwrap_or_default();
    let pairing_id = base64url(&random_bytes::<16>());
    let code = format!("{:06}", rand::random_range(0..1_000_000u32));
    let request = PairingRequest {
        pairing_id: pairing_id.clone(),
        code,
        client_name: body.client_name,
        app: body.app,
        platform: body.platform,
        expires_in: PAIRING_LIFETIME,
    };
    {
        let mut pending = state
            .pairing
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        pending.retain(|_, p| p.expires > now);
        pending.insert(
            pairing_id.clone(),
            Pending {
                request: request.clone(),
                expires: now + PAIRING_LIFETIME,
                wrong: 0,
            },
        );
    }
    tracing::info!(
        "pairing code {} for {} ({} on {})",
        request.code,
        request.client_name,
        request.app,
        request.platform
    );
    let _ = state.pairing.requests.send(request);
    json_response(
        StatusCode::OK,
        &serde_json::json!({"pairingId": pairing_id, "expiresIn": PAIRING_LIFETIME.as_secs()}),
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ConfirmBody {
    code: String,
}

/// `POST /v1/pair/{pairingId}/confirm`.
pub async fn confirm(
    State(state): State<AppState>,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Path(pairing_id): Path<String>,
    body: Bytes,
) -> Response {
    if !state.pairing.allow(address.ip()) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "pairing_locked",
            "too many pairing attempts from this address",
        );
    }
    let body: ConfirmBody = serde_json::from_slice(&body).unwrap_or_default();
    let request = {
        let mut pending = state
            .pairing
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(entry) = pending.get_mut(&pairing_id) else {
            return error(StatusCode::NOT_FOUND, "not_found", "no such pairing");
        };
        if entry.expires <= Instant::now() {
            pending.remove(&pairing_id);
            return error(
                StatusCode::GONE,
                "pairing_expired",
                "the pairing code expired",
            );
        }
        if entry.wrong >= MAX_WRONG_CODES {
            return error(
                StatusCode::FORBIDDEN,
                "pairing_locked",
                "too many wrong codes for this pairing",
            );
        }
        if body.code.trim() != entry.request.code {
            entry.wrong += 1;
            let code = if entry.wrong >= MAX_WRONG_CODES {
                "pairing_locked"
            } else {
                "pairing_code_wrong"
            };
            return error(StatusCode::FORBIDDEN, code, "wrong pairing code");
        }
        pending.remove(&pairing_id).expect("present").request
    };
    let token = base64url(&random_bytes::<32>());
    let device = PairedDevice {
        client_id: uuid::Uuid::new_v4().to_string(),
        client_name: request.client_name,
        app: request.app,
        platform: request.platform,
        token_hash: token_hash(&token),
        generation: 1,
        paired_at: crate::http::content::unix_now() * 1000,
        revoked: false,
    };
    let client_id = device.client_id.clone();
    // The settings live behind the library's lock, which a rebuild can hold for a while; wait
    // for it off the async workers.
    let library = state.library.clone();
    let saved = tokio::task::spawn_blocking(move || {
        library.update_settings(|settings| {
            if settings.pairing.signing_key.is_none() {
                settings.pairing.signing_key = Some(new_signing_key());
            }
            settings.pairing.devices.push(device);
            Ok(settings.server_id)
        })
    })
    .await
    .unwrap_or_else(|error| Err(anyhow::anyhow!("pairing task failed: {error}")));
    match saved {
        Ok(server_id) => json_response(
            StatusCode::OK,
            &serde_json::json!({"clientId": client_id, "token": token, "serverId": server_id}),
        ),
        Err(err) => {
            tracing::error!("can't save a paired device: {err:#}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "can't save the pairing",
            )
        }
    }
}

/// HMAC-SHA256 over the item id, client id, generation and expiry.
fn signature(key: &[u8], item: &str, client: &str, generation: u64, expires: u64) -> Vec<u8> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(format!("{item}\n{client}\n{generation}\n{expires}").as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// `GET /v1/items/{id}/content-url`.
pub async fn content_url(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let published = state.published();
    let client = match super::authorize(&published, &headers) {
        Ok(client) => client,
        Err(response) => return response,
    };
    if !published.items.contains_key(&id) {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    }
    let (Some(client_id), Some(key)) = (client, published.auth.signing_key.as_deref()) else {
        // Open mode: the plain content URL already works in any player.
        return json_response(
            StatusCode::OK,
            &serde_json::json!({"url": format!("/v1/items/{id}/content")}),
        );
    };
    let Some(device) = published
        .auth
        .devices
        .iter()
        .find(|d| d.client_id == client_id)
    else {
        return error(
            StatusCode::UNAUTHORIZED,
            "not_paired",
            "this device isn't paired",
        );
    };
    let expires = crate::http::content::unix_now() + SIGNED_URL_LIFETIME.as_secs();
    let sig = base64url(&signature(key, &id, &client_id, device.generation, expires));
    let url = format!(
        "/v1/c/{id}?client={client_id}&gen={}&exp={expires}&sig={sig}",
        device.generation
    );
    json_response(
        StatusCode::OK,
        &serde_json::json!({"url": url, "expiresIn": SIGNED_URL_LIFETIME.as_secs()}),
    )
}

/// `GET /v1/c/{id}?client=…&gen=…&exp=…&sig=…`: a signed content URL.
pub async fn signed_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let published = state.published();
    let Some(item) = published.items.get(&id).cloned() else {
        return error(StatusCode::NOT_FOUND, "not_found", "no such item");
    };
    let head = method == Method::HEAD;
    if published.auth.mode != crate::settings::AuthMode::Pairing {
        // Pairing is off: the server ignores tokens and signatures.
        return super::content::serve(&state, &item, &headers, head, None).await;
    }
    let params: HashMap<String, String> = query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect();
    let (Some(client), Some(generation), Some(expires), Some(sig)) = (
        params.get("client"),
        params.get("gen").and_then(|g| g.parse::<u64>().ok()),
        params.get("exp").and_then(|e| e.parse::<u64>().ok()),
        params.get("sig"),
    ) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "not_paired",
            "the URL isn't signed",
        );
    };
    let Some(key) = published.auth.signing_key.as_deref() else {
        return error(
            StatusCode::UNAUTHORIZED,
            "not_paired",
            "the URL isn't signed",
        );
    };
    let Ok(sig) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig) else {
        return error(StatusCode::UNAUTHORIZED, "not_paired", "bad signature");
    };
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(format!("{id}\n{client}\n{generation}\n{expires}").as_bytes());
    if mac.verify_slice(&sig).is_err() {
        return error(StatusCode::UNAUTHORIZED, "not_paired", "bad signature");
    }
    let Some(device) = published
        .auth
        .devices
        .iter()
        .find(|d| &d.client_id == client)
    else {
        return error(
            StatusCode::UNAUTHORIZED,
            "not_paired",
            "this device isn't paired",
        );
    };
    if device.revoked || device.generation != generation {
        return error(
            StatusCode::UNAUTHORIZED,
            "token_revoked",
            "this device was revoked",
        );
    }
    if expires < crate::http::content::unix_now() {
        return error(StatusCode::UNAUTHORIZED, "url_expired", "the URL expired");
    }
    super::content::serve(&state, &item, &headers, head, Some(client.clone())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_bind_every_field() {
        let key = b"key";
        let base = signature(key, "i1", "c1", 1, 100);
        assert_eq!(base, signature(key, "i1", "c1", 1, 100));
        assert_ne!(base, signature(key, "i2", "c1", 1, 100));
        assert_ne!(base, signature(key, "i1", "c2", 1, 100));
        assert_ne!(base, signature(key, "i1", "c1", 2, 100));
        assert_ne!(base, signature(key, "i1", "c1", 1, 101));
        assert_ne!(base, signature(b"other", "i1", "c1", 1, 100));
    }

    #[test]
    fn rate_limits_per_address() {
        let pairing = Pairing::default();
        let address: IpAddr = "192.168.1.9".parse().unwrap();
        for _ in 0..RATE_LIMIT {
            assert!(pairing.allow(address));
        }
        assert!(!pairing.allow(address));
        assert!(pairing.allow("192.168.1.10".parse().unwrap()));
    }
}
