use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::space::AdminSpaceRow;
use crate::models::user::{AdminUpdateUser, User};

use super::users::get_user;

// -------------------------------------------------------------------------
// Spaces
// -------------------------------------------------------------------------

pub async fn list_all_spaces(
    pool: &AnyPool,
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

    let sql = super::q(&sql);
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
            public: crate::db::get_bool(&row, "public"),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub async fn admin_update_space(
    pool: &AnyPool,
    space_id: &str,
    input: &crate::models::space::AdminUpdateSpace,
    is_postgres: bool,
) -> Result<(), AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
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
        let member_sql = if is_postgres {
            "INSERT INTO members (user_id, space_id) VALUES (?, ?) ON CONFLICT DO NOTHING"
        } else {
            "INSERT OR IGNORE INTO members (user_id, space_id) VALUES (?, ?)"
        };
        sqlx::query(&super::q(member_sql))
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
            let sql = format!("UPDATE spaces SET public = ?, updated_at = {now_fn} WHERE id = ?");
            let sql = super::q(&sql);
            sqlx::query(&sql)
                .bind(public)
                .bind(space_id)
                .execute(pool)
                .await?;
            return Ok(());
        }
        sets.push("public = ?");
    }

    let updated_at_set = format!("updated_at = {now_fn}");
    sets.push(&updated_at_set);
    let set_clause = sets.join(", ");
    let sql = format!("UPDATE spaces SET {set_clause} WHERE id = ?");
    let sql = super::q(&sql);
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
    pool: &AnyPool,
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

    let sql = super::q(&sql);
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
                "bot": crate::db::get_bool(&row, "bot"),
                "system": crate::db::get_bool(&row, "system"),
                "is_admin": crate::db::get_bool(&row, "is_admin"),
                "disabled": crate::db::get_bool(&row, "disabled"),
                "created_at": row.get::<String, _>("created_at"),
                "space_count": row.get::<i64, _>("space_count"),
            })
        })
        .collect())
}

pub async fn admin_update_user(
    pool: &AnyPool,
    user_id: &str,
    input: &AdminUpdateUser,
    is_postgres: bool,
) -> Result<User, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
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

    let updated_at_set = format!("updated_at = {now_fn}");
    sets.push(&updated_at_set);
    let set_clause = sets.join(", ");
    let sql = format!("UPDATE users SET {set_clause} WHERE id = ?");
    let sql = super::q(&sql);
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

pub async fn count_admins(pool: &AnyPool) -> Result<i64, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE is_admin = TRUE")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

pub async fn delete_user(pool: &AnyPool, user_id: &str) -> Result<(), AppError> {
    // Check the user doesn't own any spaces
    let owned: i64 =
        sqlx::query_scalar(&super::q("SELECT COUNT(*) FROM spaces WHERE owner_id = ?"))
            .bind(user_id)
            .fetch_one(pool)
            .await?;
    if owned > 0 {
        return Err(AppError::BadRequest(
            "user owns spaces — transfer ownership before deleting".to_string(),
        ));
    }

    // Manual cascade deletion
    sqlx::query(&super::q("DELETE FROM user_tokens WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM bot_tokens WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM applications WHERE owner_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM reactions WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM dm_participants WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM member_roles WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM members WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM bans WHERE user_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q(
        "UPDATE bans SET banned_by = NULL WHERE banned_by = ?",
    ))
    .bind(user_id)
    .execute(pool)
    .await?;
    sqlx::query(&super::q(
        "UPDATE invites SET inviter_id = NULL WHERE inviter_id = ?",
    ))
    .bind(user_id)
    .execute(pool)
    .await?;
    sqlx::query(&super::q(
        "UPDATE emojis SET creator_id = NULL WHERE creator_id = ?",
    ))
    .bind(user_id)
    .execute(pool)
    .await?;
    sqlx::query(&super::q(
        "UPDATE channels SET owner_id = NULL WHERE owner_id = ?",
    ))
    .bind(user_id)
    .execute(pool)
    .await?;
    sqlx::query(&super::q("DELETE FROM messages WHERE author_id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;
    sqlx::query(&super::q("DELETE FROM users WHERE id = ?"))
        .bind(user_id)
        .execute(pool)
        .await?;

    Ok(())
}
