use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_channel_permission, require_membership};
use crate::models::voice::VoiceState;
use crate::state::AppState;
use crate::voice;

pub async fn list_voice_regions(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let regions = vec![serde_json::json!({
        "id": "livekit",
        "name": "LiveKit",
        "optimal": true,
        "custom": false
    })];
    Ok(Json(serde_json::json!({ "data": regions })))
}

pub async fn get_voice_status(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "view_channel").await?;
    let states = voice::state::get_channel_voice_states(&state, &channel_id);
    Ok(Json(serde_json::json!({ "data": states })))
}

#[derive(serde::Deserialize)]
pub struct JoinVoiceRequest {
    pub self_mute: Option<bool>,
    pub self_deaf: Option<bool>,
}

pub async fn join_voice(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<JoinVoiceRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "connect").await?;

    // Look up channel to confirm it exists and get space_id
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;

    if channel.channel_type != "voice" {
        return Err(AppError::BadRequest("channel_not_voice".to_string()));
    }

    let space_id = channel
        .space_id
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("channel_has_no_space".to_string()))?;

    let session_id = crate::snowflake::generate();
    let self_mute = input.self_mute.unwrap_or(false);
    let self_deaf = input.self_deaf.unwrap_or(false);

    let (voice_state, previous_channel) = voice::state::join_voice_channel(
        &state,
        &auth.user_id,
        space_id,
        &channel_id,
        &session_id,
        self_mute,
        self_deaf,
    );

    let lk = state.livekit_client.as_ref()
        .ok_or_else(|| AppError::BadRequest("voice_not_configured".to_string()))?;

    // Clean up old LiveKit room if the user moved channels
    if let Some(ref prev_ch) = previous_channel {
        if !state.test_mode {
            lk.remove_participant(prev_ch, &auth.user_id).await;
            lk.delete_room_if_empty(prev_ch).await;
        }
    }

    // Broadcast voice.state_update to space
    broadcast_voice_state_update(&state, space_id, &voice_state).await;

    if !state.test_mode {
        lk.ensure_room(&channel_id).await?;
    }
    let user = db::users::get_user(&state.db, &auth.user_id).await?;
    let display_name = user.display_name.as_deref().unwrap_or(&user.username);
    let token = lk.generate_token(&auth.user_id, display_name, &channel_id)?;
    Ok(Json(serde_json::json!({
        "data": {
            "voice_state": voice_state,
            "backend": "livekit",
            "livekit_url": lk.external_url(),
            "token": token
        }
    })))
}

pub async fn leave_voice(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "connect").await?;
    let old_state = voice::state::leave_voice_channel(&state, &auth.user_id);

    if let Some(ref vs) = old_state {
        if let Some(ref space_id) = vs.space_id {
            let left_state = VoiceState {
                user_id: auth.user_id.clone(),
                space_id: vs.space_id.clone(),
                channel_id: None,
                session_id: vs.session_id.clone(),
                deaf: false,
                mute: false,
                self_deaf: false,
                self_mute: false,
                self_stream: false,
                self_video: false,
                suppress: false,
            };
            broadcast_voice_state_update(&state, space_id, &left_state).await;
        }

        // LiveKit cleanup
        if let Some(ref channel_id) = vs.channel_id {
            if !state.test_mode {
                if let Some(ref lk) = state.livekit_client {
                    lk.remove_participant(channel_id, &auth.user_id).await;
                    lk.delete_room_if_empty(channel_id).await;
                }
            }
        }
    }

    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

pub async fn voice_info(state: State<AppState>) -> Json<serde_json::Value> {
    let backend = if state.livekit_client.is_some() { "livekit" } else { "none" };
    Json(serde_json::json!({ "backend": backend }))
}

async fn broadcast_voice_state_update(state: &AppState, space_id: &str, voice_state: &VoiceState) {
    let event = serde_json::json!({
        "op": 0,
        "type": "voice.state_update",
        "data": voice_state
    });

    if let Some(ref tx) = *state.gateway_tx.read().await {
        let _ = tx.send(GatewayBroadcast {
            space_id: Some(space_id.to_string()),
            target_user_ids: None,
            event,
            intent: "voice_states".to_string(),
        });
    }
}
