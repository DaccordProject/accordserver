//! Shared inbound signature verification for the federation endpoints
//! (`inbox`, `join`). Performs the steps common to every signed S2S request:
//! parse the `Signature` header, resolve the peer, verify the signature, and
//! enforce the trust gate. Event-specific concerns (authority, dedup, apply)
//! stay with each handler.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::db::federation::Peer;
use crate::federation::signatures;
use crate::state::AppState;

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

/// Verify a signed inbound request and return the trusted signing peer, or an
/// error response to return verbatim.
pub async fn verify_signed(
    state: &AppState,
    our_domain: &str,
    headers: &HeaderMap,
    path: &str,
    body: &[u8],
) -> Result<Peer, Response> {
    let Some(sig_header) = headers.get("signature").and_then(|v| v.to_str().ok()) else {
        return Err(err(StatusCode::UNAUTHORIZED, "missing signature"));
    };
    let Some(parsed) = signatures::parse_signature_header(sig_header) else {
        return Err(err(StatusCode::UNAUTHORIZED, "malformed signature header"));
    };

    let peer = match crate::db::federation::get_peer(&state.db, &parsed.key_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return Err(err(StatusCode::FORBIDDEN, "unknown peer")),
        Err(e) => {
            tracing::error!("federation peer lookup failed: {e}");
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, "peer lookup failed"));
        }
    };

    let date = headers
        .get("date")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let digest = headers
        .get("digest")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Err(e) = signatures::verify_request(
        &peer.public_key,
        "POST",
        path,
        our_domain,
        date,
        digest,
        body,
        &parsed.signature_b64,
    ) {
        tracing::warn!(
            "federation signature rejected from {}: {:?}",
            peer.domain,
            e
        );
        return Err(err(
            StatusCode::UNAUTHORIZED,
            "signature verification failed",
        ));
    }

    // Replay rejection for the synchronous forward endpoints, which (unlike the
    // inbox) have no event-id dedup of their own: a verified signature may only
    // be used once within its skew window. The inbox is intentionally excluded —
    // it is idempotent via `(event_id, origin)` dedup, and at-least-once
    // redelivery legitimately re-presents the same event.
    if path != crate::federation::inbox::INBOX_PATH {
        if let Some(fed) = state.federation.as_ref() {
            if !fed.note_signature(&parsed.signature_b64) {
                tracing::warn!("federation replayed signature from {}", peer.domain);
                return Err(err(StatusCode::UNAUTHORIZED, "replayed request"));
            }
        }
    }

    // Trust gate (S4): only trusted peers may exchange content.
    if !peer.is_trusted() {
        return Err(err(StatusCode::FORBIDDEN, "peer not trusted"));
    }

    // Per-peer rate limit (S7): bound an authenticated peer's request rate so a
    // single peer cannot flood the inbox.
    if let Some(fed) = state.federation.as_ref() {
        if !fed.allow_request(&peer.domain) {
            return Err(err(StatusCode::TOO_MANY_REQUESTS, "rate limited"));
        }
    }

    Ok(peer)
}
