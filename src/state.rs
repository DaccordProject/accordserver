use dashmap::DashMap;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::time::Instant;

use crate::gateway::dispatcher::Dispatcher;
use crate::gateway::events::GatewayBroadcast;
use crate::models::voice::VoiceState;
use crate::voice::livekit::LiveKitClient;

/// Per-key token bucket for rate limiting.
#[derive(Clone)]
pub struct RateLimitBucket {
    pub remaining: u32,
    pub last_refill: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub voice_states: Arc<DashMap<String, VoiceState>>,
    pub dispatcher: Arc<RwLock<Option<Dispatcher>>>,
    pub gateway_tx: Arc<RwLock<Option<broadcast::Sender<GatewayBroadcast>>>>,
    pub test_mode: bool,
    pub livekit_client: LiveKitClient,
    pub rate_limits: Arc<DashMap<String, RateLimitBucket>>,
    pub storage_path: PathBuf,
}
