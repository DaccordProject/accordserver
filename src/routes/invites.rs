use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{
    require_channel_permission, require_membership, require_permission,
};
use crate::models::invite::CreateInvite;
use crate::state::AppState;

pub async fn get_invite(
    state: State<AppState>,
    Path(code): Path<String>,
    _auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    // get_invite is accessible to any authenticated user (they need the code to look it up)
    let invite = db::invites::get_invite(&state.db, &code).await?;
    Ok(Json(serde_json::json!({ "data": invite })))
}

pub async fn delete_invite(
    state: State<AppState>,
    Path(code): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let invite = db::invites::get_invite(&state.db, &code).await?;
    require_permission(
        &state.db,
        &invite.space_id,
        &auth.user_id,
        "manage_channels",
    )
    .await?;
    db::invites::delete_invite(&state.db, &code).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn accept_invite(
    state: State<AppState>,
    Path(code): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let invite = db::invites::use_invite(&state.db, &code).await?;

    // Check if the user is banned from this space
    if db::bans::get_ban(&state.db, &invite.space_id, &auth.user_id)
        .await
        .is_ok()
    {
        return Err(AppError::Forbidden(
            "you are banned from this space".to_string(),
        ));
    }

    let _member = db::members::add_member(&state.db, &invite.space_id, &auth.user_id).await?;
    Ok(Json(serde_json::json!({ "data": invite })))
}

pub async fn list_space_invites(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let invites = db::invites::list_space_invites(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": invites })))
}

pub async fn list_channel_invites(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "view_channel").await?;
    let invites = db::invites::list_channel_invites(&state.db, &channel_id).await?;
    Ok(Json(serde_json::json!({ "data": invites })))
}

pub async fn create_channel_invite(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateInvite>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_id =
        require_channel_permission(&state.db, &channel_id, &auth.user_id, "create_invites").await?;
    let invite = db::invites::create_invite(
        &state.db,
        &space_id,
        Some(channel_id.as_str()),
        &auth.user_id,
        &input,
    )
    .await?;
    Ok(Json(serde_json::json!({ "data": invite })))
}

pub async fn create_space_invite(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateInvite>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth.user_id, "create_invites").await?;
    let _space = db::spaces::get_space_row(&state.db, &space_id).await?;
    let invite =
        db::invites::create_invite(&state.db, &space_id, None, &auth.user_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": invite })))
}
