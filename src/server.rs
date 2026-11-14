//! The running server: the library, the HTTP listener, Bonjour and the sleep assertion, with
//! the controls the desktop app and the CLI need (pause, pairing, devices).

use crate::bonjour::{Advertisement, Advertiser};
use crate::http::content::Downloads;
use crate::http::pairing::{new_signing_key, Pairing, PairingRequest};
use crate::http::{router, AppState};
use crate::library::{Library, LibraryOptions};
use crate::settings::{AuthMode, PairedDevice};
use crate::sleep::{self, Activity};
use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

/// How the server runs.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub library: LibraryOptions,
    /// 41325 by default; 0 picks a free port.
    pub port: u16,
    /// The address to listen on: every IPv4 interface by default.
    pub bind: IpAddr,
    /// Reported in `/v1/server` as `software.version`.
    pub software_version: String,
    /// Advertise over Bonjour.
    pub advertise: bool,
    /// How long the host stays awake after the last content response ends.
    pub sleep_grace: Duration,
}

impl ServerOptions {
    pub fn new(library: LibraryOptions) -> Self {
        Self {
            library,
            port: crate::network::listen_port(),
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            software_version: env!("CARGO_PKG_VERSION").to_string(),
            advertise: true,
            sleep_grace: Duration::from_secs(10 * 60),
        }
    }
}

struct Listener {
    address: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

/// A running media server.
pub struct MediaServer {
    library: Arc<Library>,
    state: AppState,
    options: ServerOptions,
    listener: tokio::sync::Mutex<Option<Listener>>,
    advertiser: Mutex<Option<Advertiser>>,
    activity: Arc<Activity>,
    sleep_stop: Arc<AtomicBool>,
}

impl MediaServer {
    /// Starts the library, the listener, Bonjour and the sleep thread.
    pub async fn start(options: ServerOptions) -> Result<Self> {
        let library_options = options.library.clone();
        let library =
            Arc::new(tokio::task::spawn_blocking(move || Library::start(library_options)).await??);
        let activity = Arc::new(Activity::new(options.sleep_grace));
        activity.set_keep_awake(library.settings().keep_awake);
        let sleep_stop = Arc::new(AtomicBool::new(false));
        sleep::spawn(activity.clone(), sleep_stop.clone());
        let state = AppState {
            published: library.published_handle(),
            library: library.clone(),
            write_log: library.write_log(),
            downloads: Arc::new(Downloads::new(activity.clone())),
            pairing: Arc::new(Pairing::default()),
            scan_options: library.scan_options(),
            art_cache_dir: library.data_dir().join("art-cache"),
            software_version: options.software_version.clone(),
        };
        let server = Self {
            library,
            state,
            options,
            listener: tokio::sync::Mutex::new(None),
            advertiser: Mutex::new(None),
            activity,
            sleep_stop,
        };
        server.listen().await?;
        Ok(server)
    }

