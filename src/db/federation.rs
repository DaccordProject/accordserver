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

// ---------------------------------------------------------------------------
// Replica upserts: mirror remote spaces/channels/roles/members into the local
// tables using qualified IDs and `origin = <home domain>`. All are idempotent.
// ---------------------------------------------------------------------------

/// Mirror a remote space. `owner_id` (a qualified remote user ID) must already
/// be upserted via [`crate::db::users::upsert_remote_user`] to satisfy the FK.
pub async fn upsert_remote_space(
    pool: &AnyPool,
    id: &str,
    origin: &str,
    name: &str,
    slug: &str,
    owner_id: &str,
) -> Result<(), AppError> {
    sqlx::query(&crate::db::q(
        "INSERT INTO spaces (id, name, slug, description, owner_id, public, allow_guest_access, origin, federation_enabled) \
         VALUES (?, ?, ?, '', ?, FALSE, FALSE, ?, 1) \
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, owner_id = excluded.owner_id, origin = excluded.origin",
    ))
    .bind(id)
    .bind(name)
    .bind(slug)
    .bind(owner_id)
    .bind(origin)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mirror a remote channel.
pub async fn upsert_remote_channel(
    pool: &AnyPool,
    id: &str,
    origin: &str,
    space_id: &str,
    name: &str,
    channel_type: &str,
    position: i64,
) -> Result<(), AppError> {
    sqlx::query(&crate::db::q(
        "INSERT INTO channels (id, name, type, space_id, position, origin) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, position = excluded.position, origin = excluded.origin",
    ))
    .bind(id)
    .bind(name)
    .bind(channel_type)
    .bind(space_id)
    .bind(position)
    .bind(origin)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mirror a remote role.
pub async fn upsert_remote_role(
    pool: &AnyPool,
    id: &str,
    origin: &str,
    space_id: &str,
    name: &str,
    position: i64,
    permissions_json: &str,
) -> Result<(), AppError> {
    sqlx::query(&crate::db::q(
        "INSERT INTO roles (id, space_id, name, position, permissions, origin) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, position = excluded.position, permissions = excluded.permissions, origin = excluded.origin",
    ))
    .bind(id)
    .bind(space_id)
    .bind(name)
    .bind(position)
    .bind(permissions_json)
    .bind(origin)
    .execute(pool)
    .await?;
    Ok(())
}

/// Add a member to a (possibly remote-homed) space, recording the member's
/// `origin`. `origin = NULL` for a local user joining a remote space; the home
/// domain for a remote user mirrored into a local space.
pub async fn add_member_with_origin(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    origin: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(&crate::db::q(
        "INSERT INTO members (user_id, space_id, origin) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
    ))
    .bind(user_id)
    .bind(space_id)
    .bind(origin)
    .execute(pool)
    .await?;
    Ok(())
}

/// The set of peer domains "interested" in a space: the distinct `origin`s of
/// its members. Used to fan out events for a locally-homed space to exactly the
/// servers that have a member there.
pub async fn interested_servers(pool: &AnyPool, space_id: &str) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query(&crate::db::q(
        "SELECT DISTINCT origin FROM members WHERE space_id = ? AND origin IS NOT NULL",
    ))
    .bind(space_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("origin").ok())
        .collect())
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
