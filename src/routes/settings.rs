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

    let old_public_listing = state.settings.load().public_listing;

    let updated = db::settings::update_settings(&state.db, &input, state.db_is_postgres).await?;
    state.settings.store(Arc::new(updated.clone()));

    // Handle public_listing toggle → start/stop master registration task
    if let Some(new_listing) = input.public_listing {
        if new_listing != old_public_listing {
            let mut task_guard = state.master_task.lock().await;
            if new_listing {
                // Turned on
                if let Some(ref mc) = state.master_config {
                    if task_guard.is_none() {
                        let handle = tokio::spawn(crate::master::run(mc.clone()));
                        *task_guard = Some(handle);
                        tracing::info!("public_listing enabled; started master registration");
                    }
                } else {
                    tracing::warn!(
                        "public_listing enabled but MASTER_SERVER_PUBLIC_URL is not configured"
                    );
                }
            } else {
                // Turned off
                if let Some(handle) = task_guard.take() {
                    handle.abort();
                    tracing::info!("public_listing disabled; stopped master registration");
                }
                // Deregister from master
                if let Some(ref mc) = state.master_config {
                    crate::master::deregister_from(mc).await;
                }
            }
        }
    }

    Ok(Json(serde_json::json!({ "data": updated })))
}
