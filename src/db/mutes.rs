use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::mute::ChannelMute;

pub async fn get_mute(
    pool: &SqlitePool,
    user_id: &str,
    channel_id: &str,
) -> Result<Option<ChannelMute>, AppError> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT user_id, channel_id, created_at FROM channel_mutes WHERE user_id = ? AND channel_id = ?",
    )
    .bind(user_id)
    .bind(channel_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(user_id, channel_id, created_at)| ChannelMute {
        user_id,
        channel_id,
        created_at,
    }))
}

pub async fn create_mute(
    pool: &SqlitePool,
    user_id: &str,
    channel_id: &str,
) -> Result<ChannelMute, AppError> {
    sqlx::query(
        "INSERT OR IGNORE INTO channel_mutes (user_id, channel_id) VALUES (?, ?)",
    )
    .bind(user_id)
    .bind(channel_id)
    .execute(pool)
    .await?;

    get_mute(pool, user_id, channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound("mute not found".into()))
}

pub async fn delete_mute(
    pool: &SqlitePool,
    user_id: &str,
    channel_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM channel_mutes WHERE user_id = ? AND channel_id = ?")
        .bind(user_id)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_mutes_for_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<ChannelMute>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT user_id, channel_id, created_at FROM channel_mutes WHERE user_id = ? ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(user_id, channel_id, created_at)| ChannelMute {
            user_id,
            channel_id,
            created_at,
        })
        .collect())
}

/// Returns all muted channel IDs for a user, including channels that inherit
/// a mute from their parent category.
pub async fn list_effective_muted_channel_ids(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    // Get directly muted channel IDs
    let direct: Vec<(String,)> = sqlx::query_as(
        "SELECT channel_id FROM channel_mutes WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let direct_ids: std::collections::HashSet<String> =
        direct.into_iter().map(|(id,)| id).collect();

    // Find channels whose parent category is muted (but which don't have
    // their own explicit mute entry — those are already included).
    let inherited: Vec<(String,)> = sqlx::query_as(
        "SELECT c.id FROM channels c \
         INNER JOIN channel_mutes cm ON cm.channel_id = c.parent_id AND cm.user_id = ? \
         WHERE c.parent_id IS NOT NULL",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let mut all_ids = direct_ids;
    for (id,) in inherited {
        all_ids.insert(id);
    }

    Ok(all_ids.into_iter().collect())
}
