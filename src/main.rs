use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use accordserver::config::Config;
use accordserver::gateway::dispatcher::Dispatcher;
use accordserver::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "accordserver=debug,tower_http=debug".into()),
        )
        .init();

    let config = Config::from_env();
    print_banner(&config);
    run_main_server(config).await;
}

fn print_banner(config: &Config) {
    let version = env!("CARGO_PKG_VERSION");
    let voice = match &config.livekit {
        Some(lk) => format!("livekit ({}, ext: {})", lk.internal_url, lk.external_url),
        None => "disabled".to_string(),
    };

    eprintln!();
    eprintln!("  \x1b[1;36maccord\x1b[0m \x1b[2mv{version}\x1b[0m");
    eprintln!();
    eprintln!("  \x1b[2mport\x1b[0m         {}", config.port);
    eprintln!("  \x1b[2mdatabase\x1b[0m     {}", config.database_url);
    eprintln!("  \x1b[2mvoice\x1b[0m        {voice}");

    if config.test_mode {
        eprintln!();
        eprintln!("  \x1b[33m! test mode enabled\x1b[0m");
    }

    eprintln!();
}

async fn run_main_server(config: Config) {
    // Ensure the database directory exists before opening the pool.
    // The default DATABASE_URL uses a relative `data/` subfolder to keep
    // the database separate from the application binary.
    if let Some(path) = config.database_url
        .strip_prefix("sqlite:")
        .and_then(|s| s.split('?').next())
    {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    tracing::error!("failed to create database directory {:?}: {:?}", parent, e);
                }
            }
        }
    }

    let db = accordserver::db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

    let (dispatcher, gateway_tx) = Dispatcher::new();

    let livekit_client = match config.livekit.as_ref() {
        Some(lk) => {
            let client = accordserver::voice::livekit::LiveKitClient::new(
                &lk.internal_url,
                &lk.external_url,
                &lk.api_key,
                &lk.api_secret,
            );
            match client.check_connectivity().await {
                Ok(()) => {
                    eprintln!("  \x1b[32m✓ livekit reachable\x1b[0m");
                }
                Err(e) => {
                    eprintln!();
                    eprintln!("  \x1b[31m✗ livekit preflight failed\x1b[0m");
                    eprintln!("    {e}");
                    eprintln!();
                    eprintln!("  Voice will not work until LiveKit is reachable.");
                    eprintln!("  Check LIVEKIT_INTERNAL_URL and ensure the LiveKit server is running.");
                    eprintln!();
                }
            }
            Some(client)
        }
        None => None,
    };

    // Create storage directories
    let storage_path = config.storage_path.clone();
    for subdir in &["emojis", "sounds", "avatars", "icons", "banners"] {
        let dir = storage_path.join(subdir);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::error!("failed to create storage directory {:?}: {:?}", dir, e);
        }
    }

    let gateway_tx_arc = Arc::new(RwLock::new(Some(gateway_tx)));

    let settings = accordserver::db::settings::get_settings(&db)
        .await
        .unwrap_or_default();

    let state = AppState {
        db,
        voice_states: Arc::new(DashMap::new()),
        presences: Arc::new(DashMap::new()),
        dispatcher: Arc::new(RwLock::new(Some(dispatcher))),
        gateway_tx: gateway_tx_arc,
        test_mode: config.test_mode,
        livekit_client,
        rate_limits: Arc::new(DashMap::new()),
        storage_path,
        settings: Arc::new(ArcSwap::from_pointee(settings)),
    };

    // Ensure a default invite exists and display it
    match accordserver::db::invites::ensure_default_invite(&state.db).await {
        Ok(code) => {
            eprintln!("  \x1b[2minvite\x1b[0m       {code}");
        }
        Err(e) => {
            tracing::warn!("failed to create default invite: {:?}", e);
        }
    }

    let app = accordserver::routes::router(state);

    let listener = TcpListener::bind(("0.0.0.0", config.port))
        .await
        .expect("failed to bind");

    let actual_port = listener
        .local_addr()
        .expect("failed to get local address")
        .port();
    eprintln!("  \x1b[32m→ listening on 0.0.0.0:{actual_port}\x1b[0m");
    eprintln!();

    axum::serve(listener, app).await.expect("server error");
}
