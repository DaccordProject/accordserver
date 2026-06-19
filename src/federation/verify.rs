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

    // Trust gate (S4): only trusted peers may exchange content.
    if !peer.is_trusted() {
        return Err(err(StatusCode::FORBIDDEN, "peer not trusted"));
    }

    Ok(peer)
}
