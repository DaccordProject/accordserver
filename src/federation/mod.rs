//! Peer-to-peer content federation.
//!
//! Servers trust each other directly via a configured peer list (no central
//! directory). Each space is authoritative on its home server; other servers
//! mirror the parts their users participate in, keyed by qualified IDs
//! (`<snowflake>@<domain>`). See `mapping`, `authority`, and the plan for the
//! full model.

pub mod apply;
pub mod authority;
pub mod forward;
pub mod handshake;
pub mod identity;
pub mod inbox;
pub mod mapping;
pub mod outbound;
pub mod peers;
pub mod sender;
pub mod signatures;
pub mod verify;
pub mod wellknown;

use std::path::Path;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::time::Instant;

use crate::config::FederationConfig;
use crate::state::RateLimitBucket;

/// Per-peer inbound request budget (token bucket): requests per window.
const PEER_CAPACITY: u32 = 300;
/// Window over which a peer's budget fully refills.
const PEER_WINDOW_SECS: u64 = 60;

/// Shared, process-wide federation state held in [`crate::state::AppState`].
#[derive(Clone)]
pub struct FederationContext {
    /// This server's domain, used to qualify local IDs and as the signing key id.
    pub domain: String,
    /// Public base URL where our endpoints are reachable (e.g. `https://a.example`).
    pub public_url: String,
    /// This server's Ed25519 signing identity.
    pub identity: identity::ServerIdentity,
    /// Shared HTTP client for outbound federation requests.
    pub client: reqwest::Client,
    /// Per-peer inbound rate-limit buckets (S7), keyed by peer domain.
    pub rate_limits: Arc<DashMap<String, RateLimitBucket>>,
}

impl FederationContext {
    /// Build the context, loading or generating the signing key beside the
    /// other persisted server state (next to `master_server_id`).
    pub fn build(config: &FederationConfig, storage_path: &Path) -> std::io::Result<Self> {
        // data/cdn -> data/federation_key (mirrors resolve_master_server_id).
        let key_path = storage_path
            .parent()
            .unwrap_or(storage_path)
            .join("federation_key");
        let identity = identity::ServerIdentity::load_or_create(&key_path)?;
        Ok(Self {
            domain: config.domain.clone(),
            public_url: config.public_url.clone(),
            identity,
            client: reqwest::Client::new(),
            rate_limits: Arc::new(DashMap::new()),
        })
    }

    /// Token-bucket admission for an authenticated peer. Returns `false` when the
    /// peer has exhausted its inbound budget (S7).
    pub fn allow_request(&self, domain: &str) -> bool {
        let now = Instant::now();
        let mut entry = self
            .rate_limits
            .entry(domain.to_string())
            .or_insert_with(|| RateLimitBucket {
                remaining: PEER_CAPACITY,
                last_refill: now,
            });
        let b = entry.value_mut();
        let elapsed = now.duration_since(b.last_refill).as_secs();
        if elapsed >= PEER_WINDOW_SECS {
            b.remaining = PEER_CAPACITY;
            b.last_refill = now;
        } else if elapsed > 0 {
            let refill = ((elapsed as f64 / PEER_WINDOW_SECS as f64) * PEER_CAPACITY as f64) as u32;
            b.remaining = (b.remaining + refill).min(PEER_CAPACITY);
            b.last_refill = now;
        }
        if b.remaining == 0 {
            false
        } else {
            b.remaining -= 1;
            true
        }
    }

    /// The fully-qualified inbox URL advertised to peers.
    pub fn inbox_url(&self) -> String {
        format!(
            "{}{}",
            self.public_url.trim_end_matches('/'),
            inbox::INBOX_PATH
        )
    }
}

/// Background task entry point: drains the outbound delivery queue.
pub async fn run(state: crate::state::AppState) {
    sender::run(state).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> FederationContext {
        let dir =
            std::env::temp_dir().join(format!("accord-fedctx-{}", crate::snowflake::generate()));
        FederationContext {
            domain: "a.test".to_string(),
            public_url: "https://a.test".to_string(),
            identity: identity::ServerIdentity::load_or_create(&dir.join("k")).unwrap(),
            client: reqwest::Client::new(),
            rate_limits: Arc::new(DashMap::new()),
        }
    }

    #[test]
    fn per_peer_rate_limit_exhausts_then_blocks() {
        let ctx = test_context();
        for _ in 0..PEER_CAPACITY {
            assert!(ctx.allow_request("b.test"));
        }
        assert!(!ctx.allow_request("b.test"));
        // A different peer has its own independent budget.
        assert!(ctx.allow_request("c.test"));
    }
}
