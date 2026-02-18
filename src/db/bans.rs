use sqlx::SqlitePool;

use crate::error::AppError;

#[derive(Debug, Clone)]
pub struct BanRow {
    pub user_id: String,
    pub space_id: String,
    pub reason: Option<String>,
    pub banned_by: Option<String>,
    pub created_at: String,
}

pub async fn get_ban(pool: &SqlitePool, space_id: &str, user_id: &str) -> Result<BanRow, AppError> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String)>(
        "SELECT user_id, space_id, reason, banned_by, created_at FROM bans WHERE space_id = ? AND user_id = ?"
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

pub async fn list_bans(pool: &SqlitePool, space_id: &str) -> Result<Vec<BanRow>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String)>(
        "SELECT user_id, space_id, reason, banned_by, created_at FROM bans WHERE space_id = ? ORDER BY created_at DESC"
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
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    reason: Option<&str>,
    banned_by: &str,
) -> Result<BanRow, AppError> {
    // Remove member first
    sqlx::query("DELETE FROM members WHERE space_id = ? AND user_id = ?")
        .bind(space_id)
        .bind(user_id)
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO bans (user_id, space_id, reason, banned_by) VALUES (?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(space_id)
    .bind(reason)
    .bind(banned_by)
    .execute(pool)
    .await?;

    get_ban(pool, space_id, user_id).await
}

pub async fn delete_ban(pool: &SqlitePool, space_id: &str, user_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM bans WHERE space_id = ? AND user_id = ?")
        .bind(space_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}
