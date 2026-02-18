use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::emoji::{CreateEmoji, Emoji, UpdateEmoji};
use crate::snowflake;

type EmojiRow = (
    String,
    String,
    bool,
    bool,
    bool,
    bool,
    Option<String>,
    Option<String>,
);

fn row_to_emoji(row: EmojiRow, role_ids: Vec<String>) -> Emoji {
    Emoji {
        id: Some(row.0),
        name: row.1,
        animated: row.2,
        managed: row.3,
        available: row.4,
        require_colons: row.5,
        role_ids,
        creator_id: row.6,
        image_url: row.7,
    }
}

pub async fn get_emoji(pool: &SqlitePool, emoji_id: &str) -> Result<Emoji, AppError> {
    let row = sqlx::query_as::<_, EmojiRow>(
        "SELECT id, name, animated, managed, available, require_colons, creator_id, image_path FROM emojis WHERE id = ?"
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

    Ok(row_to_emoji(row, role_ids))
}

pub async fn list_emojis(pool: &SqlitePool, space_id: &str) -> Result<Vec<Emoji>, AppError> {
    let rows = sqlx::query_as::<_, EmojiRow>(
        "SELECT id, name, animated, managed, available, require_colons, creator_id, image_path FROM emojis WHERE space_id = ?"
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

        emojis.push(row_to_emoji(row, role_ids));
    }
    Ok(emojis)
}

#[allow(clippy::too_many_arguments)]
pub async fn create_emoji(
    pool: &SqlitePool,
    space_id: &str,
    creator_id: &str,
    input: &CreateEmoji,
    image_path: Option<&str>,
    image_content_type: Option<&str>,
    image_size: Option<usize>,
    animated: bool,
) -> Result<Emoji, AppError> {
    let id = snowflake::generate();

    sqlx::query(
        "INSERT INTO emojis (id, space_id, name, creator_id, animated, image_path, image_content_type, image_size) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(space_id)
    .bind(&input.name)
    .bind(creator_id)
    .bind(animated)
    .bind(image_path)
    .bind(image_content_type)
    .bind(image_size.map(|s| s as i64))
    .execute(pool)
    .await?;

    get_emoji(pool, &id).await
}

/// Returns the emoji ID (for use in generating the ID). Used by the route
/// to get the snowflake before saving the file.
pub fn generate_emoji_id() -> String {
    snowflake::generate()
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

/// Delete an emoji. Returns the image_path for file cleanup.
pub async fn delete_emoji(pool: &SqlitePool, emoji_id: &str) -> Result<Option<String>, AppError> {
    let image_path: Option<String> =
        sqlx::query_scalar("SELECT image_path FROM emojis WHERE id = ?")
            .bind(emoji_id)
            .fetch_optional(pool)
            .await?
            .flatten();

    sqlx::query("DELETE FROM emojis WHERE id = ?")
        .bind(emoji_id)
        .execute(pool)
        .await?;

    Ok(image_path)
}
