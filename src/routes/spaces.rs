use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::{AuthUser, OptionalAuthUser};
use crate::middleware::permissions::{require_membership, require_permission};
use crate::models::channel::{ChannelPositionUpdate, ChannelRow, CreateChannel};
use crate::models::permission::PermissionOverwrite;
use crate::models::space::{CreateSpace, UpdateSpace};
use crate::state::AppState;

pub async fn create_space(
    state: State<AppState>,
    auth: AuthUser,
    Json(input): Json<CreateSpace>,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = db::spaces::create_space(&state.db, &auth.user_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": space })))
}

pub async fn get_space(
    state: State<AppState>,
    Path(id_or_slug): Path<String>,
    auth: OptionalAuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    // Try ID lookup first, fall back to slug lookup
    let space = match db::spaces::get_space_row(&state.db, &id_or_slug).await {
        Ok(s) => s,
        Err(AppError::NotFound(_)) => {
            db::spaces::get_space_by_slug(&state.db, &id_or_slug).await?
        }
        Err(e) => return Err(e),
    };
    if !space.public {
        let user = auth
            .0
            .ok_or_else(|| AppError::Unauthorized("authentication required".into()))?;
        require_membership(&state.db, &space.id, &user.user_id).await?;
    }
    Ok(Json(serde_json::json!({ "data": space })))
}

pub async fn update_space(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<UpdateSpace>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_space").await?;
    let space = db::spaces::update_space(&state.db, &space_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": space })))
}

pub async fn delete_space(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = db::spaces::get_space_row(&state.db, &space_id).await?;
    if space.owner_id != auth.user_id && !auth.is_admin {
        return Err(AppError::Forbidden("you do not own this space".to_string()));
    }
    db::spaces::delete_space(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn list_channels(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: OptionalAuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = db::spaces::get_space_row(&state.db, &space_id).await?;
    if !space.public {
        let user = auth
            .0
            .ok_or_else(|| AppError::Unauthorized("authentication required".into()))?;
        require_membership(&state.db, &space_id, &user.user_id).await?;
    }
    let channels = db::channels::list_channels_in_space(&state.db, &space_id).await?;
    let data = channels_to_json_async(&state.db, &channels).await?;
    Ok(Json(serde_json::json!({ "data": data })))
}

pub async fn create_channel(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateChannel>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_channels").await?;
    let channel = db::channels::create_channel(&state.db, &space_id, &input).await?;
    // Newly created channel has no overwrites
    Ok(Json(
        serde_json::json!({ "data": channel_row_to_json_with_overwrites(&channel, &[]) }),
    ))
}

pub async fn reorder_channels(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<Vec<ChannelPositionUpdate>>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_channels").await?;
    let updates: Vec<(String, i64)> = input.into_iter().map(|u| (u.id, u.position)).collect();
    db::channels::reorder_channels(&state.db, &space_id, &updates).await?;
    let channels = db::channels::list_channels_in_space(&state.db, &space_id).await?;
    let data = channels_to_json_async(&state.db, &channels).await?;
    Ok(Json(serde_json::json!({ "data": data })))
}

/// Build channel JSON for external callers (e.g. channels.rs).
/// Loads overwrites from the DB.
pub async fn channel_row_to_json_pub(
    pool: &sqlx::SqlitePool,
    row: &ChannelRow,
) -> serde_json::Value {
    let overwrites = db::permission_overwrites::list_overwrites(pool, &row.id)
        .await
        .unwrap_or_default();
    channel_row_to_json_with_overwrites(row, &overwrites)
}

fn channel_row_to_json_with_overwrites(
    row: &ChannelRow,
    overwrites: &[PermissionOverwrite],
) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "type": row.channel_type,
        "space_id": row.space_id,
        "name": row.name,
        "topic": row.topic,
        "position": row.position,
        "parent_id": row.parent_id,
        "nsfw": row.nsfw,
        "rate_limit": row.rate_limit,
        "bitrate": row.bitrate,
        "user_limit": row.user_limit,
        "owner_id": row.owner_id,
        "last_message_id": row.last_message_id,
        "permission_overwrites": overwrites,
        "archived": row.archived,
        "auto_archive_after": row.auto_archive_after,
        "created_at": row.created_at
    })
}

async fn channels_to_json_async(
    pool: &sqlx::SqlitePool,
    rows: &[ChannelRow],
) -> Result<Vec<serde_json::Value>, AppError> {
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let overwrites = db::permission_overwrites::list_overwrites(pool, &row.id).await?;
        result.push(channel_row_to_json_with_overwrites(row, &overwrites));
    }
    Ok(result)
}

pub async fn list_public_spaces(
    state: State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let spaces = db::spaces::list_public_spaces(&state.db).await?;
    Ok(Json(serde_json::json!({ "data": spaces })))
}

pub async fn join_public_space(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space = db::spaces::get_space_row(&state.db, &space_id).await?;
    if !space.public {
        return Err(AppError::Forbidden("this space is not public".to_string()));
    }

    // Check if the user is banned
    if db::bans::get_ban(&state.db, &space_id, &auth.user_id)
        .await
        .is_ok()
    {
        return Err(AppError::Forbidden(
            "you are banned from this space".to_string(),
        ));
    }

    let _member = db::members::add_member(&state.db, &space_id, &auth.user_id).await?;
    Ok(Json(
        serde_json::json!({ "data": { "space_id": space_id } }),
    ))
}
