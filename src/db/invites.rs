use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::invite::{CreateInvite, Invite};

fn row_to_invite(row: sqlx::any::AnyRow) -> Invite {
    Invite {
        code: row.get("code"),
        space_id: row.get("space_id"),
        channel_id: row.get("channel_id"),
        inviter_id: row.get("inviter_id"),
        max_uses: row.get("max_uses"),
        uses: row.get("uses"),
        max_age: row.get("max_age"),
        temporary: crate::db::get_bool(&row, "temporary"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

const SELECT_INVITES: &str = "SELECT code, space_id, channel_id, inviter_id, max_uses, uses, max_age, temporary, created_at, expires_at FROM invites";

pub async fn get_invite(pool: &AnyPool, code: &str) -> Result<Invite, AppError> {
    let row = sqlx::query(&super::q(&format!("{SELECT_INVITES} WHERE code = ?")))
        .bind(code)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("invite not found".to_string()))?;

    Ok(row_to_invite(row))
}

pub async fn list_space_invites(pool: &AnyPool, space_id: &str) -> Result<Vec<Invite>, AppError> {
    let rows = sqlx::query(&super::q(&format!("{SELECT_INVITES} WHERE space_id = ?")))
        .bind(space_id)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(row_to_invite).collect())
}

pub async fn list_channel_invites(
    pool: &AnyPool,
    channel_id: &str,
) -> Result<Vec<Invite>, AppError> {
    let rows = sqlx::query(&super::q(&format!("{SELECT_INVITES} WHERE channel_id = ?")))
        .bind(channel_id)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(row_to_invite).collect())
}

fn generate_code() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

pub async fn create_invite(
    pool: &AnyPool,
    space_id: &str,
    channel_id: Option<&str>,
    inviter_id: &str,
    input: &CreateInvite,
) -> Result<Invite, AppError> {
    let code = generate_code();
    let max_age = input.max_age;
    // A max_age of 0 (or null) means the invite never expires.
    let expires_at = max_age.filter(|age| *age > 0).map(|age| {
        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::seconds(age);
        expires.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    });

    sqlx::query(
        &super::q("INSERT INTO invites (code, space_id, channel_id, inviter_id, max_uses, max_age, temporary, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
    )
    .bind(&code)
    .bind(space_id)
    .bind(channel_id)
    .bind(inviter_id)
    .bind(input.max_uses)
    .bind(input.max_age)
    .bind(input.temporary.unwrap_or(false))
    .bind(&expires_at)
    .execute(pool)
    .await?;

    get_invite(pool, &code).await
}

/// Ensures a default permanent invite exists for the first space.
/// If no spaces exist, creates a system user and a default "Accord" space.
/// Returns the invite code.
pub async fn ensure_default_invite(pool: &AnyPool) -> Result<String, AppError> {
    // Find the first space
    let space: Option<(String,)> =
        sqlx::query_as("SELECT id FROM spaces ORDER BY created_at ASC LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let space_id = match space {
        Some((id,)) => id,
        None => {
            // Create a system user to own the default space
            let system_user = crate::db::users::create_user(
                pool,
                &crate::models::user::CreateUser {
                    username: "System".to_string(),
                    display_name: Some("System".to_string()),
                },
            )
            .await?;

            sqlx::query(&super::q("UPDATE users SET system = TRUE WHERE id = ?"))
                .bind(&system_user.id)
                .execute(pool)
                .await?;

            // create_space also creates a #general channel and adds the owner as a member
            let space = crate::db::spaces::create_space(
                pool,
                &system_user.id,
                &crate::models::space::CreateSpace {
                    name: "General".to_string(),
                    slug: None,
                    description: Some("Default space".to_string()),
                    public: Some(true),
                    allow_guest_access: None,
                },
            )
            .await?;

            space.id
        }
    };

    // Ensure the space has at least one channel
    let has_channels: Option<(String,)> = sqlx::query_as(&super::q(
        "SELECT id FROM channels WHERE space_id = ? LIMIT 1",
    ))
    .bind(&space_id)
    .fetch_optional(pool)
    .await?;

    if has_channels.is_none() {
        let channel_id = crate::snowflake::generate();
        sqlx::query(
            &super::q("INSERT INTO channels (id, name, type, space_id, position) VALUES (?, 'general', 'text', ?, 0)")
        )
        .bind(&channel_id)
        .bind(&space_id)
        .execute(pool)
        .await?;
        tracing::info!("created default #general channel in space {}", space_id);
    }

    // Check for an existing permanent invite (no expiry, no max uses)
    let existing: Option<(String,)> = sqlx::query_as(
        &super::q("SELECT code FROM invites WHERE space_id = ? AND max_uses IS NULL AND expires_at IS NULL LIMIT 1")
    )
    .bind(&space_id)
    .fetch_optional(pool)
    .await?;

    if let Some((code,)) = existing {
        return Ok(code);
    }

    // Create a permanent space-level invite (no channel)
    let code = generate_code();
    sqlx::query(
        &super::q("INSERT INTO invites (code, space_id, channel_id, inviter_id, max_uses, max_age, temporary) VALUES (?, ?, NULL, NULL, NULL, NULL, FALSE)")
    )
    .bind(&code)
    .bind(&space_id)
    .execute(pool)
    .await?;

    Ok(code)
}

pub async fn delete_invite(pool: &AnyPool, code: &str) -> Result<(), AppError> {
    sqlx::query(&super::q("DELETE FROM invites WHERE code = ?"))
        .bind(code)
        .execute(pool)
        .await?;
    Ok(())
}

/// Serialize finite-invite claims and commit membership and use count together.
pub async fn accept_invite(
    pool: &AnyPool,
    code: &str,
    user_id: &str,
) -> Result<(Invite, bool), AppError> {
    let mut tx = pool.begin().await?;
    // A write first locks this invite on PostgreSQL and avoids SQLite snapshot upgrades.
    let locked = sqlx::query(&super::q("UPDATE invites SET uses = uses WHERE code = ?"))
        .bind(code)
        .execute(&mut *tx)
        .await?;
    if locked.rows_affected() == 0 {
        return Err(AppError::NotFound("invite not found".into()));
    }
    let row = sqlx::query(&super::q(&format!("{SELECT_INVITES} WHERE code = ?")))
        .bind(code)
        .fetch_one(&mut *tx)
        .await?;
    let mut invite = row_to_invite(row);
    let banned: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT user_id FROM bans WHERE space_id = ? AND user_id = ?",
    ))
    .bind(&invite.space_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if banned.is_some() {
        return Err(AppError::Forbidden("you are banned from this space".into()));
    }
    let existing: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT user_id FROM members WHERE space_id = ? AND user_id = ?",
    ))
    .bind(&invite.space_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_some() {
        tx.commit().await?;
        return Ok((invite, false));
    }
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string();
    let result = sqlx::query(&super::q("UPDATE invites SET uses = uses + 1 WHERE code = ? AND (max_uses IS NULL OR max_uses = 0 OR uses < max_uses) AND (expires_at IS NULL OR expires_at > ?)"))
        .bind(code).bind(&now).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::BadRequest("invite expired or exhausted".into()));
    }
    let inserted = sqlx::query(&super::q("INSERT INTO members (space_id, user_id) VALUES (?, ?) ON CONFLICT (space_id, user_id) DO NOTHING"))
        .bind(&invite.space_id).bind(user_id).execute(&mut *tx).await?;
    if inserted.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok((invite, false));
    }
    invite.uses += 1;
    tx.commit().await?;
    Ok((invite, true))
}
