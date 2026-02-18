use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_channel_membership, require_channel_permission};
use crate::models::channel::UpdateChannel;
use crate::models::permission::{PermissionOverwrite, ALL_PERMISSIONS};
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct UpsertOverwriteRequest {
    #[serde(rename = "type")]
    pub overwrite_type: String,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

pub async fn get_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let json = super::spaces::channel_row_to_json_pub(&state.db, &channel).await;
    Ok(Json(serde_json::json!({ "data": json })))
}

pub async fn update_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<UpdateChannel>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_channels").await?;
    let channel = db::channels::update_channel(&state.db, &channel_id, &input).await?;
    let json = super::spaces::channel_row_to_json_pub(&state.db, &channel).await;
    Ok(Json(serde_json::json!({ "data": json })))
}

pub async fn delete_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_channels").await?;
    db::channels::delete_channel(&state.db, &channel_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn list_overwrites(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_roles").await?;
    let overwrites = db::permission_overwrites::list_overwrites(&state.db, &channel_id).await?;
    Ok(Json(serde_json::json!({ "data": overwrites })))
}

pub async fn upsert_overwrite(
    state: State<AppState>,
    Path((channel_id, overwrite_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<UpsertOverwriteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_roles").await?;

    // Validate overwrite_type
    if input.overwrite_type != "role" && input.overwrite_type != "member" {
        return Err(AppError::BadRequest(
            "type must be 'role' or 'member'".into(),
        ));
    }

    // Validate permission strings
    for perm in input.allow.iter().chain(input.deny.iter()) {
        if !ALL_PERMISSIONS.contains(&perm.as_str()) {
            return Err(AppError::BadRequest(format!("unknown permission: {perm}")));
        }
    }

    let overwrite = PermissionOverwrite {
        id: overwrite_id,
        overwrite_type: input.overwrite_type,
        allow: input.allow,
        deny: input.deny,
    };
    db::permission_overwrites::upsert_overwrite(&state.db, &channel_id, &overwrite).await?;

    Ok(Json(serde_json::json!({ "data": overwrite })))
}

pub async fn delete_overwrite(
    state: State<AppState>,
    Path((channel_id, overwrite_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_roles").await?;
    db::permission_overwrites::delete_overwrite(&state.db, &channel_id, &overwrite_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}
