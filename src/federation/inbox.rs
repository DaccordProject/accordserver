//! Inbound federation endpoint: `POST /federation/v1/inbox`.
//!
//! Pipeline (S1/S3/S4 in the plan): verify signature → authority check →
//! trust gate → dedup → apply. Content application (messages, membership,
//! reactions) is added in later phases; Phase 0 handles `m.ping` and rejects
//! unknown event types as unprocessable so the security envelope can be
//! validated end-to-end first.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::federation::{authority, mapping, signatures};
use crate::state::AppState;

/// Path of this endpoint, also used as the signed `(request-target)`.
pub const INBOX_PATH: &str = "/federation/v1/inbox";

fn err(status: StatusCode, msg: &str) -> axum::response::Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

pub async fn handle_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let Some(fed) = state.federation.clone() else {
        return err(StatusCode::NOT_FOUND, "federation disabled");
    };

    // --- Signature header ---
    let Some(sig_header) = headers.get("signature").and_then(|v| v.to_str().ok()) else {
        return err(StatusCode::UNAUTHORIZED, "missing signature");
    };
    let Some(parsed) = signatures::parse_signature_header(sig_header) else {
        return err(StatusCode::UNAUTHORIZED, "malformed signature header");
    };

    // --- Known peer? (key needed to verify) ---
    let peer = match crate::db::federation::get_peer(&state.db, &parsed.key_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return err(StatusCode::FORBIDDEN, "unknown peer"),
        Err(e) => {
            tracing::error!("inbox peer lookup failed: {e}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "peer lookup failed");
        }
    };

    // --- Verify signature over method/path/host/date/digest ---
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
        INBOX_PATH,
        &fed.domain,
        date,
        digest,
        &body,
        &parsed.signature_b64,
    ) {
        tracing::warn!("inbox signature rejected from {}: {:?}", peer.domain, e);
        return err(StatusCode::UNAUTHORIZED, "signature verification failed");
    }

    // --- Parse envelope ---
    let envelope: mapping::FederationEnvelope = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(_) => return err(StatusCode::BAD_REQUEST, "invalid envelope"),
    };

    // --- Authority binding (S1) ---
    if let Err(e) = authority::check(&peer.domain, &envelope) {
        tracing::warn!("inbox authority check failed from {}: {e}", peer.domain);
        return err(StatusCode::FORBIDDEN, "authority check failed");
    }

    // --- Trust gate (S4): only trusted peers may exchange content ---
    if !peer.is_trusted() {
        return err(StatusCode::FORBIDDEN, "peer not trusted");
    }

    // --- Dedup (S3): idempotent at-least-once delivery ---
    match crate::db::federation::dedup_first_seen(&state.db, &envelope.event_id, &envelope.origin)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            // Already applied; acknowledge without re-applying.
            return StatusCode::OK.into_response();
        }
        Err(e) => {
            tracing::error!("inbox dedup failed: {e}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "dedup failed");
        }
    }

    // --- Apply ---
    if envelope.event_type == "m.ping" {
        return StatusCode::OK.into_response();
    }
    match crate::federation::apply::apply_event(&state, &peer.domain, &envelope).await {
        Ok(crate::federation::apply::Applied::Ok) => StatusCode::OK.into_response(),
        Ok(crate::federation::apply::Applied::Unsupported) => {
            tracing::debug!(
                "inbox received unhandled event type `{}` from {}",
                envelope.event_type,
                peer.domain
            );
            err(StatusCode::NOT_IMPLEMENTED, "event type not yet supported")
        }
        Err(e) => {
            tracing::warn!(
                "inbox failed to apply {} from {}: {e}",
                envelope.event_type,
                peer.domain
            );
            e.into_response()
        }
    }
}
