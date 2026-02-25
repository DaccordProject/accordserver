use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::settings::{ServerSettings, UpdateServerSettings};

pub async fn get_settings(pool: &SqlitePool) -> Result<ServerSettings, AppError> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, Option<String>)>(
        "SELECT max_emoji_size, max_avatar_size, max_sound_size, max_attachment_size, max_attachments_per_message, updated_at FROM server_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;

    Ok(ServerSettings {
        max_emoji_size: row.0,
        max_avatar_size: row.1,
        max_sound_size: row.2,
        max_attachment_size: row.3,
        max_attachments_per_message: row.4,
        updated_at: row.5,
    })
}

pub async fn update_settings(
    pool: &SqlitePool,
    input: &UpdateServerSettings,
) -> Result<ServerSettings, AppError> {
    // Build dynamic SET clause for only the provided fields
    let mut sets = Vec::new();
    if input.max_emoji_size.is_some() {
        sets.push("max_emoji_size = ?");
    }
    if input.max_avatar_size.is_some() {
        sets.push("max_avatar_size = ?");
    }
    if input.max_sound_size.is_some() {
        sets.push("max_sound_size = ?");
    }
    if input.max_attachment_size.is_some() {
        sets.push("max_attachment_size = ?");
    }
    if input.max_attachments_per_message.is_some() {
        sets.push("max_attachments_per_message = ?");
    }

    if sets.is_empty() {
        return get_settings(pool).await;
    }

    sets.push("updated_at = datetime('now')");

    let sql = format!("UPDATE server_settings SET {} WHERE id = 1", sets.join(", "));

    let mut query = sqlx::query(&sql);
    if let Some(v) = input.max_emoji_size {
        query = query.bind(v);
    }
    if let Some(v) = input.max_avatar_size {
        query = query.bind(v);
    }
    if let Some(v) = input.max_sound_size {
        query = query.bind(v);
    }
    if let Some(v) = input.max_attachment_size {
        query = query.bind(v);
    }
    if let Some(v) = input.max_attachments_per_message {
        query = query.bind(v);
    }

    query.execute(pool).await?;

    get_settings(pool).await
}
