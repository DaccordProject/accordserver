use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use sqlx::AnyPool;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::{create_token_hash, generate_token};
use crate::models::channel::CreateChannel;
use crate::models::plugin::PluginManifest;
use crate::models::space::{CreateSpace, SpaceRow};
use crate::models::user::{CreateUser, User};
use crate::snowflake;
use crate::state::AppState;

pub async fn seed(State(state): State<AppState>) -> impl IntoResponse {
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

    sqlx::query(&crate::db::q("DELETE FROM user_tokens WHERE user_id = ?"))
        .bind(&user.id)
        .execute(pool)
        .await?;

    let user_token = generate_token();
    let user_token_hash = create_token_hash(&user_token);

    sqlx::query(&crate::db::q(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, '2099-12-31T23:59:59')",
    ))
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
    if !channels
        .iter()
        .any(|ch| ch.name.as_deref() == Some("testing"))
    {
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
                allow_anonymous_read: None,
                position: None,
            },
        )
        .await?;
    }

    // 5. Ensure a test plugin exists in the space
    let plugin = find_or_create_test_plugin(pool, &space.id, &user.id).await?;

    // 6. Ensure both the user and the bot are members of the space
    for uid in [&user.id, &bot_user_id] {
        let is_member: i64 = sqlx::query_scalar(&crate::db::q(
            "SELECT COUNT(*) FROM members WHERE user_id = ? AND space_id = ?",
        ))
        .bind(uid)
        .bind(&space.id)
        .fetch_one(pool)
        .await?;

        if is_member == 0 {
            db::members::add_member(pool, &space.id, uid, state.db_is_postgres).await?;
        }
    }

    // 7. Build response with all channels
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
        "channels": channels_json,
        "plugin": {
            "id": plugin.id,
            "name": plugin.name,
            "type": plugin.plugin_type,
            "runtime": plugin.runtime,
            "version": plugin.version,
            "space_id": plugin.space_id,
        }
    }))
}

// ---------------------------------------------------------------------------
// Find-or-create helpers
// ---------------------------------------------------------------------------

async fn find_or_create_user(
    pool: &AnyPool,
    username: &str,
    display_name: &str,
) -> Result<User, AppError> {
    let existing: Option<String> =
        sqlx::query_scalar(&crate::db::q("SELECT id FROM users WHERE username = ?"))
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
    pool: &AnyPool,
    owner_id: &str,
    name: &str,
    description: &str,
) -> Result<(crate::models::application::Application, String, String), AppError> {
    // Check if an application already exists for this owner
    let existing: Option<(String, String)> = sqlx::query_as(&crate::db::q(
        "SELECT id, bot_user_id FROM applications WHERE owner_id = ?",
    ))
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
        sqlx::query_scalar(&crate::db::q("SELECT id FROM users WHERE username = ?"))
            .bind(&bot_username)
            .fetch_optional(pool)
            .await?;

    let bot_user_id = match existing_bot_id {
        Some(id) => {
            sqlx::query(&crate::db::q("UPDATE users SET bot = TRUE WHERE id = ?"))
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
            sqlx::query(&crate::db::q("UPDATE users SET bot = TRUE WHERE id = ?"))
                .bind(&bot_user.id)
                .execute(pool)
                .await?;
            bot_user.id
        }
    };

    // Clean up any orphaned application rows pointing to this bot user
    sqlx::query(&crate::db::q(
        "DELETE FROM applications WHERE bot_user_id = ?",
    ))
    .bind(&bot_user_id)
    .execute(pool)
    .await?;

    // Create the application
    let app_id = snowflake::generate();
    sqlx::query(&crate::db::q(
        "INSERT INTO applications (id, name, description, owner_id, bot_user_id) VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(&app_id)
    .bind(name)
    .bind(description)
    .bind(owner_id)
    .bind(&bot_user_id)
    .execute(pool)
    .await?;

    let token = generate_token();
    let token_hash = create_token_hash(&token);

    sqlx::query(&crate::db::q(
        "INSERT INTO bot_tokens (token_hash, application_id, user_id) VALUES (?, ?, ?)",
    ))
    .bind(&token_hash)
    .bind(&app_id)
    .bind(&bot_user_id)
    .execute(pool)
    .await?;

    let app = db::auth::get_application(pool, &app_id).await?;
    Ok((app, bot_user_id, token))
}

async fn find_or_create_test_plugin(
    pool: &AnyPool,
    space_id: &str,
    creator_id: &str,
) -> Result<crate::models::plugin::Plugin, AppError> {
    // Check if a plugin named "Test Plugin" already exists in this space
    let existing: Option<String> = sqlx::query_scalar(&crate::db::q(
        "SELECT id FROM plugins WHERE space_id = ? AND name = 'Test Plugin'",
    ))
    .bind(space_id)
    .fetch_optional(pool)
    .await?;

    if let Some(id) = existing {
        return db::plugins::get_plugin(pool, &id).await;
    }

    // Minimal ELF stub (just enough bytes so the blob is non-empty)
    let elf_stub: &[u8] = &[0x7f, b'E', b'L', b'F'];

    let manifest = PluginManifest {
        name: "Test Plugin".to_string(),
        description: "A scripted test plugin for integration tests".to_string(),
        plugin_type: "activity".to_string(),
        runtime: "scripted".to_string(),
        version: "1.0.0".to_string(),
        max_participants: 8,
        max_spectators: -1,
        lobby: true,
        canvas_size: Some([480, 360]),
        ..Default::default()
    };

    db::plugins::create_plugin(pool, space_id, creator_id, &manifest, Some(elf_stub), None).await
}

async fn find_or_create_space(
    pool: &AnyPool,
    owner_id: &str,
    name: &str,
) -> Result<SpaceRow, AppError> {
    let existing: Option<String> =
        sqlx::query_scalar(&crate::db::q("SELECT id FROM spaces WHERE owner_id = ?"))
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
                    allow_guest_access: None,
                },
            )
            .await
        }
    }
}
