use axum::extract::State;
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::models::application::CreateApplication;
use crate::state::AppState;

pub async fn create_application(
    state: State<AppState>,
    auth: AuthUser,
    Json(input): Json<CreateApplication>,
) -> Result<Json<serde_json::Value>, AppError> {
    let description = input.description.as_deref().unwrap_or("");
    let (app, token) =
        db::auth::create_application(&state.db, &auth.user_id, &input.name, description).await?;
    Ok(Json(serde_json::json!({
        "data": {
            "application": app,
            "token": token
        }
    })))
}

pub async fn get_current_application(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let app = db::auth::get_application_by_owner(&state.db, &auth.user_id).await?;
    Ok(Json(serde_json::json!({ "data": app })))
}

pub async fn update_current_application(
    state: State<AppState>,
    auth: AuthUser,
    Json(_body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let app = db::auth::get_application_by_owner(&state.db, &auth.user_id).await?;
    Ok(Json(serde_json::json!({ "data": app })))
}

pub async fn reset_token(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let app = db::auth::get_application_by_owner(&state.db, &auth.user_id).await?;
    let token = db::auth::reset_bot_token(&state.db, &app.id).await?;
    Ok(Json(serde_json::json!({ "data": { "token": token } })))
}
