//! What the desktop app's Rust side sends its window: the `status` command's result and the
//! `pairing-request` event, as serde structs.
//!
//! They're a crate of their own so their TypeScript can be generated without building Tauri:
//! `cargo test` writes it, with ts-rs, into `gui/src/backend/bindings`, for the window's code
//! to import. CI checks that what's committed is current.

use serde::Serialize;

/// Where the server is in starting up.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(
    test,
    derive(ts_rs::TS),
    ts(export, export_to = "../../src/backend/bindings/")
)]
pub enum StatusPhase {
    Starting,
    Failed,
    Running,
}

/// Everything the window shows, fetched with the `status` command once a second.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(
    test,
    derive(ts_rs::TS),
    ts(export, export_to = "../../src/backend/bindings/")
)]
pub struct Status {
    pub phase: StatusPhase,
    /// Why the server couldn't start, when `phase` is "failed".
    pub error: Option<String>,
    /// The data directory, with the settings and index.
    pub data_dir: String,
    /// Only while `phase` is "running".
    pub server: Option<ServerView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(
    test,
    derive(ts_rs::TS),
    ts(export, export_to = "../../src/backend/bindings/")
)]
pub struct ServerView {
    /// The name devices see.
    pub name: String,
    /// The saved name, or none for the host name.
    pub custom_name: Option<String>,
    /// This computer's own name, which devices see when there's no saved name.
    pub host_name: String,
    /// Whether new devices need a pairing code.
    pub pairing: bool,
    pub keep_awake: bool,
    pub paused: bool,
    /// Like "http://192.168.1.24:8787"; none while paused, since pausing closes the listener.
    pub address: Option<String>,
    /// Streams playing now.
    pub downloads: usize,
    pub folders: Vec<FolderView>,
    pub devices: Vec<DeviceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(
    test,
    derive(ts_rs::TS),
    ts(export, export_to = "../../src/backend/bindings/")
)]
pub struct FolderView {
    pub id: String,
    pub name: String,
    pub path: String,
    /// Whether devices can see it now.
    pub shared: bool,
    /// Why it isn't shared: "Not shared until it's indexed", "Not shared: …" or "Offline: …".
    pub note: Option<String>,
    /// What indexing is doing now, like "Reading new files".
    pub activity: Option<String>,
    /// Done and total for `activity`, when it's countable.
    pub progress: Option<(usize, usize)>,
    pub files: usize,
    /// Files whose tags or video details couldn't be read.
    pub unreadable: usize,
    /// "path: message" for each item the last scan couldn't read.
    pub scan_errors: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(
    test,
    derive(ts_rs::TS),
    ts(export, export_to = "../../src/backend/bindings/")
)]
pub struct DeviceView {
    pub client_id: String,
    pub client_name: String,
    /// The client app, like "MiniMoon".
    pub app: String,
    /// Free-form, as the app reports it: "iOS", "tvOS", "Android TV", "macOS"…
    pub platform: String,
    /// Milliseconds since the Unix epoch.
    #[cfg_attr(test, ts(type = "number"))]
    pub paired_at: u64,
}

/// The `pairing-request` event: a device asking to connect, and the code to enter on it.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(
    test,
    derive(ts_rs::TS),
    ts(
        export,
        export_to = "../../src/backend/bindings/",
        rename = "PairingRequest"
    )
)]
pub struct PairingEvent {
    pub pairing_id: String,
    /// Six digits.
    pub code: String,
    pub client_name: String,
    pub app: String,
    pub platform: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub expires_in_secs: u64,
}
