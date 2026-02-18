use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_membership, require_permission};
use crate::models::emoji::{CreateEmoji, UpdateEmoji};
use crate::state::AppState;
use crate::storage;

pub async fn list_emojis(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let emojis = db::emojis::list_emojis(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": emojis })))
}

pub async fn get_emoji(
    state: State<AppState>,
    Path((space_id, emoji_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let emoji = db::emojis::get_emoji(&state.db, &emoji_id).await?;
    Ok(Json(serde_json::json!({ "data": emoji })))
}

pub async fn create_emoji(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateEmoji>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_emojis").await?;

    // Save the image file
    let (image_path, content_type, size, animated) =
        storage::save_base64_image(&state.storage_path, &space_id, &input.name, &input.image)
            .await?;

    let emoji = db::emojis::create_emoji(
        &state.db,
        &space_id,
        &auth.user_id,
        &input,
        Some(&image_path),
        Some(&content_type),
        Some(size),
        animated,
    )
    .await?;

    // Rename the file to use the actual emoji ID instead of the name
    if let (Some(ref emoji_id), Some(_)) = (&emoji.id, &emoji.image_url) {
        // The file was saved with input.name, but we want it named by ID
        // Re-save with the correct ID-based path
        let _ = storage::delete_file(&state.storage_path, &image_path).await;
        let (real_path, _, _, _) =
            storage::save_base64_image(&state.storage_path, &space_id, emoji_id, &input.image)
                .await?;

        // Update the DB with the correct path
        sqlx::query("UPDATE emojis SET image_path = ? WHERE id = ?")
            .bind(&real_path)
            .bind(emoji_id)
            .execute(&state.db)
            .await?;

        // Re-fetch to get the updated path
        let emoji = db::emojis::get_emoji(&state.db, emoji_id).await?;

        // Broadcast to gateway
        if let Some(ref dispatcher) = *state.gateway_tx.read().await {
            let event = serde_json::json!({
                "op": 0,
                "type": "emoji.create",
                "data": {
                    "space_id": space_id,
                    "emoji": emoji
                }
            });
            let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
                space_id: Some(space_id),
                event,
                intent: "emojis".to_string(),
            });
        }

        return Ok(Json(serde_json::json!({ "data": emoji })));
    }

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "emoji.create",
            "data": {
                "space_id": space_id,
                "emoji": emoji
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "emojis".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": emoji })))
}

pub async fn update_emoji(
    state: State<AppState>,
    Path((space_id, emoji_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<UpdateEmoji>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_emojis").await?;
    let emoji = db::emojis::update_emoji(&state.db, &emoji_id, &input).await?;

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "emoji.update",
            "data": {
                "space_id": space_id,
                "emoji": emoji
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "emojis".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": emoji })))
}

pub async fn delete_emoji(
    state: State<AppState>,
    Path((space_id, emoji_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_emojis").await?;

    let image_path = db::emojis::delete_emoji(&state.db, &emoji_id).await?;

    // Delete the file from disk
    if let Some(ref path) = image_path {
        let _ = storage::delete_file(&state.storage_path, path).await;
    }

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "emoji.delete",
            "data": {
                "space_id": space_id,
                "emoji_id": emoji_id
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            event,
            intent: "emojis".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}
