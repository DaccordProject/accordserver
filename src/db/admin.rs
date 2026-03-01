use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::models::space::AdminSpaceRow;
use crate::models::user::{AdminUpdateUser, User};

use super::users::get_user;

// -------------------------------------------------------------------------
// Spaces
// -------------------------------------------------------------------------

pub async fn list_all_spaces(
    pool: &SqlitePool,
    after: Option<&str>,
    limit: i64,
    search: Option<&str>,
) -> Result<Vec<AdminSpaceRow>, AppError> {
    let mut conditions = Vec::new();
    if after.is_some() {
        conditions.push("s.id > ?");
    }
    if search.is_some() {
        conditions.push("s.name LIKE ?");
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "SELECT s.id, s.name, s.slug, s.description, s.icon, s.owner_id, s.public, s.created_at, \
         COUNT(m.user_id) as member_count \
         FROM spaces s LEFT JOIN members m ON m.space_id = s.id \
         {where_clause} \
         GROUP BY s.id ORDER BY s.id LIMIT ?"
    );

    let mut query = sqlx::query(&sql);
    if let Some(a) = after {
        query = query.bind(a);
    }
    if let Some(s) = search {
        query = query.bind(format!("%{s}%"));
    }
    query = query.bind(limit + 1);

    let rows = query.fetch_all(pool).await?;

    Ok(rows
        .into_iter()
        .map(|row| AdminSpaceRow {
            id: row.get("id"),
            name: row.get("name"),
            slug: row.get("slug"),
            description: row.get("description"),
            icon: row.get("icon"),
            owner_id: row.get("owner_id"),
            member_count: row.get("member_count"),
            public: row.get("public"),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub async fn admin_update_space(
    pool: &SqlitePool,
    space_id: &str,
    input: &crate::models::space::AdminUpdateSpace,
) -> Result<(), AppError> {
    let mut sets = Vec::new();
    let mut str_values: Vec<String> = Vec::new();

    if let Some(ref name) = input.name {
        sets.push("name = ?");
        str_values.push(name.clone());
    }
    if let Some(ref description) = input.description {
        sets.push("description = ?");
        str_values.push(description.clone());
    }
    if let Some(ref owner_id) = input.owner_id {
        sets.push("owner_id = ?");
        str_values.push(owner_id.clone());

        // Ensure the new owner is a member of the space
        sqlx::query("INSERT OR IGNORE INTO members (user_id, space_id) VALUES (?, ?)")
            .bind(owner_id)
            .bind(space_id)
            .execute(pool)
            .await?;
    }

    if sets.is_empty() && input.public.is_none() {
        return Ok(());
    }

    if let Some(public) = input.public {
        if sets.is_empty() {
            sqlx::query("UPDATE spaces SET public = ?, updated_at = datetime('now') WHERE id = ?")
                .bind(public)
                .bind(space_id)
                .execute(pool)
                .await?;
            return Ok(());
        }
        sets.push("public = ?");
    }

    sets.push("updated_at = datetime('now')");
    let set_clause = sets.join(", ");
    let sql = format!("UPDATE spaces SET {set_clause} WHERE id = ?");
    let mut query = sqlx::query(&sql);
    for v in &str_values {
        query = query.bind(v);
    }
    if let Some(public) = input.public {
        query = query.bind(public);
    }
    query = query.bind(space_id);
    query.execute(pool).await?;

    Ok(())
}

// -------------------------------------------------------------------------
// Users
// -------------------------------------------------------------------------

pub async fn list_all_users(
    pool: &SqlitePool,
    after: Option<&str>,
    limit: i64,
    search: Option<&str>,
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut conditions = Vec::new();
    if after.is_some() {
        conditions.push("u.id > ?");
    }
    if search.is_some() {
        conditions.push("u.username LIKE ?");
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let sql = format!(
        "SELECT u.id, u.username, u.display_name, u.avatar, u.bot, u.system, \
         u.is_admin, u.disabled, u.created_at, \
         COALESCE(mc.space_count, 0) as space_count \
         FROM users u \
         LEFT JOIN (SELECT user_id, COUNT(*) as space_count FROM members GROUP BY user_id) mc \
         ON mc.user_id = u.id \
         {where_clause} \
         ORDER BY u.id LIMIT ?"
    );

    let mut query = sqlx::query(&sql);
    if let Some(a) = after {
        query = query.bind(a);
    }
    if let Some(s) = search {
        query = query.bind(format!("%{s}%"));
    }
    query = query.bind(limit + 1);

    let rows = query.fetch_all(pool).await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get::<String, _>("id"),
                "username": row.get::<String, _>("username"),
                "display_name": row.get::<Option<String>, _>("display_name"),
                "avatar": row.get::<Option<String>, _>("avatar"),
                "bot": row.get::<bool, _>("bot"),
                "system": row.get::<bool, _>("system"),
                "is_admin": row.get::<bool, _>("is_admin"),
                "disabled": row.get::<bool, _>("disabled"),
                "created_at": row.get::<String, _>("created_at"),
                "space_count": row.get::<i64, _>("space_count"),
            })
        })
        .collect())
}

pub async fn admin_update_user(
    pool: &SqlitePool,
    user_id: &str,
    input: &AdminUpdateUser,
) -> Result<User, AppError> {
    let mut sets = Vec::new();
    let mut str_values: Vec<String> = Vec::new();

    if let Some(ref username) = input.username {
        sets.push("username = ?");
        str_values.push(username.clone());
    }
    if let Some(ref display_name) = input.display_name {
        sets.push("display_name = ?");
        str_values.push(display_name.clone());
    }

    // Boolean fields tracked separately for correct bind order
    let mut bool_binds: Vec<bool> = Vec::new();
    if let Some(v) = input.is_admin {
        sets.push("is_admin = ?");
        bool_binds.push(v);
    }
    if let Some(v) = input.disabled {
        sets.push("disabled = ?");
        bool_binds.push(v);
    }
    if let Some(v) = input.force_password_reset {
        sets.push("force_password_reset = ?");
        bool_binds.push(v);
    }

    if sets.is_empty() {
        return get_user(pool, user_id).await;
    }

    sets.push("updated_at = datetime('now')");
    let set_clause = sets.join(", ");
    let sql = format!("UPDATE users SET {set_clause} WHERE id = ?");
    let mut query = sqlx::query(&sql);
    for v in &str_values {
        query = query.bind(v);
    }
    for v in &bool_binds {
        query = query.bind(v);
    }
    query = query.bind(user_id);
    query.execute(pool).await?;

    get_user(pool, user_id).await
}

pub async fn count_admins(pool: &SqlitePool) -> Result<i64, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = 1")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

pub async fn delete_user(pool: &SqlitePool, user_id: &str) -> Result<(), AppError> {
    // Check the user doesn't own any spaces
    let owned: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM spaces WHERE owner_id = ?")
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    if owned > 0 {
        return Err(AppError::BadRequest(
            "user owns spaces — transfer ownership before deleting".to_string(),
        ));
    }

    // Manual cascade deletion (SQLite has no CASCADE from users)
    sqlx::query("DELETE FROM user_tokens WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM bot_tokens WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM applications WHERE owner_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM reactions WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM dm_participants WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM member_roles WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM members WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM bans WHERE user_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE bans SET banned_by = NULL WHERE banned_by = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE invites SET inviter_id = NULL WHERE inviter_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE emojis SET creator_id = NULL WHERE creator_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE channels SET owner_id = NULL WHERE owner_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM messages WHERE author_id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;

    Ok(())
}
