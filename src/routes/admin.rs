use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_server_admin;
use crate::models::space::AdminUpdateSpace;
use crate::models::user::AdminUpdateUser;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AdminListQuery {
    pub after: Option<String>,
    pub limit: Option<i64>,
    pub search: Option<String>,
}

// =========================================================================
// Spaces
// =========================================================================

pub async fn list_spaces(
    state: State<AppState>,
    auth: AuthUser,
    Query(params): Query<AdminListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    let limit = params.limit.unwrap_or(50).min(1000);
    let mut rows = db::admin::list_all_spaces(
        &state.db,
        params.after.as_deref(),
        limit,
        params.search.as_deref(),
    )
    .await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let last_id = rows.last().map(|s| s.id.clone());
    let mut response = serde_json::json!({ "data": rows });
    if has_more {
        response["cursor"] = serde_json::json!({
            "after": last_id.unwrap_or_default(),
            "has_more": has_more
        });
    }
    Ok(Json(response))
}

pub async fn update_space(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<AdminUpdateSpace>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Verify the space exists
    db::spaces::get_space_row(&state.db, &space_id).await?;

    // If transferring ownership, verify the target user exists
    if let Some(ref owner_id) = input.owner_id {
        db::users::get_user(&state.db, owner_id).await?;
    }

    db::admin::admin_update_space(&state.db, &space_id, &input, state.db_is_postgres).await?;

    let space = db::spaces::get_space_row(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": space })))
}

// =========================================================================
// Users
// =========================================================================

pub async fn list_users(
    state: State<AppState>,
    auth: AuthUser,
    Query(params): Query<AdminListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    let limit = params.limit.unwrap_or(50).min(1000);
    let mut rows = db::admin::list_all_users(
        &state.db,
        params.after.as_deref(),
        limit,
        params.search.as_deref(),
    )
    .await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let last_id = rows
        .last()
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut response = serde_json::json!({ "data": rows });
    if has_more {
        response["cursor"] = serde_json::json!({
            "after": last_id.unwrap_or_default(),
            "has_more": has_more
        });
    }
    Ok(Json(response))
}

pub async fn update_user(
    state: State<AppState>,
    Path(user_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<AdminUpdateUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Verify target user exists
    let target = db::users::get_user(&state.db, &user_id).await?;

    // Self-demotion protection: can't remove your own admin flag
    if auth.user_id == user_id {
        if let Some(false) = input.is_admin {
            return Err(AppError::BadRequest(
                "cannot remove your own admin privileges".to_string(),
            ));
        }
    }

    // Last-admin protection: if removing admin from someone, ensure at least one admin remains
    if let Some(false) = input.is_admin {
        if target.is_admin {
            let admin_count = db::admin::count_admins(&state.db).await?;
            if admin_count <= 1 {
                return Err(AppError::BadRequest(
                    "cannot remove the last server admin".to_string(),
                ));
            }
        }
    }

    let user = db::admin::admin_update_user(&state.db, &user_id, &input, state.db_is_postgres).await?;
    Ok(Json(serde_json::json!({ "data": user })))
}

pub async fn delete_user(
    state: State<AppState>,
    Path(user_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Can't delete yourself
    if auth.user_id == user_id {
        return Err(AppError::BadRequest(
            "cannot delete your own account via admin API".to_string(),
        ));
    }

    // Can't delete another admin (must remove flag first)
    let target = db::users::get_user(&state.db, &user_id).await?;
    if target.is_admin {
        return Err(AppError::BadRequest(
            "cannot delete an admin user — remove admin flag first".to_string(),
        ));
    }

    db::admin::delete_user(&state.db, &user_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

// =========================================================================
// Password Reset
// =========================================================================

#[derive(Deserialize)]
pub struct AdminResetPasswordRequest {
    pub new_password: String,
}

pub async fn reset_user_password(
    state: State<AppState>,
    Path(user_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<AdminResetPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Validate password length
    if input.new_password.len() < 8 || input.new_password.len() > 128 {
        return Err(AppError::BadRequest(
            "password must be between 8 and 128 characters".to_string(),
        ));
    }

    // Verify target user exists
    let target = db::users::get_user(&state.db, &user_id).await?;

    // Don't allow resetting bot user passwords
    if target.bot {
        return Err(AppError::BadRequest(
            "cannot reset password for a bot user".to_string(),
        ));
    }

    // Hash the new password with Argon2id (same params as registration)
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(19456, 3, 1, None)
            .map_err(|e| AppError::Internal(format!("argon2 params failed: {e}")))?,
    );
    let password_hash = argon2
        .hash_password(input.new_password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("password hashing failed: {e}")))?
        .to_string();

    // Update password and set force_password_reset flag
    sqlx::query(
        "UPDATE users SET password_hash = ?, force_password_reset = TRUE WHERE id = ?",
    )
    .bind(&password_hash)
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Revoke all existing sessions so the user must log in with the new password
    sqlx::query("DELETE FROM user_tokens WHERE user_id = ?")
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    // Disable 2FA so the reset password can actually be used to log in
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_enabled = FALSE WHERE id = ?")
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    // Clean up backup codes
    sqlx::query("DELETE FROM backup_codes WHERE user_id = ?")
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": {
            "message": "password has been reset",
            "force_password_reset": true
        }
    })))
}
