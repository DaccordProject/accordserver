use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::emoji::{CreateEmoji, Emoji, UpdateEmoji};
use crate::snowflake;

fn row_to_emoji(row: sqlx::any::AnyRow, role_ids: Vec<String>) -> Emoji {
    Emoji {
        id: Some(row.get("id")),
        name: row.get("name"),
        animated: crate::db::get_bool(&row, "animated"),
        managed: crate::db::get_bool(&row, "managed"),
        available: crate::db::get_bool(&row, "available"),
        require_colons: crate::db::get_bool(&row, "require_colons"),
        role_ids,
        creator_id: row.get("creator_id"),
        image_url: row.get("image_path"),
    }
}

/// Verify an emoji belongs to the given space. Returns an error if it doesn't.
pub async fn require_emoji_in_space(
    pool: &AnyPool,
    emoji_id: &str,
    space_id: &str,
) -> Result<(), AppError> {
    let row: Option<(String,)> =
        sqlx::query_as(&super::q("SELECT space_id FROM emojis WHERE id = ?"))
            .bind(emoji_id)
            .fetch_optional(pool)
            .await?;
    match row {
        Some((sid,)) if sid == space_id => Ok(()),
        Some(_) => Err(AppError::NotFound(
            "emoji not found in this space".to_string(),
        )),
        None => Err(AppError::NotFound("unknown_emoji".to_string())),
    }
}

pub async fn get_emoji(pool: &AnyPool, emoji_id: &str) -> Result<Emoji, AppError> {
    let row = sqlx::query(
        &super::q("SELECT id, name, animated, managed, available, require_colons, creator_id, image_path FROM emojis WHERE id = ?")
    )
    .bind(emoji_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown_emoji".to_string()))?;

    let role_ids = sqlx::query_as::<_, (String,)>(&super::q(
        "SELECT role_id FROM emoji_roles WHERE emoji_id = ?",
    ))
    .bind(emoji_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| r.0)
    .collect();

    Ok(row_to_emoji(row, role_ids))
}

pub async fn list_emojis(pool: &AnyPool, space_id: &str) -> Result<Vec<Emoji>, AppError> {
    let rows = sqlx::query(
        &super::q("SELECT id, name, animated, managed, available, require_colons, creator_id, image_path FROM emojis WHERE space_id = ?")
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    let mut emojis = Vec::new();
    for row in rows {
        let emoji_id: String = row.get("id");
        let role_ids = sqlx::query_as::<_, (String,)>(&super::q(
            "SELECT role_id FROM emoji_roles WHERE emoji_id = ?",
        ))
        .bind(&emoji_id)
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
    pool: &AnyPool,
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
        &super::q("INSERT INTO emojis (id, space_id, name, creator_id, animated, image_path, image_content_type, image_size) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
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
    pool: &AnyPool,
    emoji_id: &str,
    input: &UpdateEmoji,
    is_postgres: bool,
) -> Result<Emoji, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
    if let Some(ref name) = input.name {
        let sql = format!("UPDATE emojis SET name = ?, updated_at = {now_fn} WHERE id = ?");
        sqlx::query(&super::q(&sql))
            .bind(name)
            .bind(emoji_id)
            .execute(pool)
            .await?;
    }
    get_emoji(pool, emoji_id).await
}

/// The home domain of an emoji, or `None` if it is locally homed. Used by the
/// inbound applier to confirm a delete only ever touches a replica row homed on
/// the signing peer (S2).
pub async fn emoji_origin(pool: &AnyPool, emoji_id: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query(&super::q("SELECT origin FROM emojis WHERE id = ?"))
        .bind(emoji_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|r| r.try_get::<String, _>("origin").ok()))
}

pub async fn emoji_space_id(pool: &AnyPool, emoji_id: &str) -> Result<Option<String>, AppError> {
    let row = sqlx::query(&super::q("SELECT space_id FROM emojis WHERE id = ?"))
        .bind(emoji_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|r| r.try_get::<String, _>("space_id").ok()))
}

/// Mirror a remote space's emoji (`origin = <home domain>`). Idempotent on the
/// qualified emoji ID. `image_url` is an absolute home-server URL (the image is
/// not mirrored). Role restrictions are replaced wholesale, but only for roles
/// we already mirror — a role we don't have would violate the FK, so it is
/// silently dropped from the restriction set.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_remote_emoji(
    pool: &AnyPool,
    id: &str,
    origin: &str,
    space_id: &str,
    name: &str,
    animated: bool,
    image_url: Option<&str>,
    role_ids: &[String],
) -> Result<(), AppError> {
    sqlx::query(
        &super::q("INSERT INTO emojis (id, space_id, name, animated, managed, image_path, origin) \
         VALUES (?, ?, ?, ?, TRUE, ?, ?) \
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, animated = excluded.animated, image_path = excluded.image_path, origin = excluded.origin"),
    )
    .bind(id)
    .bind(space_id)
    .bind(name)
    .bind(animated)
    .bind(image_url)
    .bind(origin)
    .execute(pool)
    .await?;

    sqlx::query(&super::q("DELETE FROM emoji_roles WHERE emoji_id = ?"))
        .bind(id)
        .execute(pool)
        .await?;
    for role_id in role_ids {
        let exists: Option<(String,)> =
            sqlx::query_as(&super::q("SELECT id FROM roles WHERE id = ?"))
                .bind(role_id)
                .fetch_optional(pool)
                .await?;
        if exists.is_some() {
            sqlx::query(&super::q(
                "INSERT INTO emoji_roles (emoji_id, role_id) VALUES (?, ?) ON CONFLICT DO NOTHING",
            ))
            .bind(id)
            .bind(role_id)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Delete an emoji. Returns the image_path for file cleanup.
pub async fn delete_emoji(pool: &AnyPool, emoji_id: &str) -> Result<Option<String>, AppError> {
    let image_path: Option<String> =
        sqlx::query_scalar(&super::q("SELECT image_path FROM emojis WHERE id = ?"))
            .bind(emoji_id)
            .fetch_optional(pool)
            .await?
            .flatten();

    sqlx::query(&super::q("DELETE FROM emojis WHERE id = ?"))
        .bind(emoji_id)
        .execute(pool)
        .await?;

    Ok(image_path)
}
