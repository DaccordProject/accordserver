use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::models::user::UpdateUser;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateDmRequest {
    pub recipient_id: Option<String>,
    pub recipients: Option<Vec<String>>,
}

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

pub async fn create_dm_channel(
    state: State<AppState>,
    auth: AuthUser,
    Json(input): Json<CreateDmRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Build the recipient list from either field
    let recipient_ids: Vec<String> = match (input.recipient_id, input.recipients) {
        (Some(rid), _) => vec![rid],
        (_, Some(rids)) => rids,
        _ => {
            return Err(AppError::BadRequest(
                "recipient_id or recipients is required".into(),
            ))
        }
    };

    if recipient_ids.is_empty() {
        return Err(AppError::BadRequest(
            "at least one recipient is required".into(),
        ));
    }

    if recipient_ids.len() > 9 {
        return Err(AppError::BadRequest(
            "group DMs cannot have more than 10 participants".into(),
        ));
    }

    // Cannot DM yourself alone
    if recipient_ids.len() == 1 && recipient_ids[0] == auth.user_id {
        return Err(AppError::BadRequest(
            "cannot create a DM with yourself".into(),
        ));
    }

    // Validate all recipient IDs exist
    for rid in &recipient_ids {
        db::users::get_user(&state.db, rid).await?;
    }

    let channel =
        db::dm_participants::create_dm_channel(&state.db, &auth.user_id, &recipient_ids).await?;

    let json = super::spaces::channel_row_to_json_pub(&state.db, &channel).await;

    // Broadcast channel.create to all participants
    let participant_ids =
        db::dm_participants::list_participant_ids(&state.db, &channel.id).await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "channel.create",
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
