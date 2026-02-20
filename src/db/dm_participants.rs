use sqlx::SqlitePool;

use crate::db;
use crate::error::AppError;
use crate::models::channel::ChannelRow;
use crate::models::user::User;
use crate::snowflake;

/// Check whether a user is a participant in a DM channel.
pub async fn is_participant(
    pool: &SqlitePool,
    channel_id: &str,
    user_id: &str,
) -> Result<bool, AppError> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM dm_participants WHERE channel_id = ? AND user_id = ?")
            .bind(channel_id)
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    Ok(count > 0)
}

/// List all participant user IDs for a DM channel.
pub async fn list_participant_ids(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows =
        sqlx::query_as::<_, (String,)>("SELECT user_id FROM dm_participants WHERE channel_id = ?")
            .bind(channel_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Get full User objects for all participants in a DM channel.
pub async fn get_participant_users(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<User>, AppError> {
    let ids = list_participant_ids(pool, channel_id).await?;
    let mut users = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(user) = db::users::get_user(pool, &id).await {
            users.push(user);
        }
    }
    Ok(users)
}

/// Add a participant to a DM channel. No-op if already present.
pub async fn add_participant(
    pool: &SqlitePool,
    channel_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT OR IGNORE INTO dm_participants (channel_id, user_id) VALUES (?, ?)")
        .bind(channel_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Remove a participant from a DM channel.
pub async fn remove_participant(
    pool: &SqlitePool,
    channel_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM dm_participants WHERE channel_id = ? AND user_id = ?")
        .bind(channel_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Find an existing 1:1 DM channel between two users.
pub async fn find_existing_dm(
    pool: &SqlitePool,
    user_a: &str,
    user_b: &str,
) -> Result<Option<ChannelRow>, AppError> {
    // Find channels where both users are participants and the channel type is "dm"
    let row = sqlx::query(
        "SELECT c.id, c.type, c.space_id, c.name, c.description, c.topic, c.position, \
         c.parent_id, c.nsfw, c.rate_limit, c.bitrate, c.user_limit, c.owner_id, \
         c.last_message_id, c.archived, c.auto_archive_after, c.created_at \
         FROM channels c \
         INNER JOIN dm_participants p1 ON c.id = p1.channel_id AND p1.user_id = ? \
         INNER JOIN dm_participants p2 ON c.id = p2.channel_id AND p2.user_id = ? \
         WHERE c.type = 'dm' \
         LIMIT 1",
    )
    .bind(user_a)
    .bind(user_b)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| {
        use sqlx::Row;
        ChannelRow {
            id: r.get("id"),
            channel_type: r.get("type"),
            space_id: r.get("space_id"),
            name: r.get("name"),
            description: r.get("description"),
            topic: r.get("topic"),
            position: r.get("position"),
            parent_id: r.get("parent_id"),
            nsfw: r.get("nsfw"),
            rate_limit: r.get("rate_limit"),
            bitrate: r.get("bitrate"),
            user_limit: r.get("user_limit"),
            owner_id: r.get("owner_id"),
            last_message_id: r.get("last_message_id"),
            archived: r.get("archived"),
            auto_archive_after: r.get("auto_archive_after"),
            created_at: r.get("created_at"),
        }
    }))
}

/// Create a DM channel. For 1:1 DMs (single recipient), returns an existing
/// channel if one already exists. For group DMs (multiple recipients), always
/// creates a new channel.
pub async fn create_dm_channel(
    pool: &SqlitePool,
    creator_id: &str,
    recipient_ids: &[String],
) -> Result<ChannelRow, AppError> {
    if recipient_ids.is_empty() {
        return Err(AppError::BadRequest(
            "at least one recipient is required".into(),
        ));
    }

    // For 1:1 DMs, check for an existing channel first
    if recipient_ids.len() == 1 {
        if let Some(existing) = find_existing_dm(pool, creator_id, &recipient_ids[0]).await? {
            // Re-add the creator as a participant in case they left
            add_participant(pool, &existing.id, creator_id).await?;
            return Ok(existing);
        }
    }

    let channel_type = if recipient_ids.len() == 1 {
        "dm"
    } else {
        "group_dm"
    };

    let id = snowflake::generate();
    sqlx::query(
        "INSERT INTO channels (id, type, owner_id, position, nsfw, rate_limit, archived) \
         VALUES (?, ?, ?, 0, 0, 0, 0)",
    )
    .bind(&id)
    .bind(channel_type)
    .bind(creator_id)
    .execute(pool)
    .await?;

    // Add creator and all recipients as participants
    add_participant(pool, &id, creator_id).await?;
    for rid in recipient_ids {
        add_participant(pool, &id, rid).await?;
    }

    db::channels::get_channel_row(pool, &id).await
}

/// Count the number of participants in a DM channel.
pub async fn count_participants(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<i64, AppError> {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM dm_participants WHERE channel_id = ?")
            .bind(channel_id)
            .fetch_one(pool)
            .await?;
    Ok(count)
}
