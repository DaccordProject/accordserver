use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::models::space::{CreateSpace, SpaceRow, UpdateSpace};
use crate::slug;
use crate::snowflake;

fn row_to_space(row: sqlx::sqlite::SqliteRow) -> SpaceRow {
    SpaceRow {
        id: row.get("id"),
        name: row.get("name"),
        slug: row.get("slug"),
        description: row.get("description"),
        icon: row.get("icon"),
        banner: row.get("banner"),
        splash: row.get("splash"),
        owner_id: row.get("owner_id"),
        verification_level: row.get("verification_level"),
        default_notifications: row.get("default_notifications"),
        explicit_content_filter: row.get("explicit_content_filter"),
        vanity_url_code: row.get("vanity_url_code"),
        preferred_locale: row.get("preferred_locale"),
        afk_channel_id: row.get("afk_channel_id"),
        afk_timeout: row.get("afk_timeout"),
        system_channel_id: row.get("system_channel_id"),
        rules_channel_id: row.get("rules_channel_id"),
        nsfw_level: row.get("nsfw_level"),
        premium_tier: row.get("premium_tier"),
        premium_subscription_count: row.get("premium_subscription_count"),
        public: row.get("public"),
        max_members: row.get("max_members"),
        created_at: row.get("created_at"),
    }
}

const SELECT_SPACES: &str = "SELECT id, name, slug, description, icon, banner, splash, owner_id, verification_level, default_notifications, explicit_content_filter, vanity_url_code, preferred_locale, afk_channel_id, afk_timeout, system_channel_id, rules_channel_id, nsfw_level, premium_tier, premium_subscription_count, public, max_members, created_at FROM spaces";

