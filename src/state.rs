use dashmap::DashMap;
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::time::Instant;

use crate::config::VoiceBackend;
use crate::gateway::dispatcher::Dispatcher;
use crate::gateway::events::GatewayBroadcast;
use crate::models::voice::VoiceState;
use crate::voice::livekit::LiveKitClient;

#[derive(Clone)]
pub struct SfuNode {
    pub id: String,
    pub endpoint: String,
    pub region: String,
    pub capacity: i64,
    pub current_load: i64,
    pub status: String,
}

/// Per-key token bucket for rate limiting.
#[derive(Clone)]
pub struct RateLimitBucket {
    pub remaining: u32,
    pub last_refill: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub sfu_nodes: Arc<DashMap<String, SfuNode>>,
    pub voice_states: Arc<DashMap<String, VoiceState>>,
    pub dispatcher: Arc<RwLock<Option<Dispatcher>>>,
    pub gateway_tx: Arc<RwLock<Option<broadcast::Sender<GatewayBroadcast>>>>,
    pub test_mode: bool,
    pub voice_backend: VoiceBackend,
    pub livekit_client: Option<LiveKitClient>,
    pub rate_limits: Arc<DashMap<String, RateLimitBucket>>,
}