    async fn listen(&self) -> Result<()> {
        let mut listener = self.listener.lock().await;
        if listener.is_some() {
            return Ok(());
        }
        let bind = SocketAddr::from((self.options.bind, self.options.port));
        let mut attempts = 0;
        let tcp = loop {
            match tokio::net::TcpListener::bind(bind).await {
                Ok(tcp) => break tcp,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempts < 50 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("can't listen on port {}", bind.port()))
                }
            }
        };
        let address = tcp.local_addr()?;
        let (shutdown, shutdown_rx) = oneshot::channel::<()>();
        let app = router(self.state.clone());
        let task = tokio::spawn(async move {
            let serve = axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                });
            if let Err(error) = serve.await {
                tracing::error!("HTTP server stopped: {error}");
            }
        });
        *listener = Some(Listener {
            address,
            shutdown,
            task,
        });
        drop(listener);
        self.readvertise();
        Ok(())
    }

    /// The address the server listens on (all interfaces), while it isn't paused.
    pub async fn local_addr(&self) -> Option<SocketAddr> {
        self.listener.lock().await.as_ref().map(|l| l.address)
    }

    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Updates the Bonjour advertisement to the current name and auth mode.
    pub fn readvertise(&self) {
        if !self.options.advertise {
            return;
        }
        let address = match self.listener.try_lock() {
            Ok(listener) => listener.as_ref().map(|l| l.address),
            Err(_) => return,
        };
        let mut advertiser = self.advertiser.lock().unwrap_or_else(|e| e.into_inner());
        let Some(address) = address else {
            if let Some(advertiser) = advertiser.as_mut() {
                advertiser.withdraw();
            }
            return;
        };
        let settings = self.library.settings();
        let advertisement = Advertisement {
            server_id: settings.server_id,
            name: settings.display_name(),
            auth: settings.auth.as_str(),
            port: address.port(),
        };
        if advertiser.is_none() {
            match Advertiser::new() {
                Ok(new) => *advertiser = Some(new),
                Err(error) => {
                    tracing::error!("can't start Bonjour: {error:#}");
                    return;
                }
            }
        }
        if let Err(error) = advertiser
            .as_mut()
            .expect("just created")
            .advertise(&advertisement)
        {
            tracing::error!("can't advertise over Bonjour: {error:#}");
        }
    }

    /// Pauses sharing: stops listening, closes downloads and withdraws the advertisement. The
    /// library keeps watching, so resuming is quick.
    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        if !paused {
            return self.listen().await;
        }
        let listener = self.listener.lock().await.take();
        if let Some(advertiser) = self
            .advertiser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            advertiser.withdraw();
        }
        self.state.downloads.cancel_all();
        if let Some(mut listener) = listener {
            let _ = listener.shutdown.send(());
            // Graceful shutdown waits for open responses; anything still open after this is
            // dropped, so a paused server serves nothing.
            if tokio::time::timeout(Duration::from_secs(5), &mut listener.task)
                .await
                .is_err()
            {
                listener.task.abort();
            }
        }
        Ok(())
    }

    pub async fn is_paused(&self) -> bool {
        self.listener.lock().await.is_none()
    }

    /// Sets the display name (`None` for the host name) and re-advertises.
    pub fn set_name(&self, name: Option<String>) -> Result<()> {
        self.library.set_name(name)?;
        self.readvertise();
        Ok(())
    }

    /// Turns pairing on or off (spec §4, "Switching modes").
    pub fn set_auth_mode(&self, mode: AuthMode) -> Result<()> {
        self.library.update_settings(|settings| {
            settings.auth = mode;
            if mode == AuthMode::Pairing && settings.pairing.signing_key.is_none() {
                settings.pairing.signing_key = Some(new_signing_key());
            }
            Ok(())
        })?;
        self.readvertise();
        Ok(())
    }

    /// New pairing codes to show the user.
    pub fn pairing_requests(&self) -> broadcast::Receiver<PairingRequest> {
        self.state.pairing.subscribe()
    }

    /// Paired devices that haven't been revoked.
    pub fn paired_devices(&self) -> Vec<PairedDevice> {
        self.library
            .settings()
            .pairing
            .devices
            .into_iter()
            .filter(|device| !device.revoked)
            .collect()
    }

    /// Revokes a device: its token gets `token_revoked`, its signed URLs stop working, and its
    /// open downloads are closed.
    pub fn revoke_device(&self, client_id: &str) -> Result<()> {
        self.library.update_settings(|settings| {
            let device = settings
                .pairing
                .devices
                .iter_mut()
                .find(|device| device.client_id == client_id && !device.revoked)
                .with_context(|| format!("no paired device {client_id}"))?;
            device.revoked = true;
            device.generation += 1;
            Ok(())
        })?;
        let closed = self.state.downloads.cancel_client(client_id);
        tracing::info!("revoked {client_id}; closed {closed} download(s)");
        Ok(())
    }

    /// "Keep this computer awake while sharing".
    pub fn set_keep_awake(&self, on: bool) -> Result<()> {
        self.library.update_settings(|settings| {
            settings.keep_awake = on;
            Ok(())
        })?;
        self.activity.set_keep_awake(on);
        Ok(())
    }

    /// Content responses streaming now.
    pub fn active_downloads(&self) -> usize {
        self.state.downloads.count()
    }

    /// Stops everything.
    pub async fn shutdown(&self) {
        let _ = self.set_paused(true).await;
        *self.advertiser.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.sleep_stop.store(true, Ordering::SeqCst);
        let library = self.library.clone();
        let _ = tokio::task::spawn_blocking(move || library.shutdown()).await;
    }
}
