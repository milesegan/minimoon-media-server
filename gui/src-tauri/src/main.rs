//! A desktop app around the media server: shared folders, access, pairing and devices.
//!
//! The server runs in this process, embedded through `MediaServer`, with the same data
//! directory as the CLI by default. Don't run both at once: they'd share settings and port.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{bail, Context, Result};
use minimoon_media_server::library::NOT_INDEXED_YET;
use minimoon_media_server::library::{IndexActivity, LibraryOptions, SharedFolderStatus};
use minimoon_media_server::network::preferred_bind_ip;
use minimoon_media_server::settings::{
    default_data_dir, default_server_name, AuthMode, PairedDevice,
};
use minimoon_media_server::{MediaServer, ServerOptions};
use minimoon_media_server_gui_types::{
    DeviceView, FolderView, PairingEvent, ServerView, Status, StatusPhase,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, UserAttentionType, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::filter::LevelFilter;
use uuid::Uuid;

const USAGE: &str = "usage: minimoon-media-server-gui [--data-dir <dir>] [--port <port>]";

/// Where the server is in starting up.
enum Phase {
    Starting,
    Failed(String),
    Running(Arc<MediaServer>),
}

struct App {
    phase: Mutex<Phase>,
    data_dir: PathBuf,
}

impl App {
    fn server(&self) -> Result<Arc<MediaServer>, String> {
        match &*self.phase.lock().unwrap_or_else(|e| e.into_inner()) {
            Phase::Running(server) => Ok(server.clone()),
            Phase::Starting => Err("The server is still starting.".into()),
            Phase::Failed(error) => Err(error.clone()),
        }
    }
}

/// Runs `f` off the async workers: the library's lock can be held for a whole batch of reads.
async fn blocking<T: Send + 'static>(
    server: Arc<MediaServer>,
    f: impl FnOnce(&MediaServer) -> Result<T> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || f(&server))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

fn parse_id(id: &str) -> Result<Uuid, String> {
    id.parse().map_err(|_| format!("not a folder id: {id}"))
}

/// Asks a yes/no question with a sheet on `window`. Without a parent, macOS never shows it.
fn confirm(window: &WebviewWindow, title: &str, message: String, ok: &str) -> bool {
    window
        .dialog()
        .message(message)
        .parent(window)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            ok.into(),
            "Cancel".into(),
        ))
        .blocking_show()
}

// MARK: Status

fn folder_view(folder: SharedFolderStatus) -> FolderView {
    let note = (!folder.available).then(|| {
        let reason = folder.unavailable_reason.as_deref();
        match (folder.indexed, reason) {
            (false, None | Some(NOT_INDEXED_YET)) => "Not shared until it's indexed".into(),
            (false, Some(reason)) => format!("Not shared: {reason}"),
            (true, reason) => format!("Offline: {}", reason.unwrap_or("unknown")),
        }
    });
    let (activity, progress) = match folder.activity {
        IndexActivity::Reading { done, total } => (
            Some(if folder.indexed {
                "Reading new files"
            } else {
                "Indexing"
            }),
            Some((done, total)),
        ),
        IndexActivity::Listing => (
            Some(if folder.indexed {
                "Checking for changes"
            } else {
                "Listing files"
            }),
            None,
        ),
        IndexActivity::Idle if folder.outdated > 0 => {
            let total = folder.outdated_total.max(folder.outdated);
            (
                Some("Updating metadata"),
                Some((total - folder.outdated, total)),
            )
        }
        IndexActivity::Idle => (None, None),
    };
    FolderView {
        id: folder.id.to_string(),
        name: folder.name,
        path: folder.path.display().to_string(),
        shared: folder.available,
        note,
        activity: activity.map(String::from),
        progress,
        files: folder.files,
        unreadable: folder.unreadable,
        scan_errors: folder
            .scan_errors
            .iter()
            .map(|error| {
                format!(
                    "{}: {}",
                    folder.path.join(&error.path).display(),
                    error.message
                )
            })
            .collect(),
    }
}

fn device_view(device: PairedDevice) -> DeviceView {
    DeviceView {
        client_id: device.client_id,
        client_name: device.client_name,
        app: device.app,
        platform: device.platform,
        paired_at: device.paired_at,
    }
}

