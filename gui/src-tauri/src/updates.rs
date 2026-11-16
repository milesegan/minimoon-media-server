use std::time::Duration;
use tauri::menu::{Menu, MenuItem, HELP_SUBMENU_ID};
use tauri::{AppHandle, Builder, Manager, WebviewWindow, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::{oneshot, Mutex};

const CHECK_FOR_UPDATES: &str = "check-for-updates";
const TITLE: &str = "MiniMoon Media Server Update";

#[derive(Default)]
struct UpdateCheck(Mutex<()>);

pub(super) fn configure(builder: Builder<Wry>) -> Builder<Wry> {
    if tauri::is_dev() {
        return builder;
    }
    builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(UpdateCheck::default())
        .menu(|app| {
            let menu = Menu::default(app)?;
            if let Some(help) = menu
                .get(HELP_SUBMENU_ID)
                .and_then(|item| item.as_submenu().cloned())
            {
                help.prepend(&MenuItem::with_id(
                    app,
                    CHECK_FOR_UPDATES,
                    "Check for Updates…",
                    true,
                    None::<&str>,
                )?)?;
            }
            Ok(menu)
        })
        .on_menu_event(|app, event| {
            if event.id() == CHECK_FOR_UPDATES {
                check(app, true);
            }
        })
}

pub(super) fn check(app: &AppHandle, manual: bool) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let state = window.state::<UpdateCheck>();
        let Ok(_guard) = state.0.try_lock() else {
            return;
        };

        let result = async {
            window
                .updater_builder()
                .timeout(Duration::from_secs(30))
                .build()?
                .check()
                .await
        }
        .await;

        let update = match result {
            Ok(Some(update)) => update,
            Ok(None) => {
                if manual {
                    show_status(
                        &window,
                        "MiniMoon Media Server is up to date.",
                        MessageDialogKind::Info,
                    );
                }
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "couldn't check for updates");
                if manual {
                    show_status(
                        &window,
                        format!("Couldn't check for updates.\n\n{error}"),
                        MessageDialogKind::Error,
                    );
                }
                return;
            }
        };

        let (answer, confirmed) = oneshot::channel();
        window
            .dialog()
            .message(format!(
                "Version {} is available. The app will download it in the background, then restart the server and interrupt any active streams.",
                update.version
            ))
            .parent(&window)
            .title(TITLE)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Install and Restart".into(),
                "Later".into(),
            ))
            .show(move |install| { let _ = answer.send(install); });

        if confirmed.await != Ok(true) {
            return;
        }
        if let Err(error) = update.download_and_install(|_, _| {}, || {}).await {
            tracing::warn!(%error, "couldn't install update");
            show_status(
                &window,
                format!("Couldn't install the update.\n\n{error}"),
                MessageDialogKind::Error,
            );
            return;
        }
        window.app_handle().request_restart();
    });
}

fn show_status(window: &WebviewWindow, message: impl Into<String>, kind: MessageDialogKind) {
    window
        .dialog()
        .message(message)
        .parent(window)
        .title(TITLE)
        .kind(kind)
        .show(|_| {});
}
