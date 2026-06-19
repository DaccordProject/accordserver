//! Database access for federation state: known peers, inbound dedup, and the
//! durable outbound delivery queue.

use sqlx::{AnyPool, Row};

use crate::error::AppError;

/// Timestamp format shared by all federation columns: UTC `YYYY-MM-DD HH:MM:SS`,
/// matching [`crate::db::now_sql`] so lexicographic comparison against the
/// database's `now()` expression is valid.
fn now_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn ts_after(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

#[derive(Debug, Clone)]
pub struct Peer {
    pub domain: String,
    pub public_key: String,
    pub inbox_url: String,
    pub trust_state: String,
}

impl Peer {
    pub fn is_trusted(&self) -> bool {
        self.trust_state == "trusted"
    }
}

fn row_to_peer(row: sqlx::any::AnyRow) -> Peer {
    Peer {
        domain: row.get("domain"),
        public_key: row.get("public_key"),
        inbox_url: row.get("inbox_url"),
        trust_state: row.get("trust_state"),
    }
}

pub async fn get_peer(pool: &AnyPool, domain: &str) -> Result<Option<Peer>, AppError> {
    let row = sqlx::query(&crate::db::q(
        "SELECT domain, public_key, inbox_url, trust_state FROM federation_peers WHERE domain = ?",
    ))
    .bind(domain)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(row_to_peer))
}

pub async fn list_peers(pool: &AnyPool) -> Result<Vec<Peer>, AppError> {
    let rows = sqlx::query(&crate::db::q(
        "SELECT domain, public_key, inbox_url, trust_state FROM federation_peers ORDER BY domain",
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(row_to_peer).collect())
}

/// Insert or update a peer's key/inbox. `trust_state` is only applied on insert
/// — an existing peer's trust is preserved (use [`set_peer_trust`] to change it),
/// so re-fetching a peer's `.well-known` can refresh its key without silently
/// re-granting trust.
pub async fn upsert_peer(
    pool: &AnyPool,
    domain: &str,
    public_key: &str,
    inbox_url: &str,
    trust_state: &str,
) -> Result<(), AppError> {
    let now = now_string();
    sqlx::query(&crate::db::q(
        "INSERT INTO federation_peers (domain, public_key, inbox_url, trust_state, created_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (domain) DO UPDATE SET public_key = excluded.public_key, inbox_url = excluded.inbox_url",
    ))
    .bind(domain)
    .bind(public_key)
    .bind(inbox_url)
    .bind(trust_state)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_peer_trust(
    pool: &AnyPool,
    domain: &str,
    trust_state: &str,
) -> Result<(), AppError> {
    sqlx::query(&crate::db::q(
        "UPDATE federation_peers SET trust_state = ? WHERE domain = ?",
    ))
    .bind(trust_state)
    .bind(domain)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_peer(pool: &AnyPool, domain: &str) -> Result<(), AppError> {
    sqlx::query(&crate::db::q(
        "DELETE FROM federation_peers WHERE domain = ?",
    ))
    .bind(domain)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record an inbound event for deduplication. Returns `true` if this is the
/// first time we have seen `(event_id, origin)` (i.e. the caller should apply
/// it), `false` if it is a duplicate that should be skipped.
pub async fn dedup_first_seen(
    pool: &AnyPool,
    event_id: &str,
    origin: &str,
) -> Result<bool, AppError> {
    let now = now_string();
    let res = sqlx::query(&crate::db::q(
        "INSERT INTO federation_inbox_dedup (event_id, origin, received_at) VALUES (?, ?, ?) \
         ON CONFLICT (event_id, origin) DO NOTHING",
    ))
    .bind(event_id)
    .bind(origin)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// A queued outbound delivery.
#[derive(Debug, Clone)]
pub struct OutboxItem {
    pub id: String,
    pub target_domain: String,
    pub payload: String,
    pub attempts: i64,
}

pub async fn outbox_enqueue(
    pool: &AnyPool,
    id: &str,
    target_domain: &str,
    payload: &str,
) -> Result<(), AppError> {
    let now = now_string();
    sqlx::query(&crate::db::q(
        "INSERT INTO federation_outbox (id, target_domain, payload, attempts, next_attempt_at, created_at) \
         VALUES (?, ?, ?, 0, ?, ?)",
    ))
    .bind(id)
    .bind(target_domain)
    .bind(payload)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch up to `limit` deliveries whose `next_attempt_at` is due.
pub async fn outbox_claim_due(pool: &AnyPool, limit: i64) -> Result<Vec<OutboxItem>, AppError> {
    let now = crate::db::now_sql(crate::db::is_pg());
    let sql = crate::db::q(&format!(
        "SELECT id, target_domain, payload, attempts FROM federation_outbox \
         WHERE next_attempt_at <= {now} ORDER BY next_attempt_at LIMIT ?"
    ));
    let rows = sqlx::query(&sql).bind(limit).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| OutboxItem {
            id: row.get("id"),
            target_domain: row.get("target_domain"),
            payload: row.get("payload"),
            attempts: row.get("attempts"),
        })
        .collect())
}

pub async fn outbox_delete(pool: &AnyPool, id: &str) -> Result<(), AppError> {
    sqlx::query(&crate::db::q("DELETE FROM federation_outbox WHERE id = ?"))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Bump the attempt count and schedule the next retry `delay_secs` from now.
pub async fn outbox_reschedule(
    pool: &AnyPool,
    id: &str,
    attempts: i64,
    delay_secs: i64,
) -> Result<(), AppError> {
    let next = ts_after(delay_secs);
    sqlx::query(&crate::db::q(
        "UPDATE federation_outbox SET attempts = ?, next_attempt_at = ? WHERE id = ?",
    ))
    .bind(attempts)
    .bind(&next)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
