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

use crate::federation::err_response as err;
use crate::federation::{authority, mapping};
use crate::state::AppState;

/// Path of this endpoint, also used as the signed `(request-target)`.
pub const INBOX_PATH: &str = "/federation/v1/inbox";

/// Undo the dedup record for an event whose apply step did not complete, so a
/// redelivery is treated as new rather than a duplicate.
async fn rollback_dedup(state: &AppState, envelope: &mapping::FederationEnvelope) {
    if let Err(e) =
        crate::db::federation::dedup_remove(&state.db, &envelope.event_id, &envelope.origin).await
    {
        tracing::warn!("inbox dedup rollback failed for {}: {e}", envelope.event_id);
    }
}

pub async fn handle_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    // --- Verify signature + resolve trusted peer, parse envelope (shared) ---
    let (_our_domain, peer, envelope): (_, _, mapping::FederationEnvelope) =
        match crate::federation::verify::prepare(&state, &headers, INBOX_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };

    // --- Authority binding (S1) ---
    // (The trust gate (S4) is already enforced by verify_signed above.)
    if let Err(e) = authority::check(&peer.domain, &envelope) {
        tracing::warn!("inbox authority check failed from {}: {e}", peer.domain);
        return err(StatusCode::FORBIDDEN, "authority check failed");
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
            // Roll back dedup: we may add support later, and the peer should be
            // free to redeliver once we do.
            rollback_dedup(&state, &envelope).await;
            tracing::debug!(
                "inbox received unhandled event type `{}` from {}",
                envelope.event_type,
                peer.domain
            );
            err(StatusCode::NOT_IMPLEMENTED, "event type not yet supported")
        }
        Err(e) => {
            // The event passed dedup but failed to apply (possibly transiently).
            // Roll back the dedup record so the peer's retry can re-apply rather
            // than being acknowledged as a duplicate and dropped.
            rollback_dedup(&state, &envelope).await;
            tracing::warn!(
                "inbox failed to apply {} from {}: {e}",
                envelope.event_type,
                peer.domain
            );
            e.into_response()
        }
    }
}
