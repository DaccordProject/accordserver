use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_server_admin;
use crate::models::settings::UpdateServerSettings;
use crate::state::AppState;

/// Admin-only: returns all server settings.
pub async fn get_settings(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    let settings = state.settings.load();
    Ok(Json(serde_json::json!({ "data": *settings })))
}

/// Public: returns client-facing settings (upload limits, server name, etc.).
pub async fn get_public_settings(
    state: State<AppState>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let settings = state.settings.load();
    Ok(Json(serde_json::json!({
        "data": {
            "max_emoji_size": settings.max_emoji_size,
            "max_avatar_size": settings.max_avatar_size,
            "max_sound_size": settings.max_sound_size,
            "max_attachment_size": settings.max_attachment_size,
            "max_attachments_per_message": settings.max_attachments_per_message,
            "server_name": settings.server_name,
            "registration_policy": settings.registration_policy,
            "motd": settings.motd,
        }
    })))
}

pub async fn update_settings(
    state: State<AppState>,
    auth: AuthUser,
    Json(input): Json<UpdateServerSettings>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    let updated = db::settings::update_settings(&state.db, &input).await?;
    state.settings.store(Arc::new(updated.clone()));

    Ok(Json(serde_json::json!({ "data": updated })))
}
