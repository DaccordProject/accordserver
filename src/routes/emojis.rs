use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_membership, require_permission};
use crate::models::emoji::{CreateEmoji, UpdateEmoji};
use crate::state::AppState;

pub async fn list_emojis(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let emojis = db::emojis::list_emojis(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": emojis })))
}

pub async fn get_emoji(
    state: State<AppState>,
    Path((space_id, emoji_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let emoji = db::emojis::get_emoji(&state.db, &emoji_id).await?;
    Ok(Json(serde_json::json!({ "data": emoji })))
}

pub async fn create_emoji(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateEmoji>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth.user_id, "manage_emojis").await?;
    let emoji = db::emojis::create_emoji(&state.db, &space_id, &auth.user_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": emoji })))
}

pub async fn update_emoji(
    state: State<AppState>,
    Path((space_id, emoji_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<UpdateEmoji>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth.user_id, "manage_emojis").await?;
    let emoji = db::emojis::update_emoji(&state.db, &emoji_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": emoji })))
}

pub async fn delete_emoji(
    state: State<AppState>,
    Path((space_id, emoji_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth.user_id, "manage_emojis").await?;
    db::emojis::delete_emoji(&state.db, &emoji_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}
