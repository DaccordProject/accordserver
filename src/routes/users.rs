use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::models::user::UpdateUser;
use crate::state::AppState;

pub async fn get_current_user(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = db::users::get_user(&state.db, &auth.user_id).await?;
    Ok(Json(serde_json::json!({ "data": user })))
}

pub async fn update_current_user(
    state: State<AppState>,
    auth: AuthUser,
    Json(input): Json<UpdateUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = db::users::update_user(&state.db, &auth.user_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": user })))
}

pub async fn get_user(
    state: State<AppState>,
    Path(user_id): Path<String>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = db::users::get_user(&state.db, &user_id).await?;
    Ok(Json(serde_json::json!({ "data": user })))
}

pub async fn get_current_user_channels(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let channels = db::users::get_user_dm_channels(&state.db, &auth.user_id).await?;
    let mut json_channels = Vec::new();
    for ch in &channels {
        json_channels.push(super::spaces::channel_row_to_json_pub(&state.db, ch).await);
    }
    Ok(Json(serde_json::json!({ "data": json_channels })))
}

pub async fn get_current_user_spaces(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_ids = db::users::get_user_spaces(&state.db, &auth.user_id).await?;
    let mut spaces = Vec::new();
    for id in space_ids {
        if let Ok(space) = db::spaces::get_space_row(&state.db, &id).await {
            spaces.push(space);
        }
    }
    Ok(Json(serde_json::json!({ "data": spaces })))
}
