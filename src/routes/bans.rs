use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_hierarchy, require_permission};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateBanBody {
    pub reason: Option<String>,
}

pub async fn list_bans(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    let bans = db::bans::list_bans(&state.db, &space_id).await?;
    let data: Vec<serde_json::Value> = bans
        .iter()
        .map(|b| {
            serde_json::json!({
                "user_id": b.user_id,
                "space_id": b.space_id,
                "reason": b.reason,
                "banned_by": b.banned_by,
                "created_at": b.created_at
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

pub async fn get_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    let ban = db::bans::get_ban(&state.db, &space_id, &user_id).await?;
    Ok(Json(serde_json::json!({
        "data": {
            "user_id": ban.user_id,
            "space_id": ban.space_id,
            "reason": ban.reason,
            "banned_by": ban.banned_by,
            "created_at": ban.created_at
        }
    })))
}

pub async fn create_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
    body: Option<Json<CreateBanBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    require_hierarchy(&state.db, &space_id, &auth, &user_id).await?;
    let reason = body.and_then(|b| b.reason.clone());
    let ban = db::bans::create_ban(
        &state.db,
        &space_id,
        &user_id,
        reason.as_deref(),
        &auth.user_id,
        state.db_is_postgres,
    )
    .await?;

    // `create_ban` deletes the member row, so this is a membership change and
    // has to be announced like one. Without it the banned user's gateway
    // session keeps its stale space set and carries on receiving the space
    // until it happens to reconnect, and everyone else's roster keeps showing
    // them. Kicks already do this (routes/members.rs); bans didn't.
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        // member.leave first: the banned user's own session drops the space on
        // this event (see gateway::Delivery::RefreshSpaces), so anything sent
        // after it no longer reaches them.
        let leave = serde_json::json!({
            "op": 0,
            "type": "member.leave",
            "data": { "space_id": space_id, "user_id": user_id }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event: leave,
            intent: "members".to_string(),
        });

        // And the moderation event the intent table already anticipates
        // (`intent_for_event` maps ban.create/ban.delete to "moderation"),
        // so moderation UIs can update live.
        let created = serde_json::json!({
            "op": 0,
            "type": "ban.create",
            "data": {
                "space_id": ban.space_id,
                "user_id": ban.user_id,
                "reason": ban.reason,
                "banned_by": ban.banned_by,
                "created_at": ban.created_at
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event: created,
            intent: "moderation".to_string(),
        });
    }

    Ok(Json(serde_json::json!({
        "data": {
            "user_id": ban.user_id,
            "space_id": ban.space_id,
            "reason": ban.reason,
            "banned_by": ban.banned_by,
            "created_at": ban.created_at
        }
    })))
}

pub async fn delete_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    require_hierarchy(&state.db, &space_id, &auth, &user_id).await?;
    db::bans::delete_ban(&state.db, &space_id, &user_id).await?;

    // Unbanning doesn't restore membership, so there's no member event here —
    // only the moderation one, so a moderation UI's ban list updates live.
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "ban.delete",
            "data": { "space_id": space_id, "user_id": user_id }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event,
            intent: "moderation".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}
