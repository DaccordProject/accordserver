use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::menu::MenuItem;
use tauri::{AppHandle, Wry};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

/// How long to wait after launch before the first update check, giving the
/// sidecars time to settle.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);
/// Interval between background update checks while the app keeps running.
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Cross-process update status. Serialised to `update_status.json` in the data
/// directory so the bundled accordserver can surface it on its landing page,
/// and mirrored into the tray menu label.
#[derive(Clone, Serialize)]
pub struct UpdateState {
    /// One of: up_to_date | checking | available | downloading | ready | error.
    pub phase: String,
    pub current_version: String,
    pub new_version: Option<String>,
    pub message: Option<String>,
}

impl UpdateState {
    pub fn initial() -> Self {
        Self {
            phase: "up_to_date".into(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            new_version: None,
            message: None,
        }
    }

    /// Human-readable label for the tray menu item.
    pub fn tray_label(&self) -> String {
        match self.phase.as_str() {
            "checking" => "Checking for updates…".to_string(),
            "available" | "downloading" => match &self.new_version {
                Some(v) => format!("Downloading update v{v}…"),
                None => "Downloading update…".to_string(),
            },
            "ready" => match &self.new_version {
                Some(v) => format!("Update v{v} ready — restart to apply"),
                None => "Update ready — restart to apply".to_string(),
            },
            "error" => "Update check failed — click to retry".to_string(),
            _ => "Check for updates".to_string(),
        }
    }
}

/// Shared, mutable update status plus the handles needed to reflect it.
pub struct UpdateManager {
    pub state: Mutex<UpdateState>,
    pub status_path: PathBuf,
    pub tray_item: MenuItem<Wry>,
    /// Set once an update has been downloaded and installed; a restart is
    /// required before another check is meaningful.
    pub pending_restart: Mutex<bool>,
}

impl UpdateManager {
    pub fn new(status_path: PathBuf, tray_item: MenuItem<Wry>) -> Self {
        Self {
            state: Mutex::new(UpdateState::initial()),
            status_path,
            tray_item,
            pending_restart: Mutex::new(false),
        }
    }

    /// Write the initial "up to date" status synchronously at startup so the
    /// server can read it before the first async check runs.
    pub fn write_initial(&self) {
        self.write_status(&UpdateState::initial());
    }

    async fn apply(&self, phase: &str, new_version: Option<String>, message: Option<String>) {
        let snapshot = {
            let mut state = self.state.lock().await;
            state.phase = phase.to_string();
            state.new_version = new_version;
            state.message = message;
            state.clone()
        };
        if let Err(e) = self.tray_item.set_text(snapshot.tray_label()) {
            tracing::warn!("could not update tray label: {e}");
        }
        self.write_status(&snapshot);
    }

    fn write_status(&self, state: &UpdateState) {
        match serde_json::to_vec_pretty(state) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&self.status_path, bytes) {
                    tracing::warn!("could not write {:?}: {e}", self.status_path);
                }
            }
            Err(e) => tracing::warn!("could not serialise update status: {e}"),
        }
    }
}

/// Run a single update check. Downloads and installs in the background when a
/// newer version is published; the new version is used on the next launch.
pub async fn check_once(app: &AppHandle, mgr: &Arc<UpdateManager>) {
    if *mgr.pending_restart.lock().await {
        return;
    }

    mgr.apply("checking", None, None).await;

    let updater = match app.updater() {
        Ok(u) => u,
        Err(e) => {
            mgr.apply("error", None, Some(e.to_string())).await;
            return;
        }
    };

    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            mgr.apply("up_to_date", None, None).await;
            return;
        }
        Err(e) => {
            tracing::warn!("update check failed: {e}");
            mgr.apply("error", None, Some(e.to_string())).await;
            return;
        }
    };

    let version = update.version.clone();
    mgr.apply("downloading", Some(version.clone()), None).await;

    match update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
    {
        Ok(()) => {
            *mgr.pending_restart.lock().await = true;
            mgr.apply("ready", Some(version), None).await;
        }
        Err(e) => {
            tracing::warn!("update download/install failed: {e}");
            mgr.apply("error", Some(version), Some(e.to_string())).await;
        }
    }
}

/// Background loop: an initial delayed check followed by periodic checks until
/// an update has been staged (after which a restart is required).
pub async fn run(app: AppHandle, mgr: Arc<UpdateManager>) {
    tokio::time::sleep(FIRST_CHECK_DELAY).await;
    loop {
        check_once(&app, &mgr).await;
        if *mgr.pending_restart.lock().await {
            break;
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}
