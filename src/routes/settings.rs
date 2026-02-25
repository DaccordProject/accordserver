use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_server_admin;
use crate::models::settings::UpdateServerSettings;
use crate::state::AppState;

pub async fn get_settings(
    state: State<AppState>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let settings = state.settings.load();
    Ok(Json(serde_json::json!({ "data": *settings })))
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
