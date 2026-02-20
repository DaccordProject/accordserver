use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{
    require_channel_membership, require_channel_permission, require_dm_access,
};
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
    let existing = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if existing.channel_type == "group_dm" {
        require_dm_access(&state.db, &channel_id, &auth.user_id).await?;
        if existing.owner_id.as_deref() != Some(&auth.user_id) {
            return Err(AppError::Forbidden(
                "only the group owner can rename".into(),
            ));
        }
    } else if existing.channel_type == "dm" {
        return Err(AppError::BadRequest("cannot rename a 1:1 DM".into()));
    } else {
        require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_channels")
            .await?;
    }
    let channel = db::channels::update_channel(&state.db, &channel_id, &input).await?;
    let json = super::spaces::channel_row_to_json_pub(&state.db, &channel).await;

    // Broadcast channel.update for DM channels
    if existing.channel_type == "dm" || existing.channel_type == "group_dm" {
        let participant_ids =
            db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
        if let Some(ref dispatcher) = *state.gateway_tx.read().await {
            let event = serde_json::json!({
                "op": 0,
                "type": "channel.update",
                "data": json
            });
            let _ = dispatcher.send(GatewayBroadcast {
                space_id: None,
                target_user_ids: Some(participant_ids),
                event,
                intent: "channels".to_string(),
            });
        }
    }

    Ok(Json(serde_json::json!({ "data": json })))
}

pub async fn delete_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let existing = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if existing.channel_type == "dm" || existing.channel_type == "group_dm" {
        // For DM channels, "delete" means remove the caller from participants
        require_dm_access(&state.db, &channel_id, &auth.user_id).await?;
        db::dm_participants::remove_participant(&state.db, &channel_id, &auth.user_id).await?;

        let remaining = db::dm_participants::count_participants(&state.db, &channel_id).await?;
        if remaining <= 0 {
            // No participants left — actually delete the channel
            db::channels::delete_channel(&state.db, &channel_id).await?;
        } else if existing.channel_type == "group_dm"
            && existing.owner_id.as_deref() == Some(&auth.user_id)
        {
            // Owner left — transfer ownership to first remaining participant
            let ids =
                db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
            if let Some(new_owner) = ids.first() {
                let update = UpdateChannel {
                    name: None,
                    topic: None,
                    position: None,
                    parent_id: None,
                    nsfw: None,
                    rate_limit: None,
                    bitrate: None,
                    user_limit: None,
                    archived: None,
                };
                // We need to update owner_id directly since UpdateChannel doesn't have it
                sqlx::query("UPDATE channels SET owner_id = ? WHERE id = ?")
                    .bind(new_owner)
                    .bind(&channel_id)
                    .execute(&state.db)
                    .await?;
                let _ = update; // unused, just for clarity
            }
        }

        // Broadcast channel.update to remaining participants
        let participant_ids =
            db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
        if !participant_ids.is_empty() {
            let updated_channel =
                db::channels::get_channel_row(&state.db, &channel_id).await?;
            let json =
                super::spaces::channel_row_to_json_pub(&state.db, &updated_channel).await;
            if let Some(ref dispatcher) = *state.gateway_tx.read().await {
                let event = serde_json::json!({
                    "op": 0,
                    "type": "channel.update",
                    "data": json
                });
                let _ = dispatcher.send(GatewayBroadcast {
                    space_id: None,
                    target_user_ids: Some(participant_ids),
                    event,
                    intent: "channels".to_string(),
                });
            }
        }

        return Ok(Json(serde_json::json!({ "data": null })));
    }

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

pub async fn add_recipient(
    state: State<AppState>,
    Path((channel_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if channel.channel_type != "group_dm" {
        return Err(AppError::BadRequest(
            "can only add recipients to group DMs".into(),
        ));
    }
    require_dm_access(&state.db, &channel_id, &auth.user_id).await?;
    if channel.owner_id.as_deref() != Some(&auth.user_id) {
        return Err(AppError::Forbidden(
            "only the group owner can add members".into(),
        ));
    }

    // Validate target user exists
    db::users::get_user(&state.db, &user_id).await?;

    // Check participant count
    let count = db::dm_participants::count_participants(&state.db, &channel_id).await?;
    if count >= 10 {
        return Err(AppError::BadRequest(
            "group DMs cannot have more than 10 participants".into(),
        ));
    }

    db::dm_participants::add_participant(&state.db, &channel_id, &user_id).await?;

    let updated = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let json = super::spaces::channel_row_to_json_pub(&state.db, &updated).await;

    // Broadcast channel.update to all participants (including the new one)
    let participant_ids =
        db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "channel.update",
            "data": json
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(participant_ids),
            event,
            intent: "channels".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": json })))
}

pub async fn remove_recipient(
    state: State<AppState>,
    Path((channel_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if channel.channel_type != "group_dm" {
        return Err(AppError::BadRequest(
            "can only remove recipients from group DMs".into(),
        ));
    }
    require_dm_access(&state.db, &channel_id, &auth.user_id).await?;

    // Can remove self, or owner can remove others
    if user_id != auth.user_id {
        if channel.owner_id.as_deref() != Some(&auth.user_id) {
            return Err(AppError::Forbidden(
                "only the group owner can remove members".into(),
            ));
        }
    }

    db::dm_participants::remove_participant(&state.db, &channel_id, &user_id).await?;

    let remaining = db::dm_participants::count_participants(&state.db, &channel_id).await?;
    if remaining <= 1 {
        // Not enough participants — delete the channel
        db::channels::delete_channel(&state.db, &channel_id).await?;
        // Broadcast channel.delete to remaining participant if any
        let remaining_ids =
            db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
        if !remaining_ids.is_empty() {
            if let Some(ref dispatcher) = *state.gateway_tx.read().await {
                let event = serde_json::json!({
                    "op": 0,
                    "type": "channel.delete",
                    "data": { "id": channel_id }
                });
                let _ = dispatcher.send(GatewayBroadcast {
                    space_id: None,
                    target_user_ids: Some(remaining_ids),
                    event,
                    intent: "channels".to_string(),
                });
            }
        }
        return Ok(Json(serde_json::json!({ "data": null })));
    }

    // Transfer ownership if the owner left
    if channel.owner_id.as_deref() == Some(&user_id) {
        let ids = db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
        if let Some(new_owner) = ids.first() {
            sqlx::query("UPDATE channels SET owner_id = ? WHERE id = ?")
                .bind(new_owner)
                .bind(&channel_id)
                .execute(&state.db)
                .await?;
        }
    }

    let updated = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let json = super::spaces::channel_row_to_json_pub(&state.db, &updated).await;

    // Broadcast channel.update to remaining participants
    let participant_ids =
        db::dm_participants::list_participant_ids(&state.db, &channel_id).await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "channel.update",
            "data": json
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(participant_ids),
            event,
            intent: "channels".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": json })))
}
