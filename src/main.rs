use dashmap::DashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use accordserver::config::{AccordMode, Config, VoiceBackend};
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

    match config.mode {
        AccordMode::Main => run_main_server(config).await,
        AccordMode::Sfu => run_sfu_node(config).await,
    }
}

fn print_banner(config: &Config) {
    let version = env!("CARGO_PKG_VERSION");
    let mode = match config.mode {
        AccordMode::Main => "main",
        AccordMode::Sfu => "sfu",
    };
    let voice = match (&config.voice_backend, &config.livekit) {
        (VoiceBackend::LiveKit, Some(lk)) => format!("livekit ({})", lk.url),
        _ => "custom sfu".to_string(),
    };

    eprintln!();
    eprintln!("  \x1b[1;36maccord\x1b[0m \x1b[2mv{version}\x1b[0m");
    eprintln!();
    eprintln!("  \x1b[2mmode\x1b[0m         {mode}");
    eprintln!("  \x1b[2mport\x1b[0m         {}", config.port);

    match config.mode {
        AccordMode::Main => {
            eprintln!("  \x1b[2mdatabase\x1b[0m     {}", config.database_url);
            eprintln!("  \x1b[2mvoice\x1b[0m        {voice}");
        }
        AccordMode::Sfu => {
            if let Some(ref sfu) = config.sfu {
                eprintln!("  \x1b[2mnode\x1b[0m         {}", sfu.node_id);
                eprintln!("  \x1b[2mregion\x1b[0m       {}", sfu.region);
                eprintln!("  \x1b[2mmain url\x1b[0m     {}", sfu.main_url);
                eprintln!("  \x1b[2mcapacity\x1b[0m     {}", sfu.capacity);
            }
        }
    }

    if config.test_mode {
        eprintln!();
        eprintln!("  \x1b[33m! test mode enabled\x1b[0m");
    }

    eprintln!();
}

async fn run_main_server(config: Config) {
    let db = accordserver::db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

    let (dispatcher, gateway_tx) = Dispatcher::new();

    let sfu_nodes = Arc::new(DashMap::new());
    if let Ok(nodes) = accordserver::db::sfu::load_all_online(&db).await {
        for node in nodes {
            sfu_nodes.insert(node.id.clone(), node);
        }
        if !sfu_nodes.is_empty() {
            tracing::info!("loaded {} SFU node(s) from database", sfu_nodes.len());
        }
    }

    let livekit_client = config.livekit.as_ref().map(|lk| {
        accordserver::voice::livekit::LiveKitClient::new(&lk.url, &lk.api_key, &lk.api_secret)
    });

    // Create storage directories
    let storage_path = config.storage_path.clone();
    for subdir in &["emojis", "sounds"] {
        let dir = storage_path.join(subdir);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::error!("failed to create storage directory {:?}: {:?}", dir, e);
        }
    }

    let gateway_tx_arc = Arc::new(RwLock::new(Some(gateway_tx)));

    // Create embedded SFU for custom voice backend
    let embedded_sfu = if config.voice_backend == VoiceBackend::Custom {
        let sfu = accordserver::voice::embedded_sfu::EmbeddedSfu::new(
            Arc::clone(&gateway_tx_arc),
        );
        tracing::info!("embedded WebRTC SFU initialized");
        Some(sfu)
    } else {
        None
    };

    let state = AppState {
        db,
        sfu_nodes: sfu_nodes.clone(),
        voice_states: Arc::new(DashMap::new()),
        dispatcher: Arc::new(RwLock::new(Some(dispatcher))),
        gateway_tx: gateway_tx_arc,
        test_mode: config.test_mode,
        voice_backend: config.voice_backend.clone(),
        livekit_client,
        embedded_sfu,
        rate_limits: Arc::new(DashMap::new()),
        storage_path,
    };

    // Spawn stale-node reaper only when using custom SFU backend
    if config.voice_backend == VoiceBackend::Custom {
        let reaper_db = state.db.clone();
        let reaper_nodes = sfu_nodes;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                match accordserver::db::sfu::mark_stale_nodes_offline(&reaper_db, 60).await {
                    Ok(stale_ids) => {
                        for id in &stale_ids {
                            reaper_nodes.remove(id);
                        }
                        if !stale_ids.is_empty() {
                            tracing::info!("reaped {} stale SFU node(s)", stale_ids.len());
                        }
                    }
                    Err(e) => {
                        tracing::error!("stale node reaper error: {:?}", e);
                    }
                }
            }
        });
    }

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

async fn run_sfu_node(config: Config) {
    let sfu_config = config
        .sfu
        .expect("SFU config is required when running in SFU mode");

    accordserver::sfu_runtime::run(sfu_config, config.port).await;
}
