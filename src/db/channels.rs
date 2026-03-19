use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::channel::{ChannelRow, CreateChannel, UpdateChannel};
use crate::snowflake;

fn row_to_channel(row: sqlx::any::AnyRow) -> ChannelRow {
    ChannelRow {
        id: row.get("id"),
        channel_type: row.get("type"),
        space_id: row.get("space_id"),
        name: row.get("name"),
        description: row.get("description"),
        topic: row.get("topic"),
        position: row.get("position"),
        parent_id: row.get("parent_id"),
        nsfw: crate::db::get_bool(&row, "nsfw"),
        rate_limit: row.get("rate_limit"),
        bitrate: row.get("bitrate"),
        user_limit: row.get("user_limit"),
        owner_id: row.get("owner_id"),
        last_message_id: row.get("last_message_id"),
        archived: crate::db::get_bool(&row, "archived"),
        auto_archive_after: row.get("auto_archive_after"),
        allow_anonymous_read: crate::db::get_bool(&row, "allow_anonymous_read"),
        created_at: row.get("created_at"),
    }
}

const SELECT_CHANNELS: &str = "SELECT id, type, space_id, name, description, topic, position, parent_id, nsfw, rate_limit, bitrate, user_limit, owner_id, last_message_id, archived, auto_archive_after, allow_anonymous_read, created_at FROM channels";

pub async fn get_channel_row(pool: &AnyPool, channel_id: &str) -> Result<ChannelRow, AppError> {
    let row = sqlx::query(&super::q(&format!("{SELECT_CHANNELS} WHERE id = ?")))
        .bind(channel_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_channel".to_string()))?;

    Ok(row_to_channel(row))
}

pub async fn list_channels_in_space(
    pool: &AnyPool,
    space_id: &str,
) -> Result<Vec<ChannelRow>, AppError> {
    let rows = sqlx::query(&super::q(&format!(
        "{SELECT_CHANNELS} WHERE space_id = ? ORDER BY position"
    )))
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_channel).collect())
}

pub async fn create_channel(
    pool: &AnyPool,
    space_id: &str,
    input: &CreateChannel,
) -> Result<ChannelRow, AppError> {
    let id = snowflake::generate();
    let position = input.position.unwrap_or(0);

    sqlx::query(&super::q(
        "INSERT INTO channels (id, name, type, space_id, topic, parent_id, nsfw, bitrate, user_limit, rate_limit, position, allow_anonymous_read) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
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
    .bind(input.allow_anonymous_read.unwrap_or(false))
    .execute(pool)
    .await?;

    get_channel_row(pool, &id).await
}

pub async fn update_channel(
    pool: &AnyPool,
    channel_id: &str,
    input: &UpdateChannel,
    is_postgres: bool,
) -> Result<ChannelRow, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
    let mut sets = Vec::new();
    let mut str_values: Vec<Option<String>> = Vec::new();
    let mut int_values: Vec<(String, i64)> = Vec::new();
    let mut bool_values: Vec<(String, bool)> = Vec::new();

    if let Some(ref name) = input.name {
        sets.push("name = ?".to_string());
        str_values.push(Some(name.clone()));
    }
    if let Some(ref topic) = input.topic {
        sets.push("topic = ?".to_string());
        str_values.push(Some(topic.clone()));
    }
    if let Some(parent_id) = &input.parent_id {
        sets.push("parent_id = ?".to_string());
        str_values.push(parent_id.clone());
    }

    if let Some(position) = input.position {
        int_values.push(("position".to_string(), position));
    }
    if let Some(nsfw) = input.nsfw {
        bool_values.push(("nsfw".to_string(), nsfw));
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
        bool_values.push(("archived".to_string(), archived));
    }
    if let Some(allow_anonymous_read) = input.allow_anonymous_read {
        bool_values.push(("allow_anonymous_read".to_string(), allow_anonymous_read));
    }

    for (col, _) in &int_values {
        sets.push(format!("{col} = ?"));
    }
    for (col, _) in &bool_values {
        sets.push(format!("{col} = ?"));
    }

    if sets.is_empty() {
        return get_channel_row(pool, channel_id).await;
    }

    sets.push(format!("updated_at = {now_fn}"));
    let set_clause = sets.join(", ");
    let query = format!("UPDATE channels SET {set_clause} WHERE id = ?");
    let query = super::q(&query);
    let mut q = sqlx::query(&query);
    for v in &str_values {
        q = q.bind(v);
    }
    for (_, val) in &int_values {
        q = q.bind(val);
    }
    for (_, val) in &bool_values {
        q = q.bind(val);
    }
    q = q.bind(channel_id);
    q.execute(pool).await?;

    get_channel_row(pool, channel_id).await
}

pub async fn delete_channel(pool: &AnyPool, channel_id: &str) -> Result<(), AppError> {
    sqlx::query(&super::q("DELETE FROM channels WHERE id = ?"))
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reorder_channels(
    pool: &AnyPool,
    space_id: &str,
    updates: &[(String, i64)],
) -> Result<(), AppError> {
    for (id, position) in updates {
        sqlx::query(&super::q(
            "UPDATE channels SET position = ? WHERE id = ? AND space_id = ?",
        ))
        .bind(position)
        .bind(id)
        .bind(space_id)
        .execute(pool)
        .await?;
    }
    Ok(())
}
