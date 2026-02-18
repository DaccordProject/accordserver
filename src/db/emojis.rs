use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::emoji::{CreateEmoji, Emoji, UpdateEmoji};
use crate::snowflake;

pub async fn get_emoji(pool: &SqlitePool, emoji_id: &str) -> Result<Emoji, AppError> {
    let row = sqlx::query_as::<_, (String, String, bool, bool, bool, bool, Option<String>)>(
        "SELECT id, name, animated, managed, available, require_colons, creator_id FROM emojis WHERE id = ?"
    )
    .bind(emoji_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown_emoji".to_string()))?;

    let role_ids =
        sqlx::query_as::<_, (String,)>("SELECT role_id FROM emoji_roles WHERE emoji_id = ?")
            .bind(emoji_id)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect();

    Ok(Emoji {
        id: Some(row.0),
        name: row.1,
        animated: row.2,
        managed: row.3,
        available: row.4,
        require_colons: row.5,
        role_ids,
        creator_id: row.6,
    })
}

pub async fn list_emojis(pool: &SqlitePool, space_id: &str) -> Result<Vec<Emoji>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, bool, bool, bool, bool, Option<String>)>(
        "SELECT id, name, animated, managed, available, require_colons, creator_id FROM emojis WHERE space_id = ?"
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    let mut emojis = Vec::new();
    for row in rows {
        let role_ids =
            sqlx::query_as::<_, (String,)>("SELECT role_id FROM emoji_roles WHERE emoji_id = ?")
                .bind(&row.0)
                .fetch_all(pool)
                .await?
                .into_iter()
                .map(|r| r.0)
                .collect();

        emojis.push(Emoji {
            id: Some(row.0),
            name: row.1,
            animated: row.2,
            managed: row.3,
            available: row.4,
            require_colons: row.5,
            role_ids,
            creator_id: row.6,
        });
    }
    Ok(emojis)
}

pub async fn create_emoji(
    pool: &SqlitePool,
    space_id: &str,
    creator_id: &str,
    input: &CreateEmoji,
) -> Result<Emoji, AppError> {
    let id = snowflake::generate();

    sqlx::query("INSERT INTO emojis (id, space_id, name, creator_id) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(space_id)
        .bind(&input.name)
        .bind(creator_id)
        .execute(pool)
        .await?;

    get_emoji(pool, &id).await
}

pub async fn update_emoji(
    pool: &SqlitePool,
    emoji_id: &str,
    input: &UpdateEmoji,
) -> Result<Emoji, AppError> {
    if let Some(ref name) = input.name {
        sqlx::query("UPDATE emojis SET name = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(name)
            .bind(emoji_id)
            .execute(pool)
            .await?;
    }
    get_emoji(pool, emoji_id).await
}

pub async fn delete_emoji(pool: &SqlitePool, emoji_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM emojis WHERE id = ?")
        .bind(emoji_id)
        .execute(pool)
        .await?;
    Ok(())
}
