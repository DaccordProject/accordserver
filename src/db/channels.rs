use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::models::channel::{ChannelRow, CreateChannel, UpdateChannel};
use crate::snowflake;

fn row_to_channel(row: sqlx::sqlite::SqliteRow) -> ChannelRow {
    ChannelRow {
        id: row.get("id"),
        channel_type: row.get("type"),
        space_id: row.get("space_id"),
        name: row.get("name"),
        description: row.get("description"),
        topic: row.get("topic"),
        position: row.get("position"),
        parent_id: row.get("parent_id"),
        nsfw: row.get("nsfw"),
        rate_limit: row.get("rate_limit"),
        bitrate: row.get("bitrate"),
        user_limit: row.get("user_limit"),
        owner_id: row.get("owner_id"),
        last_message_id: row.get("last_message_id"),
        archived: row.get("archived"),
        auto_archive_after: row.get("auto_archive_after"),
        created_at: row.get("created_at"),
    }
}

const SELECT_CHANNELS: &str = "SELECT id, type, space_id, name, description, topic, position, parent_id, nsfw, rate_limit, bitrate, user_limit, owner_id, last_message_id, archived, auto_archive_after, created_at FROM channels";

pub async fn get_channel_row(pool: &SqlitePool, channel_id: &str) -> Result<ChannelRow, AppError> {
    let row = sqlx::query(&format!("{SELECT_CHANNELS} WHERE id = ?"))
        .bind(channel_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_channel".to_string()))?;

    Ok(row_to_channel(row))
}

pub async fn list_channels_in_space(
    pool: &SqlitePool,
    space_id: &str,
) -> Result<Vec<ChannelRow>, AppError> {
    let rows = sqlx::query(&format!(
        "{SELECT_CHANNELS} WHERE space_id = ? ORDER BY position"
    ))
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_channel).collect())
}

pub async fn create_channel(
    pool: &SqlitePool,
    space_id: &str,
    input: &CreateChannel,
) -> Result<ChannelRow, AppError> {
    let id = snowflake::generate();
    let position = input.position.unwrap_or(0);

    sqlx::query(
        "INSERT INTO channels (id, name, type, space_id, topic, parent_id, nsfw, bitrate, user_limit, rate_limit, position) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&input.name)
    .bind(&input.channel_type)
    .bind(space_id)
    .bind(&input.topic)
    .bind(&input.parent_id)
    .bind(input.nsfw.unwrap_or(false))
    .bind(input.bitrate)
    .bind(input.user_limit)
    .bind(input.rate_limit.unwrap_or(0))
    .bind(position)
    .execute(pool)
    .await?;

    get_channel_row(pool, &id).await
}

pub async fn update_channel(
    pool: &SqlitePool,
    channel_id: &str,
    input: &UpdateChannel,
) -> Result<ChannelRow, AppError> {
    let mut sets = Vec::new();
    let mut str_values: Vec<Option<String>> = Vec::new();
    let mut int_values: Vec<(String, i64)> = Vec::new();

    if let Some(ref name) = input.name {
        sets.push("name = ?".to_string());
        str_values.push(Some(name.clone()));
    }
    if let Some(ref topic) = input.topic {
        sets.push("topic = ?".to_string());
        str_values.push(Some(topic.clone()));
    }
    if let Some(ref parent_id) = input.parent_id {
        sets.push("parent_id = ?".to_string());
        str_values.push(Some(parent_id.clone()));
    }

    if let Some(position) = input.position {
        int_values.push(("position".to_string(), position));
    }
    if let Some(nsfw) = input.nsfw {
        int_values.push(("nsfw".to_string(), nsfw as i64));
    }
    if let Some(rate_limit) = input.rate_limit {
        int_values.push(("rate_limit".to_string(), rate_limit));
    }
    if let Some(bitrate) = input.bitrate {
        int_values.push(("bitrate".to_string(), bitrate));
    }
    if let Some(user_limit) = input.user_limit {
        int_values.push(("user_limit".to_string(), user_limit));
    }
    if let Some(archived) = input.archived {
        int_values.push(("archived".to_string(), archived as i64));
    }

    for (col, _) in &int_values {
        sets.push(format!("{col} = ?"));
    }

    if sets.is_empty() {
        return get_channel_row(pool, channel_id).await;
    }

    sets.push("updated_at = datetime('now')".to_string());
    let set_clause = sets.join(", ");
    let query = format!("UPDATE channels SET {set_clause} WHERE id = ?");
    let mut q = sqlx::query(&query);
    for v in &str_values {
        q = q.bind(v);
    }
    for (_, val) in &int_values {
        q = q.bind(val);
    }
    q = q.bind(channel_id);
    q.execute(pool).await?;

    get_channel_row(pool, channel_id).await
}

pub async fn delete_channel(pool: &SqlitePool, channel_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM channels WHERE id = ?")
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reorder_channels(
    pool: &SqlitePool,
    space_id: &str,
    updates: &[(String, i64)],
) -> Result<(), AppError> {
    for (id, position) in updates {
        sqlx::query("UPDATE channels SET position = ? WHERE id = ? AND space_id = ?")
            .bind(position)
            .bind(id)
            .bind(space_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}
