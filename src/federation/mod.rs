//! Peer-to-peer content federation.
//!
//! Servers trust each other directly via a configured peer list (no central
//! directory). Each space is authoritative on its home server; other servers
//! mirror the parts their users participate in, keyed by qualified IDs
//! (`<snowflake>@<domain>`). See `mapping`, `authority`, and the plan for the
//! full model.

pub mod apply;
pub mod authority;
pub mod dm;
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

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use dashmap::DashMap;
use tokio::time::Instant;

use crate::config::FederationConfig;
use crate::state::{AppState, RateLimitBucket};

/// Shared JSON error response (`{ "error": msg }`) for the signed S2S endpoints.
pub fn err_response(status: StatusCode, msg: &str) -> Response {
    (status, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// Re-broadcast a federated event to local gateway sessions for `space_id`
/// (or, when `None`, to all sessions filtered by `intent`). Delivery-only: it
/// never enqueues outbound fanout (S7).
pub(crate) async fn broadcast_space(
    state: &AppState,
    space_id: Option<String>,
    event_type: &str,
    data: serde_json::Value,
    intent: &str,
) {
    if let Some(dispatcher) = state.gateway_tx.read().await.as_ref() {
        let event = serde_json::json!({ "op": 0, "type": event_type, "data": data });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id,
            target_user_ids: None,
            event,
            intent: intent.to_string(),
        });
    }
}

/// Mirror a referenced remote user's cached profile.
///
/// A server is authoritative for a user's profile only when it *is* that user's
/// home. The user's home is taken from its own qualified ID (falling back to
/// `authoritative_domain` for an unqualified id). When the user is homed on
/// `authoritative_domain` we refresh the profile (`upsert`); otherwise we only
/// ensure the row exists (`ensure`), so a peer can never spoof or clobber
/// another server's user (S2).
pub(crate) async fn mirror_user(
    state: &AppState,
    authoritative_domain: &str,
    id: &str,
    handle: &str,
    display_name: Option<&str>,
    avatar: Option<&str>,
) -> Result<(), crate::error::AppError> {
    let domain = mapping::domain_of(id).unwrap_or(authoritative_domain);

    // A peer may legitimately *echo* an action taken by one of our own users
    // (e.g. our user reacted in a remote-homed space), referencing them by a
    // qualified `<snowflake>@<our_domain>` id. Such a reference must resolve to
    // a real local account, and the mirror row must adopt that account's real
    // profile — never the peer-supplied handle/display/avatar, which would let a
    // peer publish a spoofed profile under our user's identity (S2/S3).
    if let Some(our_domain) = state.federation.as_ref().map(|f| f.domain.as_str()) {
        if domain.eq_ignore_ascii_case(our_domain) {
            let local_id = mapping::local_part(id);
            let local = crate::db::users::get_user(&state.db, local_id)
                .await
                .map_err(|_| {
                    crate::error::AppError::Forbidden(
                        "referenced local user does not exist".to_string(),
                    )
                })?;
            // Reject references to non-local rows that merely share our domain
            // suffix (remote-origin rows are never authoritative-as-local).
            if local.origin.is_some() {
                return Err(crate::error::AppError::Forbidden(
                    "referenced id is not a local user".to_string(),
                ));
            }
            let real_handle = mapping::handle(&local.username, our_domain);
            crate::db::users::ensure_remote_user(
                &state.db,
                id,
                our_domain,
                &real_handle,
                local.display_name.as_deref(),
                local.avatar.as_deref(),
            )
            .await?;
            return Ok(());
        }
    }

    if domain.eq_ignore_ascii_case(authoritative_domain) {
        crate::db::users::upsert_remote_user(&state.db, id, domain, handle, display_name, avatar)
            .await?;
    } else {
        crate::db::users::ensure_remote_user(&state.db, id, domain, handle, display_name, avatar)
            .await?;
    }
    Ok(())
}

/// Per-peer inbound request budget (token bucket): requests per window.
const PEER_CAPACITY: u32 = 300;
/// Window over which a peer's budget fully refills.
const PEER_WINDOW_SECS: u64 = 60;
/// How long a seen signature is remembered for replay rejection. Must cover the
/// signature `Date` skew window (after which the date check rejects replays on
/// its own), so 5 minutes mirrors `signatures::MAX_SKEW_SECS`.
const SIGNATURE_REPLAY_WINDOW_SECS: u64 = 300;

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
    /// Recently-seen request signatures, for replay rejection across all signed
    /// S2S endpoints (the inbox additionally dedups by event id). Keyed by the
    /// base64 signature; values are when it was first seen.
    pub seen_signatures: Arc<DashMap<String, Instant>>,
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
            client: build_client(),
            rate_limits: Arc::new(DashMap::new()),
            seen_signatures: Arc::new(DashMap::new()),
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

    /// Record a request signature and report whether it is fresh. Returns
    /// `false` if the same signature was already seen within the replay window
    /// (i.e. this is a replay and the request should be rejected). The signature
    /// covers the `Date` header, so an attacker cannot refresh the window by
    /// re-dating without re-signing.
    pub fn note_signature(&self, signature_b64: &str) -> bool {
        let now = Instant::now();
        let window = std::time::Duration::from_secs(SIGNATURE_REPLAY_WINDOW_SECS);
        if let Some(seen) = self.seen_signatures.get(signature_b64) {
            if now.duration_since(*seen) < window {
                return false;
            }
        }
        self.seen_signatures.insert(signature_b64.to_string(), now);
        true
    }

    /// Drop replay-cache entries older than the window. Called periodically by
    /// the sender loop so the map cannot grow without bound.
    pub fn prune_signatures(&self) {
        let now = Instant::now();
        let window = std::time::Duration::from_secs(SIGNATURE_REPLAY_WINDOW_SECS);
        self.seen_signatures
            .retain(|_, seen| now.duration_since(*seen) < window);
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

/// Build the shared HTTP client for all outbound federation requests.
///
/// Redirects are disabled (S5): `validate_peer_url_resolved` checks the request
/// URL before connecting, but a peer could otherwise 3xx-redirect us to an
/// internal address that was never validated. A bounded total timeout caps how
/// long a slow/hostile peer can tie up a delivery worker.
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .dns_resolver(std::sync::Arc::new(peers::SsrfGuardResolver))
        .build()
        .unwrap_or_default()
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
            client: build_client(),
            rate_limits: Arc::new(DashMap::new()),
            seen_signatures: Arc::new(DashMap::new()),
        }
    }

    #[test]
    fn signature_replay_is_rejected_once_seen() {
        let ctx = test_context();
        // First sighting is fresh; an immediate repeat is a replay.
        assert!(ctx.note_signature("sig-abc"));
        assert!(!ctx.note_signature("sig-abc"));
        // A different signature is independent.
        assert!(ctx.note_signature("sig-xyz"));
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
