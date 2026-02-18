use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::models::message::{CreateMessage, MessageRow, UpdateMessage};
use crate::snowflake;

fn row_to_message(row: sqlx::sqlite::SqliteRow) -> MessageRow {
    MessageRow {
        id: row.get("id"),
        channel_id: row.get("channel_id"),
        space_id: row.get("space_id"),
        author_id: row.get("author_id"),
        content: row.get("content"),
        message_type: row.get("type"),
        created_at: row.get("created_at"),
        edited_at: row.get("edited_at"),
        tts: row.get("tts"),
        pinned: row.get("pinned"),
        mention_everyone: row.get("mention_everyone"),
        mentions: row.get("mentions"),
        mention_roles: row.get("mention_roles"),
        embeds: row.get("embeds"),
        reply_to: row.get("reply_to"),
        flags: row.get("flags"),
        webhook_id: row.get("webhook_id"),
    }
}

const SELECT_MESSAGES: &str = "SELECT id, channel_id, space_id, author_id, content, type, created_at, edited_at, tts, pinned, mention_everyone, mentions, mention_roles, embeds, reply_to, flags, webhook_id FROM messages";

pub async fn get_message_row(pool: &SqlitePool, message_id: &str) -> Result<MessageRow, AppError> {
    let row = sqlx::query(&format!("{SELECT_MESSAGES} WHERE id = ?"))
        .bind(message_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_message".to_string()))?;

    Ok(row_to_message(row))
}

pub async fn list_messages(
    pool: &SqlitePool,
    channel_id: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<MessageRow>, AppError> {
    let rows = if let Some(after_id) = after {
        sqlx::query(&format!(
            "{SELECT_MESSAGES} WHERE channel_id = ? AND id > ? ORDER BY id ASC LIMIT ?"
        ))
        .bind(channel_id)
        .bind(after_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(&format!(
            "{SELECT_MESSAGES} WHERE channel_id = ? ORDER BY id DESC LIMIT ?"
        ))
        .bind(channel_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    };

    Ok(rows.into_iter().map(row_to_message).collect())
}

pub async fn create_message(
    pool: &SqlitePool,
    channel_id: &str,
    author_id: &str,
    space_id: Option<&str>,
    input: &CreateMessage,
) -> Result<MessageRow, AppError> {
    let id = snowflake::generate();
    let embeds_json = serde_json::to_string(&input.embeds.as_deref().unwrap_or(&[])).unwrap();

    sqlx::query(
        "INSERT INTO messages (id, channel_id, space_id, author_id, content, tts, embeds, reply_to) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(channel_id)
    .bind(space_id)
    .bind(author_id)
    .bind(&input.content)
    .bind(input.tts.unwrap_or(false))
    .bind(&embeds_json)
    .bind(&input.reply_to)
    .execute(pool)
    .await?;

    // Update last_message_id on the channel
    sqlx::query("UPDATE channels SET last_message_id = ? WHERE id = ?")
        .bind(&id)
        .bind(channel_id)
        .execute(pool)
        .await?;

    get_message_row(pool, &id).await
}

pub async fn update_message(
    pool: &SqlitePool,
    message_id: &str,
    input: &UpdateMessage,
) -> Result<MessageRow, AppError> {
    if let Some(ref content) = input.content {
        sqlx::query("UPDATE messages SET content = ?, edited_at = datetime('now'), updated_at = datetime('now') WHERE id = ?")
            .bind(content)
            .bind(message_id)
            .execute(pool)
            .await?;
    }
    if let Some(ref embeds) = input.embeds {
        let embeds_json = serde_json::to_string(embeds).unwrap();
        sqlx::query("UPDATE messages SET embeds = ?, edited_at = datetime('now'), updated_at = datetime('now') WHERE id = ?")
            .bind(&embeds_json)
            .bind(message_id)
            .execute(pool)
            .await?;
    }
    get_message_row(pool, message_id).await
}

pub async fn delete_message(pool: &SqlitePool, message_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(message_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn bulk_delete_messages(
    pool: &SqlitePool,
    message_ids: &[String],
) -> Result<(), AppError> {
    for id in message_ids {
        sqlx::query("DELETE FROM messages WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn pin_message(
    pool: &SqlitePool,
    channel_id: &str,
    message_id: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT OR IGNORE INTO pinned_messages (channel_id, message_id) VALUES (?, ?)")
        .bind(channel_id)
        .bind(message_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE messages SET pinned = 1 WHERE id = ?")
        .bind(message_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn unpin_message(
    pool: &SqlitePool,
    channel_id: &str,
    message_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM pinned_messages WHERE channel_id = ? AND message_id = ?")
        .bind(channel_id)
        .bind(message_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE messages SET pinned = 0 WHERE id = ?")
        .bind(message_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_pinned_messages(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<MessageRow>, AppError> {
    let rows = sqlx::query(
        "SELECT m.id, m.channel_id, m.space_id, m.author_id, m.content, m.type, m.created_at, m.edited_at, m.tts, m.pinned, m.mention_everyone, m.mentions, m.mention_roles, m.embeds, m.reply_to, m.flags, m.webhook_id FROM messages m INNER JOIN pinned_messages p ON m.id = p.message_id WHERE p.channel_id = ? ORDER BY p.pinned_at DESC"
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_message).collect())
}
