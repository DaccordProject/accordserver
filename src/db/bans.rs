use sqlx::AnyPool;

use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct BanRow {
    pub user_id: String,
    pub space_id: String,
    pub reason: Option<String>,
    pub banned_by: Option<String>,
    pub created_at: String,
}

pub async fn get_ban(pool: &AnyPool, space_id: &str, user_id: &str) -> Result<BanRow, AppError> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String)>(
        &super::q("SELECT user_id, space_id, reason, banned_by, created_at FROM bans WHERE space_id = ? AND user_id = ?")
    )
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("ban not found".to_string()))?;

    Ok(BanRow {
        user_id: row.0,
        space_id: row.1,
        reason: row.2,
        banned_by: row.3,
        created_at: row.4,
    })
}

pub async fn list_bans(pool: &AnyPool, space_id: &str) -> Result<Vec<BanRow>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String)>(
        &super::q("SELECT user_id, space_id, reason, banned_by, created_at FROM bans WHERE space_id = ? ORDER BY created_at DESC")
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| BanRow {
            user_id: row.0,
            space_id: row.1,
            reason: row.2,
            banned_by: row.3,
            created_at: row.4,
        })
        .collect())
}

pub async fn create_ban(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    reason: Option<&str>,
    banned_by: &str,
    is_postgres: bool,
) -> Result<BanRow, AppError> {
    // Remove member first
    sqlx::query(&super::q(
        "DELETE FROM members WHERE space_id = ? AND user_id = ?",
    ))
    .bind(space_id)
    .bind(user_id)
    .execute(pool)
    .await?;

    let sql = if is_postgres {
        "INSERT INTO bans (user_id, space_id, reason, banned_by) VALUES (?, ?, ?, ?) ON CONFLICT (user_id, space_id) DO UPDATE SET reason = EXCLUDED.reason, banned_by = EXCLUDED.banned_by"
    } else {
        "INSERT OR REPLACE INTO bans (user_id, space_id, reason, banned_by) VALUES (?, ?, ?, ?)"
    };
    sqlx::query(&super::q(sql))
        .bind(user_id)
        .bind(space_id)
        .bind(reason)
        .bind(banned_by)
        .execute(pool)
        .await?;

    get_ban(pool, space_id, user_id).await
}

pub async fn delete_ban(pool: &AnyPool, space_id: &str, user_id: &str) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "DELETE FROM bans WHERE space_id = ? AND user_id = ?",
    ))
    .bind(space_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether a space ban is the only thing that connected `sender_id` to
/// `recipient_id`, so the banned user must not be able to reach them by DM.
///
/// True when all of these hold:
/// 1. the sender is banned from some space the recipient is still a member of;
/// 2. the two share no space membership at all;
/// 3. they are not friends (in either direction).
///
/// Check 1 is the cheap, usually-false one, so it runs first and short-circuits.
pub async fn ban_separates(
    pool: &AnyPool,
    sender_id: &str,
    recipient_id: &str,
) -> Result<bool, AppError> {
    // Select a TEXT column rather than a literal so the Any driver decodes it
    // identically on SQLite and PostgreSQL.
    let banned_from_recipient_space: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT b.space_id FROM bans b JOIN members m ON m.space_id = b.space_id \
         WHERE b.user_id = ? AND m.user_id = ? LIMIT 1",
    ))
    .bind(sender_id)
    .bind(recipient_id)
    .fetch_optional(pool)
    .await?;
    if banned_from_recipient_space.is_none() {
        return Ok(false);
    }

    let shared_space: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT a.space_id FROM members a JOIN members b ON b.space_id = a.space_id \
         WHERE a.user_id = ? AND b.user_id = ? LIMIT 1",
    ))
    .bind(sender_id)
    .bind(recipient_id)
    .fetch_optional(pool)
    .await?;
    if shared_space.is_some() {
        return Ok(false);
    }

    let friends: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT user_id FROM relationships WHERE type = 1 AND \
         ((user_id = ? AND target_user_id = ?) OR (user_id = ? AND target_user_id = ?)) \
         LIMIT 1",
    ))
    .bind(sender_id)
    .bind(recipient_id)
    .bind(recipient_id)
    .bind(sender_id)
    .fetch_optional(pool)
    .await?;
    Ok(friends.is_none())
}
