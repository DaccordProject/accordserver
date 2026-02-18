use axum::extract::{Path, State};
use axum::Json;

use crate::config::VoiceBackend;
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
    let regions = match state.voice_backend {
        VoiceBackend::LiveKit => {
            vec![serde_json::json!({
                "id": "livekit",
                "name": "LiveKit",
                "optimal": true,
                "custom": false
            })]
        }
        VoiceBackend::Custom => {
            let mut regions = Vec::new();
            for entry in state.sfu_nodes.iter() {
                let node = entry.value();
                if node.status == "online" {
                    regions.push(serde_json::json!({
                        "id": node.region,
                        "name": node.region,
                        "optimal": false,
                        "custom": false
                    }));
                }
            }
            if regions.is_empty() {
                regions.push(serde_json::json!({
                    "id": "us-east",
                    "name": "US East",
                    "optimal": true,
                    "custom": false
                }));
            }
            regions
        }
    };
    Ok(Json(serde_json::json!({ "data": regions })))
}

pub async fn get_voice_status(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "view_channel").await?;
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
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "connect").await?;

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

    let (voice_state, _previous_channel) = voice::state::join_voice_channel(
        &state,
        &auth.user_id,
        space_id,
        &channel_id,
        &session_id,
        self_mute,
        self_deaf,
    );

    // Broadcast voice.state_update to space
    broadcast_voice_state_update(&state, space_id, &voice_state).await;

    match state.voice_backend {
        VoiceBackend::LiveKit => {
            let lk = state
                .livekit_client
                .as_ref()
                .ok_or_else(|| AppError::Internal("LiveKit client not configured".to_string()))?;
            lk.ensure_room(&channel_id).await?;
            let token = lk.generate_token(&auth.user_id, &channel_id)?;
            Ok(Json(serde_json::json!({
                "data": {
                    "voice_state": voice_state,
                    "backend": "livekit",
                    "livekit_url": lk.url(),
                    "token": token
                }
            })))
        }
        VoiceBackend::Custom => {
            let sfu_endpoint =
                voice::sfu::allocate_node(&state, None).map(|node| node.endpoint.clone());
            Ok(Json(serde_json::json!({
                "data": {
                    "voice_state": voice_state,
                    "backend": "custom",
                    "sfu_endpoint": sfu_endpoint
                }
            })))
        }
    }
}

pub async fn leave_voice(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "connect").await?;
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
        if state.voice_backend == VoiceBackend::LiveKit {
            if let (Some(ref lk), Some(ref channel_id)) = (&state.livekit_client, &vs.channel_id) {
                lk.remove_participant(channel_id, &auth.user_id).await;
                lk.delete_room_if_empty(channel_id).await;
            }
        }
    }

    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

pub async fn voice_info(state: State<AppState>) -> Json<serde_json::Value> {
    let backend = match state.voice_backend {
        VoiceBackend::LiveKit => "livekit",
        VoiceBackend::Custom => "custom",
    };
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
            event,
            intent: "voice_states".to_string(),
        });
    }
}
