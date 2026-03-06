use arc_swap::ArcSwap;
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::Instant;

use crate::config::MasterServerConfig;
use crate::gateway::dispatcher::Dispatcher;
use crate::gateway::events::GatewayBroadcast;
use crate::models::presence::Presence;
use crate::models::settings::ServerSettings;
use crate::models::voice::VoiceState;
use crate::voice::livekit::LiveKitClient;

/// Per-key token bucket for rate limiting.
#[derive(Clone)]
pub struct RateLimitBucket {
    pub remaining: u32,
    pub last_refill: Instant,
}

/// Tracks TOTP verification attempts for brute-force protection.
#[derive(Clone)]
pub struct TotpAttemptTracker {
    pub failures: u32,
    pub window_start: Instant,
}

/// Short-lived MFA ticket issued after password verification when 2FA is required.
#[derive(Clone)]
pub struct MfaTicket {
    pub user_id: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub voice_states: Arc<DashMap<String, VoiceState>>,
    pub presences: Arc<DashMap<String, Presence>>,
    pub dispatcher: Arc<RwLock<Option<Dispatcher>>>,
    pub gateway_tx: Arc<RwLock<Option<broadcast::Sender<GatewayBroadcast>>>>,
    pub test_mode: bool,
    pub livekit_client: Option<LiveKitClient>,
    pub rate_limits: Arc<DashMap<String, RateLimitBucket>>,
    pub storage_path: PathBuf,
    pub settings: Arc<ArcSwap<ServerSettings>>,
    pub master_config: Option<MasterServerConfig>,
    pub master_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// ticket_hash -> MfaTicket; short-lived tickets for 2FA login flow
    pub mfa_tickets: Arc<DashMap<String, MfaTicket>>,
    /// user_id -> TotpAttemptTracker; brute-force protection for TOTP verification
    pub totp_attempts: Arc<DashMap<String, TotpAttemptTracker>>,
    /// Optional AES-256-GCM key for encrypting TOTP secrets at rest
    pub totp_key: Option<[u8; 32]>,
    /// Optional API key for MCP endpoint authentication
    pub mcp_api_key: Option<String>,
}
