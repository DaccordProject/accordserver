use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::models::settings::{ServerSettings, UpdateServerSettings};

pub async fn get_settings(pool: &SqlitePool) -> Result<ServerSettings, AppError> {
    let row = sqlx::query(
        "SELECT max_emoji_size, max_avatar_size, max_sound_size, max_attachment_size, \
         max_attachments_per_message, server_name, registration_policy, max_spaces, \
         max_members_per_space, motd, public_listing, updated_at \
         FROM server_settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;

    Ok(ServerSettings {
        max_emoji_size: row.get("max_emoji_size"),
        max_avatar_size: row.get("max_avatar_size"),
        max_sound_size: row.get("max_sound_size"),
        max_attachment_size: row.get("max_attachment_size"),
        max_attachments_per_message: row.get("max_attachments_per_message"),
        server_name: row.get("server_name"),
        registration_policy: row.get("registration_policy"),
        max_spaces: row.get("max_spaces"),
        max_members_per_space: row.get("max_members_per_space"),
        motd: row.get("motd"),
        public_listing: row.get("public_listing"),
        updated_at: row.get("updated_at"),
    })
}

pub async fn update_settings(
    pool: &SqlitePool,
    input: &UpdateServerSettings,
) -> Result<ServerSettings, AppError> {
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
    if input.server_name.is_some() {
        sets.push("server_name = ?");
    }
    if input.registration_policy.is_some() {
        sets.push("registration_policy = ?");
    }
    if input.max_spaces.is_some() {
        sets.push("max_spaces = ?");
    }
    if input.max_members_per_space.is_some() {
        sets.push("max_members_per_space = ?");
    }
    if input.motd.is_some() {
        sets.push("motd = ?");
    }
    if input.public_listing.is_some() {
        sets.push("public_listing = ?");
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
    if let Some(ref v) = input.server_name {
        query = query.bind(v);
    }
    if let Some(ref v) = input.registration_policy {
        query = query.bind(v);
    }
    if let Some(v) = input.max_spaces {
        query = query.bind(v);
    }
    if let Some(v) = input.max_members_per_space {
        query = query.bind(v);
    }
    if let Some(ref v) = input.motd {
        query = query.bind(v);
    }
    if let Some(v) = input.public_listing {
        query = query.bind(v);
    }

    query.execute(pool).await?;

    get_settings(pool).await
}
