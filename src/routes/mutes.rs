use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_channel_membership;
use crate::state::AppState;

/// PUT /channels/{channel_id}/mute
pub async fn mute_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;

    if channel.space_id.is_some() {
        require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    }

    let mute = db::mutes::create_mute(&state.db, &auth.user_id, &channel_id, state.db_is_postgres).await?;

    // Notify this user's gateway sessions to refresh their mute list
    if let Some(ref gtx) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "channel_mute.create",
            "data": { "channel_id": channel_id }
        });
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(vec![auth.user_id.clone()]),
            event,
            intent: "channels".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": mute })))
}

/// DELETE /channels/{channel_id}/mute
pub async fn unmute_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    db::mutes::delete_mute(&state.db, &auth.user_id, &channel_id).await?;

    if let Some(ref gtx) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "channel_mute.delete",
            "data": { "channel_id": channel_id }
        });
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(vec![auth.user_id.clone()]),
            event,
            intent: "channels".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}

/// GET /users/@me/mutes
pub async fn list_mutes(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let mutes = db::mutes::list_mutes_for_user(&state.db, &auth.user_id).await?;
    Ok(Json(serde_json::json!({ "data": mutes })))
}
