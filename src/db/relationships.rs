use sqlx::AnyPool;

use crate::error::AppError;

/// A relationship row joined with the target user's basic info.
#[derive(Debug, Clone)]
pub struct RelationshipRow {
    pub user_id: String,
    pub target_user_id: String,
    /// 1=friend, 2=blocked, 3=pending_incoming, 4=pending_outgoing
    pub rel_type: i64,
    pub created_at: String,
    // Joined from users
    pub target_username: String,
    pub target_display_name: Option<String>,
    pub target_avatar: Option<String>,
}

const SELECT_RELS: &str = "
    SELECT r.user_id, r.target_user_id, r.type, r.created_at,
           u.username, u.display_name, u.avatar
    FROM relationships r
    JOIN users u ON u.id = r.target_user_id";

fn row_to_rel(
    row: (
        String,
        String,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
    ),
) -> RelationshipRow {
    RelationshipRow {
        user_id: row.0,
        target_user_id: row.1,
        rel_type: row.2,
        created_at: row.3,
        target_username: row.4,
        target_display_name: row.5,
        target_avatar: row.6,
    }
}

pub async fn list_relationships(
    pool: &AnyPool,
    user_id: &str,
) -> Result<Vec<RelationshipRow>, AppError> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
        ),
    >(&super::q(&format!(
        "{SELECT_RELS} WHERE r.user_id = ? ORDER BY r.created_at DESC"
    )))
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_rel).collect())
}

pub async fn get_relationship(
    pool: &AnyPool,
    user_id: &str,
    target_id: &str,
) -> Result<Option<RelationshipRow>, AppError> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
        ),
    >(&super::q(&format!(
        "{SELECT_RELS} WHERE r.user_id = ? AND r.target_user_id = ?"
    )))
    .bind(user_id)
    .bind(target_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_rel))
}

/// Insert or update a directed relationship row.
pub async fn upsert_relationship(
    pool: &AnyPool,
    user_id: &str,
    target_id: &str,
    rel_type: i64,
) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "INSERT INTO relationships (user_id, target_user_id, type)
         VALUES (?, ?, ?)
         ON CONFLICT (user_id, target_user_id)
         DO UPDATE SET type = excluded.type, created_at = created_at",
    ))
    .bind(user_id)
    .bind(target_id)
    .bind(rel_type)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a single directed relationship row. Returns true if a row was deleted.
pub async fn delete_relationship(
    pool: &AnyPool,
    user_id: &str,
    target_id: &str,
) -> Result<bool, AppError> {
    let result = sqlx::query(&super::q(
        "DELETE FROM relationships WHERE user_id = ? AND target_user_id = ?",
    ))
    .bind(user_id)
    .bind(target_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete both directed relationship rows (mutual remove for unfriend/decline).
pub async fn delete_both_directions(
    pool: &AnyPool,
    user_a: &str,
    user_b: &str,
) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "DELETE FROM relationships
         WHERE (user_id = ? AND target_user_id = ?)
            OR (user_id = ? AND target_user_id = ?)",
    ))
    .bind(user_a)
    .bind(user_b)
    .bind(user_b)
    .bind(user_a)
    .execute(pool)
    .await?;
    Ok(())
}

/// Return all friend user IDs for a user (type = 1).
pub async fn get_friend_ids(pool: &AnyPool, user_id: &str) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query_as::<_, (String,)>(&super::q(
        "SELECT target_user_id FROM relationships WHERE user_id = ? AND type = 1",
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Check whether user_b has blocked user_a.
pub async fn is_blocked_by(
    pool: &AnyPool,
    blocker_id: &str,
    blocked_id: &str,
) -> Result<bool, AppError> {
    let row = sqlx::query_as::<_, (i64,)>(&super::q(
        "SELECT COUNT(*) FROM relationships WHERE user_id = ? AND target_user_id = ? AND type = 2",
    ))
    .bind(blocker_id)
    .bind(blocked_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0 > 0)
}
