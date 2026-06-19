//! Outbound federation delivery: a durable queue drained by a background task
//! that signs and POSTs each envelope to the target peer's inbox, with
//! exponential backoff and a dead-letter cap (S7 in the plan).

use crate::error::AppError;
use crate::federation::{mapping::FederationEnvelope, peers, signatures, FederationContext};
use crate::state::AppState;

/// How often the drain loop wakes to look for due deliveries.
const TICK: std::time::Duration = std::time::Duration::from_secs(5);
/// Max deliveries processed per tick.
const BATCH: i64 = 32;
/// Give up after this many attempts (dead-letter).
const MAX_ATTEMPTS: i64 = 12;
/// Backoff cap.
const MAX_BACKOFF_SECS: i64 = 3600;

/// Enqueue an envelope for delivery to each distinct target domain.
///
/// Loop prevention (S7): callers enqueue **only** for locally-originated
/// authoritative actions — never when applying an inbound remote event.
pub async fn enqueue(
    state: &AppState,
    envelope: &FederationEnvelope,
    targets: &[String],
) -> Result<(), AppError> {
    let Some(fed) = state.federation.as_ref() else {
        return Ok(());
    };
    let payload = serde_json::to_string(envelope)
        .map_err(|e| AppError::Internal(format!("serialize envelope: {e}")))?;

    let mut seen = std::collections::HashSet::new();
    for target in targets {
        // Never deliver to ourselves.
        if target.eq_ignore_ascii_case(&fed.domain) || !seen.insert(target.to_ascii_lowercase()) {
            continue;
        }
        let id = crate::snowflake::generate();
        crate::db::federation::outbox_enqueue(&state.db, &id, target, &payload).await?;
    }
    Ok(())
}

/// Background drain loop. Runs for the lifetime of the process.
pub async fn run(state: AppState) {
    let Some(fed) = state.federation.clone() else {
        return;
    };
    tracing::info!("federation sender started for domain `{}`", fed.domain);

    // Retention for the inbound dedup table, and how often to prune it.
    const DEDUP_RETENTION_SECS: i64 = 24 * 3600;
    let prune_every = (3600 / TICK.as_secs().max(1)).max(1); // ~hourly
    let mut tick_count: u64 = 0;

    loop {
        tokio::time::sleep(TICK).await;
        let _ = deliver_due_once(&state).await;

        tick_count = tick_count.wrapping_add(1);
        if tick_count.is_multiple_of(prune_every) {
            match crate::db::federation::cleanup_dedup(&state.db, DEDUP_RETENTION_SECS).await {
                Ok(n) if n > 0 => tracing::debug!("pruned {n} federation dedup rows"),
                Ok(_) => {}
                Err(e) => tracing::warn!("federation dedup cleanup failed: {e}"),
            }
        }
    }
}

/// Process one batch of due outbound deliveries, returning how many succeeded.
/// Used by the background loop and directly in tests.
pub async fn deliver_due_once(state: &AppState) -> usize {
    let Some(fed) = state.federation.clone() else {
        return 0;
    };
    let due = match crate::db::federation::outbox_claim_due(&state.db, BATCH).await {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!("federation outbox query failed: {e}");
            return 0;
        }
    };

    let mut delivered = 0;
    for item in due {
        match deliver(state, &fed, &item.target_domain, &item.payload).await {
            Ok(()) => {
                let _ = crate::db::federation::outbox_delete(&state.db, &item.id).await;
                delivered += 1;
            }
            Err(e) => {
                let attempts = item.attempts + 1;
                if attempts >= MAX_ATTEMPTS {
                    tracing::warn!(
                        "federation delivery to {} dead-lettered after {attempts} attempts: {e}",
                        item.target_domain
                    );
                    let _ = crate::db::federation::outbox_delete(&state.db, &item.id).await;
                } else {
                    let backoff = backoff_secs(attempts);
                    tracing::debug!(
                        "federation delivery to {} failed (attempt {attempts}), retrying in {backoff}s: {e}",
                        item.target_domain
                    );
                    let _ = crate::db::federation::outbox_reschedule(
                        &state.db, &item.id, attempts, backoff,
                    )
                    .await;
                }
            }
        }
    }
    delivered
}

fn backoff_secs(attempts: i64) -> i64 {
    // 2s, 4s, 8s, ... capped.
    2i64.saturating_pow(attempts.min(20) as u32)
        .min(MAX_BACKOFF_SECS)
}

/// Sign and POST a single payload to a peer's inbox.
async fn deliver(
    state: &AppState,
    fed: &FederationContext,
    target_domain: &str,
    payload: &str,
) -> Result<(), AppError> {
    let peer = crate::db::federation::get_peer(&state.db, target_domain)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("peer {target_domain} not found")))?;

    peers::validate_peer_url(&peer.inbox_url)?;

    let path = inbox_path(&peer.inbox_url);
    let signed = signatures::sign_request(
        &fed.identity,
        &fed.domain,
        "POST",
        &path,
        target_domain,
        payload.as_bytes(),
    );

    let resp = fed
        .client
        .post(&peer.inbox_url)
        .header("Date", &signed.date)
        .header("Digest", &signed.digest)
        .header("Signature", &signed.signature)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("post to {target_domain}: {e}")))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        Err(AppError::Internal(format!(
            "peer {target_domain} returned {}",
            resp.status()
        )))
    }
}

/// Synchronously sign and POST `body` to `path` on a trusted peer, returning
/// the response status and body. Used by the request-path forwards (initiate
/// join, post to a remote-homed space) where the caller needs the reply.
pub async fn request_signed(
    state: &AppState,
    target_domain: &str,
    path: &str,
    body: &[u8],
) -> Result<(reqwest::StatusCode, Vec<u8>), AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;
    let peer = crate::db::federation::get_peer(&state.db, target_domain)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("unknown peer {target_domain}")))?;
    if !peer.is_trusted() {
        return Err(AppError::Forbidden(format!(
            "peer {target_domain} not trusted"
        )));
    }

    // The peer's endpoints are siblings of its inbox URL.
    let base = peer
        .inbox_url
        .strip_suffix(super::inbox::INBOX_PATH)
        .unwrap_or(&peer.inbox_url);
    let url = format!("{base}{path}");
    peers::validate_peer_url(&url)?;

    let signed = signatures::sign_request(
        &fed.identity,
        &fed.domain,
        "POST",
        path,
        target_domain,
        body,
    );
    let resp = fed
        .client
        .post(&url)
        .header("Date", &signed.date)
        .header("Digest", &signed.digest)
        .header("Signature", &signed.signature)
        .header("Content-Type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("post to {target_domain}: {e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("read response from {target_domain}: {e}")))?;
    Ok((status, bytes.to_vec()))
}

/// Extract the path portion of an inbox URL for signing (must match what the
/// receiver verifies against).
fn inbox_path(inbox_url: &str) -> String {
    reqwest::Url::parse(inbox_url)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| super::inbox::INBOX_PATH.to_string())
}