pub async fn get_space_row(pool: &SqlitePool, space_id: &str) -> Result<SpaceRow, AppError> {
    let row = sqlx::query(&format!("{SELECT_SPACES} WHERE id = ?"))
        .bind(space_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_space".to_string()))?;

    Ok(row_to_space(row))
}

pub async fn create_space(
    pool: &SqlitePool,
    owner_id: &str,
    input: &CreateSpace,
) -> Result<SpaceRow, AppError> {
    let id = snowflake::generate();

    // Determine slug: use provided slug (after validation) or generate from name
    let base_slug = match &input.slug {
        Some(s) => {
            slug::validate_slug(s)
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
            s.clone()
        }
        None => slug::slugify(&input.name),
    };
    let final_slug = ensure_unique_slug(pool, &base_slug, None).await?;

    sqlx::query(
        "INSERT INTO spaces (id, name, slug, description, owner_id, public) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.name)
    .bind(&final_slug)
    .bind(&input.description)
    .bind(owner_id)
    .bind(input.public.unwrap_or(false))
    .execute(pool)
    .await?;

    // Create @everyone role with default permissions
    let role_id = snowflake::generate();
    let default_perms =
        serde_json::to_string(&crate::middleware::permissions::DEFAULT_EVERYONE_PERMISSIONS)
            .unwrap();
    sqlx::query(
        "INSERT INTO roles (id, space_id, name, position, permissions) VALUES (?, ?, '@everyone', 0, ?)"
    )
    .bind(&role_id)
    .bind(&id)
    .bind(&default_perms)
    .execute(pool)
    .await?;

    // Create Moderator role at position 1
    let mod_role_id = snowflake::generate();
    let mod_perms =
        serde_json::to_string(&crate::middleware::permissions::MODERATOR_PERMISSIONS).unwrap();
    sqlx::query(
        "INSERT INTO roles (id, space_id, name, color, hoist, position, permissions) VALUES (?, ?, 'Moderator', 3447003, 1, 1, ?)"
    )
    .bind(&mod_role_id)
    .bind(&id)
    .bind(&mod_perms)
    .execute(pool)
    .await?;

    // Create Admin role at position 2
    let admin_role_id = snowflake::generate();
    let admin_perms =
        serde_json::to_string(&crate::middleware::permissions::ADMIN_PERMISSIONS).unwrap();
    sqlx::query(
        "INSERT INTO roles (id, space_id, name, color, hoist, position, permissions) VALUES (?, ?, 'Admin', 15158332, 1, 2, ?)"
    )
    .bind(&admin_role_id)
    .bind(&id)
    .bind(&admin_perms)
    .execute(pool)
    .await?;

    // Create default #general text channel
    let channel_id = snowflake::generate();
    sqlx::query(
        "INSERT INTO channels (id, name, type, space_id, position) VALUES (?, 'general', 'text', ?, 0)"
    )
    .bind(&channel_id)
    .bind(&id)
    .execute(pool)
    .await?;

    // Add the owner as a member
    sqlx::query("INSERT INTO members (user_id, space_id) VALUES (?, ?)")
        .bind(owner_id)
        .bind(&id)
        .execute(pool)
        .await?;

    // Assign Admin role to owner
    sqlx::query("INSERT INTO member_roles (user_id, space_id, role_id) VALUES (?, ?, ?)")
        .bind(owner_id)
        .bind(&id)
        .bind(&admin_role_id)
        .execute(pool)
        .await?;

    get_space_row(pool, &id).await
}

pub async fn update_space(
    pool: &SqlitePool,
    space_id: &str,
    input: &UpdateSpace,
) -> Result<SpaceRow, AppError> {
    let mut sets: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();

    if let Some(ref name) = input.name {
        sets.push("name = ?".to_string());
        values.push(name.clone());
    }
    if let Some(ref new_slug) = input.slug {
        slug::validate_slug(new_slug)
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let final_slug = ensure_unique_slug(pool, new_slug, Some(space_id)).await?;
        sets.push("slug = ?".to_string());
        values.push(final_slug);
    }
    if let Some(ref description) = input.description {
        sets.push("description = ?".to_string());
        values.push(description.clone());
    }
    if let Some(ref icon) = input.icon {
        sets.push("icon = ?".to_string());
        values.push(icon.clone());
    }
    if let Some(ref banner) = input.banner {
        sets.push("banner = ?".to_string());
        values.push(banner.clone());
    }
    if let Some(ref verification_level) = input.verification_level {
        sets.push("verification_level = ?".to_string());
        values.push(verification_level.clone());
    }
    if let Some(ref default_notifications) = input.default_notifications {
        sets.push("default_notifications = ?".to_string());
        values.push(default_notifications.clone());
    }
    if let Some(ref afk_channel_id) = input.afk_channel_id {
        sets.push("afk_channel_id = ?".to_string());
        values.push(afk_channel_id.clone());
    }
    if let Some(ref system_channel_id) = input.system_channel_id {
        sets.push("system_channel_id = ?".to_string());
        values.push(system_channel_id.clone());
    }
    if let Some(ref rules_channel_id) = input.rules_channel_id {
        sets.push("rules_channel_id = ?".to_string());
        values.push(rules_channel_id.clone());
    }
    if let Some(ref preferred_locale) = input.preferred_locale {
        sets.push("preferred_locale = ?".to_string());
        values.push(preferred_locale.clone());
    }

    // Collect integer fields that need separate binding
    let mut int_binds: Vec<i64> = Vec::new();

    if let Some(timeout) = input.afk_timeout {
        sets.push("afk_timeout = ?".to_string());
        int_binds.push(timeout);
    }
    if let Some(public) = input.public {
        sets.push("public = ?".to_string());
        int_binds.push(if public { 1 } else { 0 });
    }

    if sets.is_empty() {
        return get_space_row(pool, space_id).await;
    }

    sets.push("updated_at = datetime('now')".to_string());
    let set_clause = sets.join(", ");
    let query = format!("UPDATE spaces SET {set_clause} WHERE id = ?");
    let mut q = sqlx::query(&query);
    for v in &values {
        q = q.bind(v);
    }
    for val in &int_binds {
        q = q.bind(val);
    }
    q = q.bind(space_id);
    q.execute(pool).await?;

    get_space_row(pool, space_id).await
}

pub async fn delete_space(pool: &SqlitePool, space_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM spaces WHERE id = ?")
        .bind(space_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_public_spaces(pool: &SqlitePool) -> Result<Vec<SpaceRow>, AppError> {
    let rows = sqlx::query(&format!("{SELECT_SPACES} WHERE public = 1"))
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(row_to_space).collect())
}

pub async fn list_space_ids_for_user(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query_as::<_, (String,)>("SELECT space_id FROM members WHERE user_id = ?")
        .bind(user_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn get_space_by_slug(pool: &SqlitePool, slug: &str) -> Result<SpaceRow, AppError> {
    let row = sqlx::query(&format!("{SELECT_SPACES} WHERE slug = ?"))
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_space".to_string()))?;

    Ok(row_to_space(row))
}

/// Ensure a slug is unique in the database. If taken, appends `-2`, `-3`, etc.
/// When `exclude_id` is `Some`, the space with that ID is ignored (for updates).
async fn ensure_unique_slug(
    pool: &SqlitePool,
    base_slug: &str,
    exclude_id: Option<&str>,
) -> Result<String, AppError> {
    let mut candidate = base_slug.to_string();
    let mut suffix = 2u64;

    loop {
        let count: i64 = match exclude_id {
            Some(eid) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM spaces WHERE slug = ? AND id != ?")
                    .bind(&candidate)
                    .bind(eid)
                    .fetch_one(pool)
                    .await?
            }
            None => {
                sqlx::query_scalar("SELECT COUNT(*) FROM spaces WHERE slug = ?")
                    .bind(&candidate)
                    .fetch_one(pool)
                    .await?
            }
        };

        if count == 0 {
            return Ok(candidate);
        }

        candidate = format!("{base_slug}-{suffix}");
        suffix += 1;
    }
}
