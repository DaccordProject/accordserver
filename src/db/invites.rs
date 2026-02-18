use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::invite::{CreateInvite, Invite};

pub async fn get_invite(pool: &SqlitePool, code: &str) -> Result<Invite, AppError> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<i64>, i64, Option<i64>, bool, String, Option<String>)>(
        "SELECT code, space_id, channel_id, inviter_id, max_uses, uses, max_age, temporary, created_at, expires_at FROM invites WHERE code = ?"
    )
    .bind(code)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("invite not found".to_string()))?;

    Ok(Invite {
        code: row.0,
        space_id: row.1,
        channel_id: row.2,
        inviter_id: row.3,
        max_uses: row.4,
        uses: row.5,
        max_age: row.6,
        temporary: row.7,
        created_at: row.8,
        expires_at: row.9,
    })
}

pub async fn list_space_invites(
    pool: &SqlitePool,
    space_id: &str,
) -> Result<Vec<Invite>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<i64>, i64, Option<i64>, bool, String, Option<String>)>(
        "SELECT code, space_id, channel_id, inviter_id, max_uses, uses, max_age, temporary, created_at, expires_at FROM invites WHERE space_id = ?"
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Invite {
            code: row.0,
            space_id: row.1,
            channel_id: row.2,
            inviter_id: row.3,
            max_uses: row.4,
            uses: row.5,
            max_age: row.6,
            temporary: row.7,
            created_at: row.8,
            expires_at: row.9,
        })
        .collect())
}

pub async fn list_channel_invites(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<Invite>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Option<i64>, i64, Option<i64>, bool, String, Option<String>)>(
        "SELECT code, space_id, channel_id, inviter_id, max_uses, uses, max_age, temporary, created_at, expires_at FROM invites WHERE channel_id = ?"
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Invite {
            code: row.0,
            space_id: row.1,
            channel_id: row.2,
            inviter_id: row.3,
            max_uses: row.4,
            uses: row.5,
            max_age: row.6,
            temporary: row.7,
            created_at: row.8,
            expires_at: row.9,
        })
        .collect())
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
    pool: &SqlitePool,
    space_id: &str,
    channel_id: Option<&str>,
    inviter_id: &str,
    input: &CreateInvite,
) -> Result<Invite, AppError> {
    let code = generate_code();
    let max_age = input.max_age;
    let expires_at = max_age.map(|age| {
        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::seconds(age);
        expires.format("%Y-%m-%dT%H:%M:%S").to_string()
    });

    sqlx::query(
        "INSERT INTO invites (code, space_id, channel_id, inviter_id, max_uses, max_age, temporary, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
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
pub async fn ensure_default_invite(pool: &SqlitePool) -> Result<String, AppError> {
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

            sqlx::query("UPDATE users SET system = 1 WHERE id = ?")
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
                },
            )
            .await?;

            space.id
        }
    };

    // Ensure the space has at least one channel
    let has_channels: Option<(String,)> =
        sqlx::query_as("SELECT id FROM channels WHERE space_id = ? LIMIT 1")
            .bind(&space_id)
            .fetch_optional(pool)
            .await?;

    if has_channels.is_none() {
        let channel_id = crate::snowflake::generate();
        sqlx::query(
            "INSERT INTO channels (id, name, type, space_id, position) VALUES (?, 'general', 'text', ?, 0)"
        )
        .bind(&channel_id)
        .bind(&space_id)
        .execute(pool)
        .await?;
        tracing::info!("created default #general channel in space {}", space_id);
    }

    // Check for an existing permanent invite (no expiry, no max uses)
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT code FROM invites WHERE space_id = ? AND max_uses IS NULL AND expires_at IS NULL LIMIT 1"
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
        "INSERT INTO invites (code, space_id, channel_id, inviter_id, max_uses, max_age, temporary) VALUES (?, ?, NULL, NULL, NULL, NULL, 0)"
    )
    .bind(&code)
    .bind(&space_id)
    .execute(pool)
    .await?;

    Ok(code)
}

pub async fn delete_invite(pool: &SqlitePool, code: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM invites WHERE code = ?")
        .bind(code)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn use_invite(pool: &SqlitePool, code: &str) -> Result<Invite, AppError> {
    let invite = get_invite(pool, code).await?;

    // Check if expired
    if let Some(ref expires_at) = invite.expires_at {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        if *expires_at < now {
            return Err(AppError::BadRequest("invite has expired".to_string()));
        }
    }

    // Check max uses
    if let Some(max_uses) = invite.max_uses {
        if invite.uses >= max_uses {
            return Err(AppError::BadRequest(
                "invite has reached max uses".to_string(),
            ));
        }
    }

    // Increment uses
    sqlx::query("UPDATE invites SET uses = uses + 1 WHERE code = ?")
        .bind(code)
        .execute(pool)
        .await?;

    get_invite(pool, code).await
}
