use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_membership, require_permission};
use crate::models::soundboard::{CreateSound, UpdateSound};
use crate::state::AppState;
use crate::storage;

pub async fn list_sounds(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let sounds = db::soundboard::list_sounds(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": sounds })))
}

pub async fn get_sound(
    state: State<AppState>,
    Path((space_id, sound_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let sound = db::soundboard::get_sound(&state.db, &sound_id).await?;
    Ok(Json(serde_json::json!({ "data": sound })))
}

pub async fn create_sound(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateSound>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_soundboard").await?;

    // Save audio file
    let id = crate::snowflake::generate();
    let (audio_path, content_type, size) =
        storage::save_base64_audio(&state.storage_path, &space_id, &id, &input.audio).await?;

    let sound = db::soundboard::create_sound(
        &state.db,
        &space_id,
        &auth.user_id,
        &input,
        Some(&audio_path),
        Some(&content_type),
        Some(size),
    )
    .await?;

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "soundboard.create",
            "data": {
                "space_id": space_id,
                "sound": sound
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "soundboard".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": sound })))
}

pub async fn update_sound(
    state: State<AppState>,
    Path((space_id, sound_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<UpdateSound>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_soundboard").await?;
    let sound = db::soundboard::update_sound(&state.db, &sound_id, &input).await?;

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "soundboard.update",
            "data": {
                "space_id": space_id,
                "sound": sound
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "soundboard".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": sound })))
}

pub async fn delete_sound(
    state: State<AppState>,
    Path((space_id, sound_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_soundboard").await?;

    let audio_path = db::soundboard::delete_sound(&state.db, &sound_id).await?;

    // Delete the file from disk
    if let Some(ref path) = audio_path {
        let _ = storage::delete_file(&state.storage_path, path).await;
    }

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "soundboard.delete",
            "data": {
                "space_id": space_id,
                "sound_id": sound_id
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "soundboard".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn play_sound(
    state: State<AppState>,
    Path((space_id, sound_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "use_soundboard").await?;

    // Verify the sound exists
    let sound = db::soundboard::get_sound(&state.db, &sound_id).await?;

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "soundboard.play",
            "data": {
                "space_id": space_id,
                "sound": sound,
                "user_id": auth.user_id
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "soundboard".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}
