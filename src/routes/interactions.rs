use axum::extract::{Path, State};
use axum::Json;

use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::state::AppState;

pub async fn list_global_commands(
    _state: State<AppState>,
    Path(_app_id): Path<String>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(serde_json::json!({ "data": [] })))
}

pub async fn create_global_command(
    _state: State<AppState>,
    Path(_app_id): Path<String>,
    _auth: AuthUser,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(serde_json::json!({ "data": body })))
}

pub async fn interaction_callback(
    _state: State<AppState>,
    Path((_interaction_id, _token)): Path<(String, String)>,
    Json(_body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(serde_json::json!({ "data": null })))
}
