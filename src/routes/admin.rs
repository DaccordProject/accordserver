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

    let user =
        db::admin::admin_update_user(&state.db, &user_id, &input, state.db_is_postgres).await?;
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
    sqlx::query(&crate::db::q(
        "UPDATE users SET password_hash = ?, force_password_reset = TRUE WHERE id = ?",
    ))
    .bind(&password_hash)
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Revoke all existing sessions so the user must log in with the new password
    sqlx::query(&crate::db::q("DELETE FROM user_tokens WHERE user_id = ?"))
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    // Disable 2FA so the reset password can actually be used to log in
    sqlx::query(&crate::db::q(
        "UPDATE users SET totp_secret = NULL, totp_enabled = FALSE WHERE id = ?",
    ))
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Clean up backup codes
    sqlx::query(&crate::db::q("DELETE FROM backup_codes WHERE user_id = ?"))
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

// =========================================================================
// Federation peers
// =========================================================================

#[derive(Deserialize)]
pub struct AddPeerInput {
    /// Domain of the peer to add (e.g. "b.example"). Its `.well-known`
    /// document is fetched to discover the public key and inbox URL.
    pub domain: String,
    /// When true, mark the peer `trusted` immediately so content can be
    /// exchanged. Otherwise it starts `pending` (key pinned only).
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Deserialize)]
pub struct UpdatePeerInput {
    /// Set/clear the trusted flag.
    pub trusted: Option<bool>,
    /// Re-fetch the peer's `.well-known` to refresh its key and inbox URL.
    #[serde(default)]
    pub refresh: bool,
}

fn peer_json(peer: &db::federation::Peer) -> serde_json::Value {
    serde_json::json!({
        "domain": peer.domain,
        "inbox_url": peer.inbox_url,
        "trust_state": peer.trust_state,
        "public_key": peer.public_key,
    })
}

/// A reqwest client for fetching peer metadata (reuses the federation client
/// when available).
fn fed_client(state: &AppState) -> reqwest::Client {
    state
        .federation
        .as_ref()
        .map(|f| f.client.clone())
        .unwrap_or_default()
}

pub async fn list_federation_peers(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    let peers = db::federation::list_peers(&state.db).await?;
    let data: Vec<_> = peers.iter().map(peer_json).collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

pub async fn add_federation_peer(
    state: State<AppState>,
    auth: AuthUser,
    Json(input): Json<AddPeerInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    let domain = input.domain.trim().to_ascii_lowercase();
    if domain.is_empty() {
        return Err(AppError::BadRequest("domain is required".to_string()));
    }

    // Discover the peer's public key and inbox by fetching its well-known.
    let client = fed_client(&state);
    let wk = crate::federation::peers::fetch_well_known(&client, &domain).await?;

    let trust_state = if input.trusted { "trusted" } else { "pending" };
    db::federation::upsert_peer(
        &state.db,
        &domain,
        &wk.public_key,
        &wk.inbox_url,
        trust_state,
    )
    .await?;
    // upsert_peer preserves an existing peer's trust; apply an explicit change.
    if input.trusted {
        db::federation::set_peer_trust(&state.db, &domain, "trusted").await?;
    }

    let peer = db::federation::get_peer(&state.db, &domain)
        .await?
        .ok_or_else(|| AppError::Internal("peer vanished after upsert".to_string()))?;
    Ok(Json(serde_json::json!({ "data": peer_json(&peer) })))
}

pub async fn update_federation_peer(
    state: State<AppState>,
    Path(domain): Path<String>,
    auth: AuthUser,
    Json(input): Json<UpdatePeerInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    let domain = domain.to_ascii_lowercase();

    db::federation::get_peer(&state.db, &domain)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_peer".to_string()))?;

    if input.refresh {
        let client = fed_client(&state);
        let wk = crate::federation::peers::fetch_well_known(&client, &domain).await?;
        // Preserve trust; only the key/inbox are refreshed.
        let existing = db::federation::get_peer(&state.db, &domain).await?;
        let trust = existing
            .as_ref()
            .map(|p| p.trust_state.clone())
            .unwrap_or_else(|| "pending".to_string());
        db::federation::upsert_peer(&state.db, &domain, &wk.public_key, &wk.inbox_url, &trust)
            .await?;
    }

    if let Some(trusted) = input.trusted {
        let state_str = if trusted { "trusted" } else { "pending" };
        db::federation::set_peer_trust(&state.db, &domain, state_str).await?;
    }

    let peer = db::federation::get_peer(&state.db, &domain)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_peer".to_string()))?;
    Ok(Json(serde_json::json!({ "data": peer_json(&peer) })))
}

pub async fn delete_federation_peer(
    state: State<AppState>,
    Path(domain): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    let domain = domain.to_ascii_lowercase();
    db::federation::delete_peer(&state.db, &domain).await?;
    Ok(Json(serde_json::json!({ "data": { "deleted": true } })))
}
