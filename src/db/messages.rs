use std::collections::HashMap;

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
        thread_id: row.get("thread_id"),
    }
}

const SELECT_MESSAGES: &str = "SELECT id, channel_id, space_id, author_id, content, type, created_at, edited_at, tts, pinned, mention_everyone, mentions, mention_roles, embeds, reply_to, flags, webhook_id, thread_id FROM messages";

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
    thread_id: Option<&str>,
) -> Result<Vec<MessageRow>, AppError> {
    let rows = match (after, thread_id) {
        (Some(after_id), Some(tid)) => {
            // Thread replies after a cursor
            sqlx::query(&format!(
                "{SELECT_MESSAGES} WHERE channel_id = ? AND thread_id = ? AND id > ? ORDER BY id ASC LIMIT ?"
            ))
            .bind(channel_id)
            .bind(tid)
            .bind(after_id)
            .bind(limit + 1)
            .fetch_all(pool)
            .await?
        }
        (None, Some(tid)) => {
            // Thread replies (oldest first)
            sqlx::query(&format!(
                "{SELECT_MESSAGES} WHERE channel_id = ? AND thread_id = ? ORDER BY id ASC LIMIT ?"
            ))
            .bind(channel_id)
            .bind(tid)
            .bind(limit + 1)
            .fetch_all(pool)
            .await?
        }
        (Some(after_id), None) => {
            // Main channel feed after a cursor (exclude thread replies)
            sqlx::query(&format!(
                "{SELECT_MESSAGES} WHERE channel_id = ? AND thread_id IS NULL AND id > ? ORDER BY id ASC LIMIT ?"
            ))
            .bind(channel_id)
            .bind(after_id)
            .bind(limit + 1)
            .fetch_all(pool)
            .await?
        }
        (None, None) => {
            // Main channel feed (exclude thread replies)
            sqlx::query(&format!(
                "{SELECT_MESSAGES} WHERE channel_id = ? AND thread_id IS NULL ORDER BY id DESC LIMIT ?"
            ))
            .bind(channel_id)
            .bind(limit + 1)
            .fetch_all(pool)
            .await?
        }
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
        "INSERT INTO messages (id, channel_id, space_id, author_id, content, tts, embeds, reply_to, thread_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(channel_id)
    .bind(space_id)
    .bind(author_id)
    .bind(&input.content)
    .bind(input.tts.unwrap_or(false))
    .bind(&embeds_json)
    .bind(&input.reply_to)
    .bind(&input.thread_id)
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
    channel_id: &str,
    message_ids: &[String],
) -> Result<(), AppError> {
    for id in message_ids {
        sqlx::query("DELETE FROM messages WHERE id = ? AND channel_id = ?")
            .bind(id)
            .bind(channel_id)
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

pub struct SearchMessagesParams<'a> {
    pub channel_ids: &'a [String],
    pub query: Option<&'a str>,
    pub author_id: Option<&'a str>,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub pinned: Option<bool>,
    pub cursor: Option<&'a str>,
    pub limit: i64,
}

pub async fn search_messages(
    pool: &SqlitePool,
    space_id: &str,
    params: &SearchMessagesParams<'_>,
) -> Result<Vec<MessageRow>, AppError> {
    if params.channel_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<&str> = params.channel_ids.iter().map(|_| "?").collect();
    let in_clause = placeholders.join(", ");

    let mut sql = format!(
        "{SELECT_MESSAGES} WHERE space_id = ? AND channel_id IN ({in_clause})"
    );
    // We'll track bind values in order after space_id and channel_ids
    let mut bind_strings: Vec<String> = Vec::new();

    if let Some(q) = params.query {
        sql.push_str(" AND content LIKE ?");
        bind_strings.push(format!("%{q}%"));
    }
    if let Some(author) = params.author_id {
        sql.push_str(" AND author_id = ?");
        bind_strings.push(author.to_string());
    }
    if let Some(before) = params.before {
        sql.push_str(" AND created_at < ?");
        bind_strings.push(before.to_string());
    }
    if let Some(after) = params.after {
        sql.push_str(" AND created_at > ?");
        bind_strings.push(after.to_string());
    }
    if let Some(pinned) = params.pinned {
        sql.push_str(" AND pinned = ?");
        bind_strings.push(if pinned { "1".to_string() } else { "0".to_string() });
    }
    if let Some(cursor) = params.cursor {
        sql.push_str(" AND id < ?");
        bind_strings.push(cursor.to_string());
    }

    sql.push_str(" ORDER BY id DESC LIMIT ?");

    let mut q = sqlx::query(&sql);
    q = q.bind(space_id);
    for cid in params.channel_ids {
        q = q.bind(cid);
    }
    for val in &bind_strings {
        q = q.bind(val);
    }
    q = q.bind(params.limit + 1);

    let rows = q.fetch_all(pool).await?;
    Ok(rows.into_iter().map(row_to_message).collect())
}

pub async fn list_pinned_messages(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<MessageRow>, AppError> {
    let rows = sqlx::query(
        "SELECT m.id, m.channel_id, m.space_id, m.author_id, m.content, m.type, m.created_at, m.edited_at, m.tts, m.pinned, m.mention_everyone, m.mentions, m.mention_roles, m.embeds, m.reply_to, m.flags, m.webhook_id, m.thread_id FROM messages m INNER JOIN pinned_messages p ON m.id = p.message_id WHERE p.channel_id = ? ORDER BY p.pinned_at DESC"
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_message).collect())
}

/// Returns the number of thread replies for a given parent message ID.
pub async fn get_thread_reply_count(
    pool: &SqlitePool,
    parent_message_id: &str,
) -> Result<i64, AppError> {
    let row = sqlx::query("SELECT COUNT(*) as cnt FROM messages WHERE thread_id = ?")
        .bind(parent_message_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("cnt"))
}

/// Returns reply counts for multiple parent message IDs in a single query.
/// Result maps parent_message_id -> reply_count.
pub async fn get_thread_reply_counts(
    pool: &SqlitePool,
    message_ids: &[String],
) -> Result<HashMap<String, i64>, AppError> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders: Vec<&str> = message_ids.iter().map(|_| "?").collect();
    let in_clause = placeholders.join(", ");
    let sql = format!(
        "SELECT thread_id, COUNT(*) as cnt FROM messages WHERE thread_id IN ({in_clause}) GROUP BY thread_id"
    );
    let mut q = sqlx::query(&sql);
    for id in message_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    let mut result = HashMap::new();
    for row in &rows {
        let tid: String = row.get("thread_id");
        let cnt: i64 = row.get("cnt");
        result.insert(tid, cnt);
    }
    Ok(result)
}

/// Returns thread metadata for a parent message: reply count, last reply timestamp,
/// and participant user IDs.
pub async fn get_thread_metadata(
    pool: &SqlitePool,
    parent_message_id: &str,
) -> Result<serde_json::Value, AppError> {
    let count_row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM messages WHERE thread_id = ?"
    )
    .bind(parent_message_id)
    .fetch_one(pool)
    .await?;
    let reply_count: i64 = count_row.get("cnt");

    let last_reply_row = sqlx::query(
        "SELECT created_at FROM messages WHERE thread_id = ? ORDER BY id DESC LIMIT 1"
    )
    .bind(parent_message_id)
    .fetch_optional(pool)
    .await?;
    let last_reply_at: Option<String> = last_reply_row.map(|r| r.get("created_at"));

    let participant_rows = sqlx::query(
        "SELECT DISTINCT author_id FROM messages WHERE thread_id = ?"
    )
    .bind(parent_message_id)
    .fetch_all(pool)
    .await?;
    let participants: Vec<String> = participant_rows.iter().map(|r| r.get("author_id")).collect();

    Ok(serde_json::json!({
        "reply_count": reply_count,
        "last_reply_at": last_reply_at,
        "participants": participants,
    }))
}

