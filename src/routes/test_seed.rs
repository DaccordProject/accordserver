use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use sqlx::SqlitePool;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::{create_token_hash, generate_token};
use crate::models::channel::CreateChannel;
use crate::models::space::{CreateSpace, SpaceRow};
use crate::models::user::{CreateUser, User};
use crate::snowflake;
use crate::state::AppState;

pub async fn seed(State(state): State<AppState>) -> impl IntoResponse {
    if !state.test_mode {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "code": "not_found",
                    "message": "not found"
                }
            })),
        );
    }

    match do_seed(&state).await {
        Ok(data) => (StatusCode::OK, Json(json!({ "data": data }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": {
                    "code": "seed_failed",
                    "message": format!("{e:?}")
                }
            })),
        ),
    }
}

async fn do_seed(state: &AppState) -> Result<serde_json::Value, AppError> {
    let pool = &state.db;

    // 1. Find or create the bearer user, then rotate its token
    let user = find_or_create_user(pool, "test_user", "Test User").await?;

    sqlx::query("DELETE FROM user_tokens WHERE user_id = ?")
        .bind(&user.id)
        .execute(pool)
        .await?;

    let user_token = generate_token();
    let user_token_hash = create_token_hash(&user_token);

    sqlx::query(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, '2099-12-31T23:59:59')",
    )
    .bind(&user_token_hash)
    .bind(&user.id)
    .execute(pool)
    .await?;

    // 2. Find or create the bot application, then rotate its token
    let (app, bot_user_id, bot_token) =
        find_or_create_application(pool, &user.id, "TestBot", "test bot").await?;

    let bot_user = db::users::get_user(pool, &bot_user_id).await?;

    // 3. Find or create the space
    let space = find_or_create_space(pool, &user.id, "Test Space").await?;

    // 4. Ensure #testing channel exists
    let channels = db::channels::list_channels_in_space(pool, &space.id).await?;
    if !channels.iter().any(|ch| ch.name.as_deref() == Some("testing")) {
        db::channels::create_channel(
            pool,
            &space.id,
            &CreateChannel {
                name: "testing".to_string(),
                channel_type: "text".to_string(),
                topic: Some("Test channel".to_string()),
                parent_id: None,
                nsfw: None,
                bitrate: None,
                user_limit: None,
                rate_limit: None,
                position: None,
            },
        )
        .await?;
    }

    // 5. Ensure both the user and the bot are members of the space
    for uid in [&user.id, &bot_user_id] {
        let is_member: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM members WHERE user_id = ? AND space_id = ?",
        )
        .bind(uid)
        .bind(&space.id)
        .fetch_one(pool)
        .await?;

        if is_member == 0 {
            db::members::add_member(pool, &space.id, uid).await?;
        }
    }

    // 6. Build response with all channels
    let channels = db::channels::list_channels_in_space(pool, &space.id).await?;
    let channels_json: Vec<serde_json::Value> = channels
        .iter()
        .map(|ch| {
            json!({
                "id": ch.id,
                "name": ch.name,
                "type": ch.channel_type,
                "space_id": ch.space_id,
                "topic": ch.topic,
                "position": ch.position,
            })
        })
        .collect();

    Ok(json!({
        "user": {
            "id": user.id,
            "username": user.username,
            "token": user_token,
            "token_type": "Bearer"
        },
        "bot": {
            "id": bot_user.id,
            "username": bot_user.username,
            "token": bot_token,
            "token_type": "Bot",
            "application_id": app.id
        },
        "space": {
            "id": space.id,
            "name": space.name,
            "slug": space.slug,
            "owner_id": space.owner_id
        },
        "channels": channels_json
    }))
}

// ---------------------------------------------------------------------------
// Find-or-create helpers
// ---------------------------------------------------------------------------

async fn find_or_create_user(
    pool: &SqlitePool,
    username: &str,
    display_name: &str,
) -> Result<User, AppError> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(pool)
            .await?;

    match existing {
        Some(id) => db::users::get_user(pool, &id).await,
        None => {
            db::users::create_user(
                pool,
                &CreateUser {
                    username: username.to_string(),
                    display_name: Some(display_name.to_string()),
                },
            )
            .await
        }
    }
}

/// Returns (Application, bot_user_id, fresh_bot_token).
///
/// Handles repeated calls gracefully: if the application already exists it
/// reuses it and rotates the token.  If a prior seed's application was
/// deleted (e.g. by a test) but the bot *user* still exists, the bot user
/// is reused instead of hitting a UNIQUE-constraint violation on `username`.
async fn find_or_create_application(
    pool: &SqlitePool,
    owner_id: &str,
    name: &str,
    description: &str,
) -> Result<(crate::models::application::Application, String, String), AppError> {
    // Check if an application already exists for this owner
    let existing: Option<(String, String)> = sqlx::query_as(
        "SELECT id, bot_user_id FROM applications WHERE owner_id = ?",
    )
    .bind(owner_id)
    .fetch_optional(pool)
    .await?;

    if let Some((app_id, bot_user_id)) = existing {
        let app = db::auth::get_application(pool, &app_id).await?;
        let token = db::auth::reset_bot_token(pool, &app_id).await?;
        return Ok((app, bot_user_id, token));
    }

    // No application found — the bot user may still exist from a previous
    // seed whose application row was removed.  Reuse it to avoid a UNIQUE
    // violation on users.username.
    let bot_username = format!("{name} Bot");
    let existing_bot_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
            .bind(&bot_username)
            .fetch_optional(pool)
            .await?;

    let bot_user_id = match existing_bot_id {
        Some(id) => {
            sqlx::query("UPDATE users SET bot = 1 WHERE id = ?")
                .bind(&id)
                .execute(pool)
                .await?;
            id
        }
        None => {
            let bot_user = db::users::create_user(
                pool,
                &CreateUser {
                    username: bot_username,
                    display_name: Some(format!("{name} Bot")),
                },
            )
            .await?;
            sqlx::query("UPDATE users SET bot = 1 WHERE id = ?")
                .bind(&bot_user.id)
                .execute(pool)
                .await?;
            bot_user.id
        }
    };

    // Clean up any orphaned application rows pointing to this bot user
    sqlx::query("DELETE FROM applications WHERE bot_user_id = ?")
        .bind(&bot_user_id)
        .execute(pool)
        .await?;

    // Create the application
    let app_id = snowflake::generate();
    sqlx::query(
        "INSERT INTO applications (id, name, description, owner_id, bot_user_id) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&app_id)
    .bind(name)
    .bind(description)
    .bind(owner_id)
    .bind(&bot_user_id)
    .execute(pool)
    .await?;

    let token = generate_token();
    let token_hash = create_token_hash(&token);

    sqlx::query("INSERT INTO bot_tokens (token_hash, application_id, user_id) VALUES (?, ?, ?)")
        .bind(&token_hash)
        .bind(&app_id)
        .bind(&bot_user_id)
        .execute(pool)
        .await?;

    let app = db::auth::get_application(pool, &app_id).await?;
    Ok((app, bot_user_id, token))
}

async fn find_or_create_space(
    pool: &SqlitePool,
    owner_id: &str,
    name: &str,
) -> Result<SpaceRow, AppError> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM spaces WHERE owner_id = ?")
            .bind(owner_id)
            .fetch_optional(pool)
            .await?;

    match existing {
        Some(id) => db::spaces::get_space_row(pool, &id).await,
        None => {
            db::spaces::create_space(
                pool,
                owner_id,
                &CreateSpace {
                    name: name.to_string(),
                    slug: None,
                    description: None,
                    public: None,
                },
            )
            .await
        }
    }
}
