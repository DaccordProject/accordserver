use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::routing::get;
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::signal;

use crate::config::SfuConfig;
use crate::sfu_client::SfuClient;

#[derive(Clone)]
struct SfuState {
    node_id: String,
    region: String,
    current_load: Arc<AtomicI64>,
}

async fn health(
    axum::extract::State(state): axum::extract::State<SfuState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "node_id": state.node_id,
        "region": state.region,
        "current_load": state.current_load.load(Ordering::Relaxed),
    }))
}

pub async fn run(config: SfuConfig, port: u16) {
    let main_url = config.main_url.clone();
    let node_id = config.node_id.clone();

    let client = SfuClient::new(
        config.main_url,
        config.node_id,
        config.endpoint,
        config.region.clone(),
        config.capacity,
    );

    // Register with exponential backoff
    let mut delay = std::time::Duration::from_secs(1);
    let max_delay = std::time::Duration::from_secs(30);
    loop {
        match client.register().await {
            Ok(()) => {
                tracing::info!("registered with main server as '{}'", node_id);
                break;
            }
            Err(e) => {
                tracing::warn!(
                    "failed to register with main server: {e}, retrying in {:?}",
                    delay
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(max_delay);
            }
        }
    }

    let state = SfuState {
        node_id: node_id.clone(),
        region: config.region,
        current_load: Arc::new(AtomicI64::new(0)),
    };

    let app = Router::new()
        .route("/health", get(health))
        .with_state(state.clone());

    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("failed to bind SFU listener");

    eprintln!("  \x1b[32m→ listening on 0.0.0.0:{port}\x1b[0m");
    eprintln!();

    // Spawn heartbeat task
    let hb_load = state.current_load.clone();
    let hb_interval = config.heartbeat_interval_secs;
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(hb_interval));
        loop {
            interval.tick().await;
            let load = hb_load.load(Ordering::Relaxed);
            if let Err(e) = client.heartbeat(load).await {
                tracing::warn!("heartbeat failed: {e}");
            }
        }
    });

    // Serve until shutdown signal
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("SFU server error");

    // Graceful shutdown: abort heartbeat, deregister
    heartbeat_handle.abort();
    tracing::info!("shutting down SFU node '{}'", node_id);

    let deregister_client =
        SfuClient::new(main_url, node_id.clone(), String::new(), String::new(), 0);
    if let Err(e) = deregister_client.deregister().await {
        tracing::warn!("failed to deregister on shutdown: {e}");
    } else {
        tracing::info!("deregistered SFU node '{}'", node_id);
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
