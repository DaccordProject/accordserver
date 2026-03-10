use sqlx::AnyPool;

use crate::error::AppError;
use crate::middleware::auth::{create_token_hash, generate_token};
use crate::models::application::Application;
use crate::models::user::CreateUser;
use crate::snowflake;

pub async fn create_application(
    pool: &AnyPool,
    owner_id: &str,
    name: &str,
    description: &str,
) -> Result<(Application, String), AppError> {
    let app_id = snowflake::generate();

    // Create a bot user for the application
    let bot_user = crate::db::users::create_user(
        pool,
        &CreateUser {
            username: format!("{name} Bot"),
            display_name: Some(format!("{name} Bot")),
        },
    )
    .await?;

    // Mark as bot
    sqlx::query("UPDATE users SET bot = TRUE WHERE id = ?")
        .bind(&bot_user.id)
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT INTO applications (id, name, description, owner_id, bot_user_id) VALUES (?, ?, ?, ?, ?)"
    )
    .bind(&app_id)
    .bind(name)
    .bind(description)
    .bind(owner_id)
    .bind(&bot_user.id)
    .execute(pool)
    .await?;

    // Generate a bot token
    let token = generate_token();
    let token_hash = create_token_hash(&token);

    sqlx::query("INSERT INTO bot_tokens (token_hash, application_id, user_id) VALUES (?, ?, ?)")
        .bind(&token_hash)
        .bind(&app_id)
        .bind(&bot_user.id)
        .execute(pool)
        .await?;

    let app = get_application(pool, &app_id).await?;
    Ok((app, token))
}

fn row_to_application(row: sqlx::any::AnyRow) -> Application {
    use sqlx::Row;
    Application {
        id: row.get("id"),
        name: row.get("name"),
        icon: row.get("icon"),
        description: row.get("description"),
        bot_public: crate::db::get_bool(&row, "bot_public"),
        owner_id: row.get("owner_id"),
        flags: row.get("flags"),
    }
}

const SELECT_APPLICATIONS: &str = "SELECT id, name, icon, description, bot_public, owner_id, flags FROM applications";

pub async fn get_application(pool: &AnyPool, app_id: &str) -> Result<Application, AppError> {
    let row = sqlx::query(&format!("{SELECT_APPLICATIONS} WHERE id = ?"))
        .bind(app_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("application not found".to_string()))?;

    Ok(row_to_application(row))
}

pub async fn get_application_by_owner(
    pool: &AnyPool,
    owner_id: &str,
) -> Result<Application, AppError> {
    let row = sqlx::query(&format!("{SELECT_APPLICATIONS} WHERE owner_id = ? LIMIT 1"))
        .bind(owner_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("application not found".to_string()))?;

    Ok(row_to_application(row))
}

pub async fn reset_bot_token(pool: &AnyPool, app_id: &str) -> Result<String, AppError> {
    // Find the bot user for this application
    let bot_user_id: String =
        sqlx::query_scalar("SELECT bot_user_id FROM applications WHERE id = ?")
            .bind(app_id)
            .fetch_one(pool)
            .await?;

    // Delete old tokens
    sqlx::query("DELETE FROM bot_tokens WHERE application_id = ?")
        .bind(app_id)
        .execute(pool)
        .await?;

    // Generate new token
    let token = generate_token();
    let token_hash = create_token_hash(&token);

    sqlx::query("INSERT INTO bot_tokens (token_hash, application_id, user_id) VALUES (?, ?, ?)")
        .bind(&token_hash)
        .bind(app_id)
        .bind(&bot_user_id)
        .execute(pool)
        .await?;

    Ok(token)
}