/// Lists parent messages that have at least one thread reply in a channel.
pub async fn list_active_threads(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<MessageRow>, AppError> {
    let sql = format!(
        "{SELECT_MESSAGES} WHERE channel_id = ? AND id IN (SELECT DISTINCT thread_id FROM messages WHERE thread_id IS NOT NULL AND channel_id = ?) ORDER BY id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(channel_id)
        .bind(channel_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(row_to_message).collect())
}

/// Reaction counts grouped by (message_id, emoji_name), with an `includes_me`
/// flag for the requesting user.
pub struct ReactionAggregate {
    pub emoji_name: String,
    pub count: i64,
    pub includes_me: bool,
}

/// Fetches aggregated reaction data for a set of messages in one query.
/// Returns a map from message_id to its list of reaction aggregates.
pub async fn get_reactions_for_messages(
    pool: &SqlitePool,
    message_ids: &[String],
    current_user_id: Option<&str>,
) -> Result<HashMap<String, Vec<ReactionAggregate>>, AppError> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<&str> = message_ids.iter().map(|_| "?").collect();
    let in_clause = placeholders.join(", ");

    let sql = format!(
        "SELECT message_id, emoji_name, COUNT(*) as cnt \
         FROM reactions WHERE message_id IN ({in_clause}) \
         GROUP BY message_id, emoji_name \
         ORDER BY message_id, MIN(rowid)"
    );

    let mut q = sqlx::query(&sql);
    for id in message_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;

    let mut result: HashMap<String, Vec<ReactionAggregate>> = HashMap::new();
    for row in &rows {
        let msg_id: String = row.get("message_id");
        let emoji_name: String = row.get("emoji_name");
        let count: i64 = row.get("cnt");
        result.entry(msg_id).or_default().push(ReactionAggregate {
            emoji_name,
            count,
            includes_me: false,
        });
    }

    // If we have a current user, check which reactions they've added
    if let Some(user_id) = current_user_id {
        let me_sql = format!(
            "SELECT message_id, emoji_name FROM reactions \
             WHERE message_id IN ({in_clause}) AND user_id = ?"
        );
        let mut mq = sqlx::query(&me_sql);
        for id in message_ids {
            mq = mq.bind(id);
        }
        mq = mq.bind(user_id);
        let me_rows = mq.fetch_all(pool).await?;

        for row in &me_rows {
            let msg_id: String = row.get("message_id");
            let emoji_name: String = row.get("emoji_name");
            if let Some(reactions) = result.get_mut(&msg_id) {
                for r in reactions.iter_mut() {
                    if r.emoji_name == emoji_name {
                        r.includes_me = true;
                    }
                }
            }
        }
    }

    Ok(result)
}
