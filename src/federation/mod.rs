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

use crate::config::FederationConfig;

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
        })
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