#[tauri::command]
async fn status(app: State<'_, App>) -> Result<Status, String> {
    let data_dir = app.data_dir.display().to_string();
    // One read of the phase, so startup finishing mid-call can't mix two states.
    let server = match &*app.phase.lock().unwrap_or_else(|e| e.into_inner()) {
        Phase::Running(server) => server.clone(),
        Phase::Starting => {
            return Ok(Status {
                phase: StatusPhase::Starting,
                error: None,
                data_dir,
                server: None,
            })
        }
        Phase::Failed(error) => {
            return Ok(Status {
                phase: StatusPhase::Failed,
                error: Some(error.clone()),
                data_dir,
                server: None,
            })
        }
    };
    let paused = server.is_paused().await;
    let address = server.local_addr().await.map(|address| {
        let ip = if address.ip().is_unspecified() {
            preferred_bind_ip()
                .map(|ip| ip.to_string())
                .unwrap_or_else(|_| "localhost".into())
        } else {
            address.ip().to_string()
        };
        format!("http://{ip}:{}", address.port())
    });
    let view = blocking(server, move |server| {
        let settings = server.library().settings();
        Ok(ServerView {
            name: settings.display_name(),
            custom_name: settings.name.clone(),
            host_name: default_server_name(),
            pairing: settings.auth == AuthMode::Pairing,
            keep_awake: settings.keep_awake,
            paused,
            address,
            downloads: server.active_downloads(),
            folders: server
                .library()
                .shared_folders()
                .into_iter()
                .map(folder_view)
                .collect(),
            // From the settings already copied, like `MediaServer::paired_devices`.
            devices: settings
                .pairing
                .devices
                .into_iter()
                .filter(|device| !device.revoked)
                .map(device_view)
                .collect(),
        })
    })
    .await?;
    Ok(Status {
        phase: StatusPhase::Running,
        error: None,
        data_dir,
        server: Some(view),
    })
}

// MARK: Controls

#[tauri::command]
async fn set_paused(app: State<'_, App>, paused: bool) -> Result<(), String> {
    app.server()?
        .set_paused(paused)
        .await
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn set_name(app: State<'_, App>, name: String) -> Result<(), String> {
    let name = Some(name.trim().to_string()).filter(|name| !name.is_empty());
    blocking(app.server()?, move |server| server.set_name(name)).await
}

#[tauri::command]
async fn set_pairing(app: State<'_, App>, on: bool) -> Result<(), String> {
    let mode = if on {
        AuthMode::Pairing
    } else {
        AuthMode::None
    };
    blocking(app.server()?, move |server| server.set_auth_mode(mode)).await
}

#[tauri::command]
async fn set_keep_awake(app: State<'_, App>, on: bool) -> Result<(), String> {
    blocking(app.server()?, move |server| server.set_keep_awake(on)).await
}

/// Asks for a folder and shares it. Returns whether one was chosen.
#[tauri::command]
async fn add_folder(window: WebviewWindow, app: State<'_, App>) -> Result<bool, String> {
    blocking(app.server()?, move |server| {
        let Some(path) = window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("Choose a Folder to Share")
            .blocking_pick_folder()
        else {
            return Ok(false);
        };
        let path = path.into_path().context("can't share that location")?;
        server.library().add_shared_folder(&path, None)?;
        Ok(true)
    })
    .await
}

#[tauri::command]
async fn rename_folder(app: State<'_, App>, id: String, name: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    blocking(app.server()?, move |server| {
        let name = name.trim();
        if name.is_empty() {
            bail!("a folder needs a name");
        }
        server.library().rename_shared_folder(id, name)
    })
    .await
}

/// Stops sharing a folder, after asking. Returns whether it was removed.
#[tauri::command]
async fn remove_folder(
    window: WebviewWindow,
    app: State<'_, App>,
    id: String,
) -> Result<bool, String> {
    let id = parse_id(&id)?;
    blocking(app.server()?, move |server| {
        let settings = server.library().settings();
        let folder = settings.folder(id).context("that folder isn't shared")?;
        let message = format!(
            "Devices will no longer see “{}”. Nothing on disk is changed.",
            folder.name
        );
        if !confirm(&window, "Stop Sharing?", message, "Stop Sharing") {
            return Ok(false);
        }
        server.library().remove_shared_folder(id)?;
        Ok(true)
    })
    .await
}

#[tauri::command]
async fn rescan(app: State<'_, App>, id: Option<String>) -> Result<(), String> {
    let id = id.as_deref().map(parse_id).transpose()?;
    blocking(app.server()?, move |server| {
        server.library().rescan(id);
        Ok(())
    })
    .await
}

