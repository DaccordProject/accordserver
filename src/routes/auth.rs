use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::{create_token_hash, generate_token, AuthUser};
use crate::snowflake;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

pub async fn register(
    State(state): State<AppState>,
    Json(input): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Validate username length
    let username = input.username.trim();
    if username.is_empty() || username.len() > 32 {
        return Err(AppError::BadRequest(
            "username must be between 1 and 32 characters".to_string(),
        ));
    }

    // Validate password length
    if input.password.len() < 8 || input.password.len() > 128 {
        return Err(AppError::BadRequest(
            "password must be between 8 and 128 characters".to_string(),
        ));
    }

    // Check for username conflict
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT id FROM users WHERE username = ? AND bot = false",
    )
    .bind(username)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;

    if existing.is_some() {
        return Err(AppError::Conflict("username already taken".to_string()));
    }

    // Hash password with Argon2id
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(input.password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("password hashing failed: {e}")))?
        .to_string();

    // Create user
    let id = snowflake::generate();
    let display_name = input
        .display_name
        .as_deref()
        .unwrap_or(username);

    // First registered (non-bot, non-system) user becomes admin
    let user_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE bot = false AND system = false")
            .fetch_one(&state.db)
            .await
            .map_err(AppError::from)?;
    let is_admin = user_count == 0;

    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, is_admin) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(display_name)
    .bind(&password_hash)
    .bind(is_admin)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    let user = db::users::get_user(&state.db, &id).await?;

    // Auto-join the default space (first space created on the server)
    let default_space: Option<(String,)> =
        sqlx::query_as("SELECT id FROM spaces ORDER BY created_at ASC LIMIT 1")
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::from)?;
    if let Some((space_id,)) = default_space {
        // Add as member (ignore if already a member somehow)
        let _ = sqlx::query("INSERT OR IGNORE INTO members (user_id, space_id) VALUES (?, ?)")
            .bind(&id)
            .bind(&space_id)
            .execute(&state.db)
            .await;
    }

    // Generate bearer token with 30-day expiry
    let token = generate_token();
    let token_hash = create_token_hash(&token);
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(30))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();

    sqlx::query("INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(&token_hash)
        .bind(&id)
        .bind(&expires_at)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": {
            "user": user,
            "token": token
        }
    })))
}

pub async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Look up user by username (must not be a bot, must have password_hash)
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT id, password_hash FROM users WHERE username = ? AND bot = false AND password_hash IS NOT NULL",
    )
    .bind(&input.username)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;

    let (user_id, stored_hash) = match row {
        Some(r) => r,
        None => {
            return Err(AppError::Unauthorized(
                "invalid credentials".to_string(),
            ));
        }
    };

    // Verify password
    let parsed_hash = PasswordHash::new(&stored_hash)
        .map_err(|e| AppError::Internal(format!("stored hash parse failed: {e}")))?;

    if Argon2::default()
        .verify_password(input.password.as_bytes(), &parsed_hash)
        .is_err()
    {
        return Err(AppError::Unauthorized(
            "invalid credentials".to_string(),
        ));
    }

    let user = db::users::get_user(&state.db, &user_id).await?;

    // Generate new bearer token with 30-day expiry
    let token = generate_token();
    let token_hash = create_token_hash(&token);
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(30))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();

    sqlx::query("INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(&token_hash)
        .bind(&user_id)
        .bind(&expires_at)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": {
            "user": user,
            "token": token
        }
    })))
}

pub async fn logout(
    State(state): State<AppState>,
    _auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    // Extract the raw token from the Authorization header and hash it
    // to delete the specific token row (single-token revocation).
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let raw_token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    let token_hash = create_token_hash(raw_token);

    sqlx::query("DELETE FROM user_tokens WHERE token_hash = ?")
        .bind(&token_hash)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": { "ok": true }
    })))
}
