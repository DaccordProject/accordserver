#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app_config;
mod paths;
mod sidecar;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, Wry};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_opener::OpenerExt;

use crate::app_config::AppConfig;
use crate::paths::AppPaths;
use crate::sidecar::{spawn_accordserver, spawn_livekit, Supervisor};

struct AppState {
    paths: AppPaths,
    config: AppConfig,
    accord: Supervisor,
    livekit: Supervisor,
    autostart_item: CheckMenuItem<Wry>,
}

fn main() {
    let paths = match AppPaths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fatal: could not resolve app data directory: {e:#}");
            std::process::exit(1);
        }
    };
    if let Err(e) = paths.ensure_dirs() {
        eprintln!("fatal: could not create app dirs: {e:#}");
        std::process::exit(1);
    }

    init_tracing(&paths);

    let config = match AppConfig::load_or_init(&paths) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("could not load/initialise config: {e:#}");
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            let handle = app.handle().clone();

            let livekit = spawn_livekit(&handle, &paths);
            let accord = spawn_accordserver(&handle, &paths, &config);

            let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
            let autostart_item = CheckMenuItem::with_id(
                app,
                "autostart",
                "Start on login",
                true,
                autostart_enabled,
                None::<&str>,
            )?;

            let tray_menu = build_menu(app.handle(), &autostart_item)?;

            let state = AppState {
                paths: paths.clone(),
                config: config.clone(),
                accord,
                livekit,
                autostart_item,
            };
            app.manage(state);

            let mut tray = TrayIconBuilder::with_id("accord-tray")
                .tooltip("Accord server")
                .menu(&tray_menu)
                .show_menu_on_left_click(true)
                .on_menu_event(handle_menu_event);
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Tauri application")
        .run(|app_handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                let handle = app_handle.clone();
                tauri::async_runtime::block_on(async move {
                    if let Some(state) = handle.try_state::<AppState>() {
                        state.accord.stop().await;
                        state.livekit.stop().await;
                    }
                });
            }
        });
}

fn build_menu(app: &tauri::AppHandle, autostart: &CheckMenuItem<Wry>) -> tauri::Result<Menu<Wry>> {
    let open = MenuItem::with_id(app, "open", "Open in browser", true, None::<&str>)?;
    let data = MenuItem::with_id(app, "data", "Open data folder", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "View logs", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Accord", true, None::<&str>)?;

    Menu::with_items(app, &[&open, &data, &logs, &sep1, autostart, &sep2, &quit])
}

fn handle_menu_event(app: &tauri::AppHandle, event: tauri::menu::MenuEvent) {
    let state = match app.try_state::<AppState>() {
        Some(s) => s,
        None => return,
    };
    let id = event.id.as_ref();
    match id {
        "open" => {
            let url = format!("http://localhost:{}", state.config.port);
            if let Err(e) = app.opener().open_url(url, None::<&str>) {
                tracing::error!("open_url failed: {e}");
            }
        }
        "data" => {
            if let Err(e) = app
                .opener()
                .open_path(state.paths.data_dir.to_string_lossy(), None::<&str>)
            {
                tracing::error!("open_path failed: {e}");
            }
        }
        "logs" => {
            if let Err(e) = app
                .opener()
                .open_path(state.paths.accord_log.to_string_lossy(), None::<&str>)
            {
                tracing::error!("open_path failed: {e}");
            }
        }
        "autostart" => {
            let mgr = app.autolaunch();
            let now_enabled = match mgr.is_enabled() {
                Ok(true) => {
                    if let Err(e) = mgr.disable() {
                        tracing::error!("autostart disable failed: {e}");
                    }
                    false
                }
                _ => {
                    if let Err(e) = mgr.enable() {
                        tracing::error!("autostart enable failed: {e}");
                    }
                    true
                }
            };
            // Reflect the real state back into the checkbox.
            let actual = mgr.is_enabled().unwrap_or(now_enabled);
            if let Err(e) = state.autostart_item.set_checked(actual) {
                tracing::error!("could not update autostart checkbox: {e}");
            }
        }
        "quit" => {
            app.exit(0);
        }
        _ => {}
    }
}

fn init_tracing(paths: &AppPaths) {
    let file_appender = tracing_appender::rolling::daily(&paths.log_dir, "desktop.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);
    // Guard's lifetime must equal the process lifetime, otherwise the
    // non-blocking writer drops logs.
    Box::leak(Box::new(guard));

    tracing_subscriber::fmt()
        // Logs go to a file, never a terminal — ANSI colour codes would just
        // be unreadable escape sequences in the log.
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "accord_desktop=info,warn".into()),
        )
        .with_writer(writer)
        .init();
}
