//! Per-account device fingerprints and instance-wide device bans.
//!
//! A fingerprint is an opaque, per-server value the client computes (a SHA-256
//! rendered as 64 lowercase hex characters). The server only stores and
//! compares it; see [`validate_fingerprint`].

use sqlx::{AnyPool, Row};

use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct DeviceBanRow {
    pub id: String,
    pub fingerprint: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub reason: Option<String>,
    pub banned_by: Option<String>,
    pub created_at: String,
}

impl DeviceBanRow {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "fingerprint": self.fingerprint,
            "user_id": self.user_id,
            "username": self.username,
            "reason": self.reason,
            "banned_by": self.banned_by,
            "created_at": self.created_at,
        })
    }
}

/// A well-formed fingerprint is exactly 64 lowercase hex characters.
pub fn is_valid_fingerprint(fp: &str) -> bool {
    fp.len() == 64 && fp.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Validate an optional fingerprint from a request body. Absent is allowed
/// (older clients and bots do not send one); malformed is a 400.
pub fn validate_fingerprint(fp: Option<&str>) -> Result<Option<&str>, AppError> {
    match fp {
        None => Ok(None),
        Some(fp) if is_valid_fingerprint(fp) => Ok(Some(fp)),
        Some(_) => Err(AppError::BadRequest("invalid device_fingerprint".into())),
    }
}

const SELECT_BANS: &str =
    "SELECT id, fingerprint, user_id, username, reason, banned_by, created_at FROM device_bans";

fn row_to_ban(row: &sqlx::any::AnyRow) -> DeviceBanRow {
    DeviceBanRow {
        id: row.get("id"),
        fingerprint: row.get("fingerprint"),
        user_id: row.get("user_id"),
        username: row.get("username"),
        reason: row.get("reason"),
        banned_by: row.get("banned_by"),
        created_at: row.get("created_at"),
    }
}

/// Record (or refresh `last_seen` for) a fingerprint used by an account.
pub async fn record_device(
    pool: &AnyPool,
    user_id: &str,
    fingerprint: &str,
    is_pg: bool,
) -> Result<(), AppError> {
    let now = super::now_sql(is_pg);
    sqlx::query(&super::q(&format!(
        "INSERT INTO user_devices (user_id, fingerprint, first_seen, last_seen) \
         VALUES (?, ?, {now}, {now}) \
         ON CONFLICT (user_id, fingerprint) DO UPDATE SET last_seen = excluded.last_seen"
    )))
    .bind(user_id)
    .bind(fingerprint)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn is_fingerprint_banned(pool: &AnyPool, fingerprint: &str) -> Result<bool, AppError> {
    let hit: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT id FROM device_bans WHERE fingerprint = ? LIMIT 1",
    ))
    .bind(fingerprint)
    .fetch_optional(pool)
    .await?;
    Ok(hit.is_some())
}

pub async fn list_user_fingerprints(
    pool: &AnyPool,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows: Vec<String> = sqlx::query_scalar(&super::q(
        "SELECT fingerprint FROM user_devices WHERE user_id = ? ORDER BY first_seen, fingerprint",
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Ban every fingerprint recorded for `user_id`. Fingerprints that are already
/// banned are skipped; only the newly created ban rows are returned.
pub async fn ban_user_devices(
    pool: &AnyPool,
    user_id: &str,
    username: Option<&str>,
    reason: Option<&str>,
    banned_by: &str,
    is_pg: bool,
) -> Result<Vec<DeviceBanRow>, AppError> {
    let now = super::now_sql(is_pg);
    let insert_sql = super::q(&format!(
        "INSERT INTO device_bans (id, fingerprint, user_id, username, reason, banned_by, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, {now}) ON CONFLICT (fingerprint) DO NOTHING"
    ));
    let select_sql = super::q(&format!("{SELECT_BANS} WHERE id = ?"));

    let mut created = Vec::new();
    for fingerprint in list_user_fingerprints(pool, user_id).await? {
        let id = crate::snowflake::generate();
        let inserted = sqlx::query(&insert_sql)
            .bind(&id)
            .bind(&fingerprint)
            .bind(user_id)
            .bind(username)
            .bind(reason)
            .bind(banned_by)
            .execute(pool)
            .await?
            .rows_affected();
        if inserted == 0 {
            continue;
        }
        let row = sqlx::query(&select_sql).bind(&id).fetch_one(pool).await?;
        created.push(row_to_ban(&row));
    }
    Ok(created)
}

/// Device bans, newest first. `after` is the id of the last row of the
/// previous page; rows older than it are returned.
pub async fn list_device_bans(
    pool: &AnyPool,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<DeviceBanRow>, AppError> {
    let where_clause = if after.is_some() { "WHERE id < ?" } else { "" };
    let sql = super::q(&format!(
        "{SELECT_BANS} {where_clause} ORDER BY id DESC LIMIT ?"
    ));
    let mut query = sqlx::query(&sql);
    if let Some(a) = after {
        query = query.bind(a);
    }
    let rows = query.bind(limit).fetch_all(pool).await?;
    Ok(rows.iter().map(row_to_ban).collect())
}

/// Returns true if a ban was removed.
pub async fn delete_device_ban(pool: &AnyPool, id: &str) -> Result<bool, AppError> {
    let result = sqlx::query(&super::q("DELETE FROM device_bans WHERE id = ?"))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
