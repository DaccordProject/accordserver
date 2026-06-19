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

    loop {
        tokio::time::sleep(TICK).await;

        let due = match crate::db::federation::outbox_claim_due(&state.db, BATCH).await {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!("federation outbox query failed: {e}");
                continue;
            }
        };

        for item in due {
            match deliver(&state, &fed, &item.target_domain, &item.payload).await {
                Ok(()) => {
                    let _ = crate::db::federation::outbox_delete(&state.db, &item.id).await;
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
    }
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

/// Extract the path portion of an inbox URL for signing (must match what the
/// receiver verifies against).
fn inbox_path(inbox_url: &str) -> String {
    reqwest::Url::parse(inbox_url)
        .map(|u| u.path().to_string())
        .unwrap_or_else(|_| super::inbox::INBOX_PATH.to_string())
}
