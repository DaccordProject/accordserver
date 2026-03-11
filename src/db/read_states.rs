use serde::Serialize;
use sqlx::AnyPool;

use crate::db::now_sql;
use crate::error::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct ReadState {
    pub channel_id: String,
    pub last_read_message_id: Option<String>,
    pub mention_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnreadChannel {
    pub channel_id: String,
    pub last_read_message_id: Option<String>,
    pub last_message_id: Option<String>,
    pub mention_count: i64,
}

/// Get all channels where the user has unread messages.
/// A channel is unread if its last_message_id is greater than the user's
/// last_read_message_id (or if there is no read state but the channel has messages).
pub async fn get_unread_channels(
    pool: &AnyPool,
    user_id: &str,
) -> Result<Vec<UnreadChannel>, AppError> {
    // Get channels the user is a member of (space channels + DMs) that have
    // messages newer than their last read position.
    let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, i64)>(
        "SELECT c.id, rs.last_read_message_id, c.last_message_id, COALESCE(rs.mention_count, 0)
         FROM channels c
         LEFT JOIN read_states rs ON rs.channel_id = c.id AND rs.user_id = ?
         WHERE c.last_message_id IS NOT NULL
           AND (
             -- Space channels: user must be a member of the space
             (c.space_id IS NOT NULL AND EXISTS (
               SELECT 1 FROM members m WHERE m.space_id = c.space_id AND m.user_id = ?
             ))
             OR
             -- DM channels: user must be a participant
             (c.space_id IS NULL AND EXISTS (
               SELECT 1 FROM dm_participants dp WHERE dp.channel_id = c.id AND dp.user_id = ?
             ))
           )
           AND (
             rs.last_read_message_id IS NULL
             OR c.last_message_id > rs.last_read_message_id
           )",
    )
    .bind(user_id)
    .bind(user_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(channel_id, last_read_message_id, last_message_id, mention_count)| UnreadChannel {
                channel_id,
                last_read_message_id,
                last_message_id,
                mention_count,
            },
        )
        .collect())
}

/// Mark a channel as read up to a given message ID.
pub async fn ack_channel(
    pool: &AnyPool,
    user_id: &str,
    channel_id: &str,
    message_id: &str,
    is_postgres: bool,
) -> Result<(), AppError> {
    let now = now_sql(is_postgres);
    let sql = format!(
        "INSERT INTO read_states (user_id, channel_id, last_read_message_id, mention_count, updated_at)
         VALUES (?, ?, ?, 0, {now})
         ON CONFLICT(user_id, channel_id) DO UPDATE SET
           last_read_message_id = CASE
             WHEN EXCLUDED.last_read_message_id > COALESCE(read_states.last_read_message_id, '') THEN EXCLUDED.last_read_message_id
             ELSE read_states.last_read_message_id
           END,
           mention_count = 0,
           updated_at = {now}"
    );
    sqlx::query(&sql)
        .bind(user_id)
        .bind(channel_id)
        .bind(message_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Increment mention count for a user in a channel.
pub async fn increment_mention_count(
    pool: &AnyPool,
    user_id: &str,
    channel_id: &str,
    is_postgres: bool,
) -> Result<(), AppError> {
    let now = now_sql(is_postgres);
    let sql = format!(
        "INSERT INTO read_states (user_id, channel_id, mention_count, updated_at)
         VALUES (?, ?, 1, {now})
         ON CONFLICT(user_id, channel_id) DO UPDATE SET
           mention_count = read_states.mention_count + 1,
           updated_at = {now}"
    );
    sqlx::query(&sql)
        .bind(user_id)
        .bind(channel_id)
        .execute(pool)
        .await?;

    Ok(())
}