/// Shows `path` in the platform's file manager.
fn reveal(path: &Path) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(opener)
        .arg(path)
        .status()
        .with_context(|| format!("can't run {opener}"))?;
    // Explorer exits with 1 even when it opened the folder.
    if !status.success() && !cfg!(windows) {
        bail!("can't open {}", path.display());
    }
    Ok(())
}

/// Opens a shared folder in the platform's file manager.
#[tauri::command]
async fn open_folder(app: State<'_, App>, id: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    blocking(app.server()?, move |server| {
        let settings = server.library().settings();
        let folder = settings.folder(id).context("that folder isn't shared")?;
        reveal(&folder.path)
    })
    .await
}

/// Opens the data directory, with the settings and index, in the file manager.
#[tauri::command]
async fn show_data_folder(app: State<'_, App>) -> Result<(), String> {
    let path = app.data_dir.clone();
    // Works before the server is up too: a failed start is when the data folder matters most.
    tauri::async_runtime::spawn_blocking(move || reveal(&path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn reindex_everything(app: State<'_, App>) -> Result<(), String> {
    blocking(app.server()?, |server| {
        server.library().reindex_everything()
    })
    .await
}

/// Revokes a paired device, after asking. Returns whether it was revoked.
#[tauri::command]
async fn revoke_device(
    window: WebviewWindow,
    app: State<'_, App>,
    client_id: String,
) -> Result<bool, String> {
    blocking(app.server()?, move |server| {
        let device = server
            .paired_devices()
            .into_iter()
            .find(|device| device.client_id == client_id)
            .context("that device isn't paired")?;
        let message = format!(
            "“{}” will need a new pairing code to connect again.",
            device.client_name
        );
        if !confirm(&window, "Revoke Device?", message, "Revoke") {
            return Ok(false);
        }
        server.revoke_device(&client_id)?;
        Ok(true)
    })
    .await
}

// MARK: Startup

/// Shows new pairing codes in the window.
fn forward_pairing_requests(handle: AppHandle, server: &MediaServer) {
    let mut requests = server.pairing_requests();
    tauri::async_runtime::spawn(async move {
        loop {
            let request = match requests.recv().await {
                Ok(request) => request,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            };
            let event = PairingEvent {
                pairing_id: request.pairing_id,
                code: request.code,
                client_name: request.client_name,
                app: request.app,
                platform: request.platform,
                expires_in_secs: request.expires_in.as_secs(),
            };
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.request_user_attention(Some(UserAttentionType::Critical));
            }
            let _ = handle.emit("pairing-request", event);
        }
    });
}

struct Args {
    data_dir: Option<PathBuf>,
    port: Option<u16>,
}

fn parse_args() -> Result<Args> {
    let mut parsed = Args {
        data_dir: None,
        port: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                parsed.data_dir = Some(args.next().context(USAGE)?.into());
            }
            "--port" => {
                parsed.port = Some(args.next().context(USAGE)?.parse().context(USAGE)?);
            }
            // macOS passes this when an unsigned app is launched from Finder.
            _ if arg.starts_with("-psn_") => {}
            _ => bail!("unknown argument {arg}\n\n{USAGE}"),
        }
    }
    Ok(parsed)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let filter = tracing_subscriber::filter::Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target("lofty", LevelFilter::ERROR)
        .with_target("mdns_sd", LevelFilter::ERROR);
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .init();

    let data_dir = match args.data_dir {
        Some(dir) => dir,
        None => default_data_dir()?,
    };
    let mut options = ServerOptions::new(LibraryOptions::new(&data_dir));
    if let Some(port) = args.port {
        options.port = port;
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(App {
            phase: Mutex::new(Phase::Starting),
            data_dir,
        })
        .invoke_handler(tauri::generate_handler![
            status,
            set_paused,
            set_name,
            set_pairing,
            set_keep_awake,
            add_folder,
            rename_folder,
            remove_folder,
            rescan,
            open_folder,
            show_data_folder,
            reindex_everything,
            revoke_device,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let phase = match MediaServer::start(options).await {
                    Ok(server) => {
                        forward_pairing_requests(handle.clone(), &server);
                        Phase::Running(Arc::new(server))
                    }
                    Err(error) => {
                        tracing::error!("can't start the server: {error:#}");
                        Phase::Failed(format!("{error:#}"))
                    }
                };
                *handle.state::<App>().phase.lock().unwrap() = phase;
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .context("can't start the app")?;

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Ok(server) = handle.state::<App>().server() {
                tauri::async_runtime::block_on(server.shutdown());
            }
        }
    });
    Ok(())
}
