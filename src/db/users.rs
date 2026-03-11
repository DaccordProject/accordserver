use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::user::{CreateUser, UpdateUser, User};
use crate::snowflake;

fn row_to_user(row: sqlx::any::AnyRow) -> User {
    User {
        id: row.get("id"),
        username: row.get("username"),
        display_name: row.get("display_name"),
        avatar: row.get("avatar"),
        banner: row.get("banner"),
        accent_color: row.get("accent_color"),
        bio: row.get("bio"),
        bot: crate::db::get_bool(&row, "bot"),
        system: crate::db::get_bool(&row, "system"),
        is_admin: crate::db::get_bool(&row, "is_admin"),
        mfa_enabled: crate::db::get_bool(&row, "totp_enabled"),
        disabled: crate::db::get_bool(&row, "disabled"),
        flags: row.get("flags"),
        public_flags: row.get("public_flags"),
        created_at: row.get("created_at"),
    }
}

const SELECT_USERS: &str = "SELECT id, username, display_name, avatar, banner, accent_color, bio, bot, system, is_admin, totp_enabled, disabled, flags, public_flags, created_at FROM users";

pub async fn get_user(pool: &AnyPool, user_id: &str) -> Result<User, AppError> {
    let row = sqlx::query(&format!("{SELECT_USERS} WHERE id = ?"))
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_user".to_string()))?;

    Ok(row_to_user(row))
}

pub async fn create_user(pool: &AnyPool, input: &CreateUser) -> Result<User, AppError> {
    let id = snowflake::generate();
    let display_name = input.display_name.as_deref().unwrap_or(&input.username);

    sqlx::query("INSERT INTO users (id, username, display_name) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&input.username)
        .bind(display_name)
        .execute(pool)
        .await?;

    get_user(pool, &id).await
}

pub async fn update_user(
    pool: &AnyPool,
    user_id: &str,
    input: &UpdateUser,
    is_postgres: bool,
) -> Result<User, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
    let mut sets = Vec::new();
    let mut values: Vec<String> = Vec::new();

    if let Some(ref username) = input.username {
        sets.push("username = ?");
        values.push(username.clone());
    }
    if let Some(ref display_name) = input.display_name {
        sets.push("display_name = ?");
        values.push(display_name.clone());
    }
    if let Some(ref avatar) = input.avatar {
        if avatar.is_empty() {
            sets.push("avatar = NULL");
        } else {
            sets.push("avatar = ?");
            values.push(avatar.clone());
        }
    }
    if let Some(ref banner) = input.banner {
        if banner.is_empty() {
            sets.push("banner = NULL");
        } else {
            sets.push("banner = ?");
            values.push(banner.clone());
        }
    }
    if let Some(ref bio) = input.bio {
        sets.push("bio = ?");
        values.push(bio.clone());
    }

    if sets.is_empty() && input.accent_color.is_none() {
        return get_user(pool, user_id).await;
    }

    if let Some(color) = input.accent_color {
        if sets.is_empty() {
            let sql = format!(
                "UPDATE users SET accent_color = ?, updated_at = {now_fn} WHERE id = ?"
            );
            sqlx::query(&sql)
                .bind(color)
                .bind(user_id)
                .execute(pool)
                .await?;
        } else {
            let updated_at_set = format!("updated_at = {now_fn}");
            sets.push(&updated_at_set);
            let set_clause = sets.join(", ");
            let query = format!("UPDATE users SET {set_clause}, accent_color = ? WHERE id = ?");
            let mut q = sqlx::query(&query);
            for v in &values {
                q = q.bind(v);
            }
            q = q.bind(color).bind(user_id);
            q.execute(pool).await?;
        }
    } else {
        let updated_at_set = format!("updated_at = {now_fn}");
        sets.push(&updated_at_set);
        let set_clause = sets.join(", ");
        let query = format!("UPDATE users SET {set_clause} WHERE id = ?");
        let mut q = sqlx::query(&query);
        for v in &values {
            q = q.bind(v);
        }
        q = q.bind(user_id);
        q.execute(pool).await?;
    }

    get_user(pool, user_id).await
}

pub async fn get_user_dm_channels(
    pool: &AnyPool,
    user_id: &str,
) -> Result<Vec<crate::models::channel::ChannelRow>, AppError> {
    let rows = sqlx::query(
        "SELECT id, type, space_id, name, description, topic, position, parent_id, \
         nsfw, rate_limit, bitrate, user_limit, owner_id, last_message_id, \
         archived, auto_archive_after, created_at \
         FROM channels WHERE id IN \
         (SELECT channel_id FROM dm_participants WHERE user_id = ?) \
         ORDER BY last_message_id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| crate::models::channel::ChannelRow {
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
            created_at: row.get("created_at"),
        })
        .collect())
}

pub async fn get_user_spaces(pool: &AnyPool, user_id: &str) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query_as::<_, (String,)>("SELECT space_id FROM members WHERE user_id = ?")
        .bind(user_id)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(|r| r.0).collect())
}
