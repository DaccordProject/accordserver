use anyhow::{Context, Result};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;
use tokio::sync::Mutex;

use crate::app_config::AppConfig;
use crate::paths::AppPaths;

/// Tracks a supervised child process. The supervisor task respawns on
/// unexpected exit; `stop()` clears the desired-running flag and kills the
/// current child if any.
#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    name: &'static str,
    child: Mutex<Option<CommandChild>>,
    should_run: Mutex<bool>,
}

impl Supervisor {
    pub fn new(name: &'static str) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                name,
                child: Mutex::new(None),
                should_run: Mutex::new(true),
            }),
        }
    }

    pub async fn stop(&self) {
        *self.inner.should_run.lock().await = false;
        if let Some(child) = self.inner.child.lock().await.take() {
            let _ = child.kill();
        }
    }

    pub async fn is_running(&self) -> bool {
        self.inner.child.lock().await.is_some()
    }
}

/// Spawns and supervises the bundled `livekit-server` sidecar.
pub fn spawn_livekit(app: &AppHandle, paths: &AppPaths) -> Supervisor {
    let supervisor = Supervisor::new("livekit-server");
    let log_path = paths.livekit_log.clone();
    let yaml = paths.livekit_yaml.clone();
    let app = app.clone();
    let sup = supervisor.clone();

    tauri::async_runtime::spawn(async move {
        run_supervised(app, sup, log_path, None, move |app| {
            app.shell()
                .sidecar("livekit-server")
                .map(|cmd| cmd.args(["--config", yaml.to_string_lossy().as_ref()]))
        })
        .await;
    });

    supervisor
}

/// Spawns and supervises the bundled `accordserver` sidecar. The supervisor
/// task waits for LiveKit to bind `livekit_port` on localhost before its first
/// spawn attempt.
pub fn spawn_accordserver(app: &AppHandle, paths: &AppPaths, config: &AppConfig) -> Supervisor {
    let supervisor = Supervisor::new("accordserver");
    let log_path = paths.accord_log.clone();
    let data_dir = paths.data_dir.clone();
    let app = app.clone();
    let sup = supervisor.clone();
    let cfg = config.clone();

    tauri::async_runtime::spawn(async move {
        run_supervised(
            app,
            sup,
            log_path,
            Some(("127.0.0.1", cfg.livekit_port, Duration::from_secs(15))),
            move |app| {
                app.shell().sidecar("accordserver").map(|cmd| {
                    cmd.args([
                        "--data-dir",
                        data_dir.to_string_lossy().as_ref(),
                        "--port",
                        &cfg.port.to_string(),
                        "--livekit-url",
                        &cfg.livekit_url(),
                        "--livekit-key",
                        &cfg.livekit_api_key,
                        "--livekit-secret",
                        &cfg.livekit_api_secret,
                    ])
                    .env("TOTP_ENCRYPTION_KEY", &cfg.totp_encryption_key)
                })
            },
        )
        .await;
    });

    supervisor
}

async fn run_supervised<F>(
    app: AppHandle,
    supervisor: Supervisor,
    log_path: PathBuf,
    wait_for: Option<(&'static str, u16, Duration)>,
    build: F,
) where
    F: Fn(
        &AppHandle,
    ) -> std::result::Result<
        tauri_plugin_shell::process::Command,
        tauri_plugin_shell::Error,
    >,
{
    if let Some((host, port, timeout)) = wait_for {
        if let Err(e) = wait_until_port_open(host, port, timeout).await {
            tracing::warn!(
                "[{}] waited for {host}:{port} but timed out: {e}. Starting anyway.",
                supervisor.inner.name
            );
        }
    }

    let mut backoff_secs: u64 = 1;
    loop {
        if !*supervisor.inner.should_run.lock().await {
            break;
        }

        let cmd = match build(&app) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("[{}] failed to construct sidecar: {e}", supervisor.inner.name);
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        };

        let (mut rx, child) = match cmd.spawn() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!("[{}] spawn failed: {e}", supervisor.inner.name);
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        };

        *supervisor.inner.child.lock().await = Some(child);
        backoff_secs = 1;

        let mut log_file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(
                    "[{}] could not open log file {:?}: {e}",
                    supervisor.inner.name,
                    log_path
                );
                None
            }
        };

        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(line) | CommandEvent::Stderr(line) => {
                    if let Some(f) = log_file.as_mut() {
                        let _ = f.write_all(&line);
                        let _ = f.write_all(b"\n");
                    }
                }
                CommandEvent::Error(e) => {
                    tracing::warn!("[{}] error event: {e}", supervisor.inner.name);
                }
                CommandEvent::Terminated(payload) => {
                    tracing::info!(
                        "[{}] terminated (code={:?}, signal={:?})",
                        supervisor.inner.name,
                        payload.code,
                        payload.signal
                    );
                    break;
                }
                _ => {}
            }
        }

        *supervisor.inner.child.lock().await = None;

        if !*supervisor.inner.should_run.lock().await {
            break;
        }

        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

/// Polls a TCP port until it accepts a connection or `timeout` elapses. Used
/// to wait for LiveKit to bind before launching accordserver.
pub async fn wait_until_port_open(host: &str, port: u16, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    let addr = format!("{host}:{port}");
    loop {
        if start.elapsed() > timeout {
            anyhow::bail!("timed out waiting for {addr}");
        }
        match tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}
