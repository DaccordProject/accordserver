use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_channel_membership;
use crate::state::AppState;

/// GET /users/@me/read-states
/// Returns all channels where the authenticated user has unread messages.
pub async fn get_unread_channels(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let unreads = db::read_states::get_unread_channels(&state.db, &auth.user_id).await?;
    let data: Vec<serde_json::Value> = unreads
        .into_iter()
        .map(|u| {
            serde_json::json!({
                "channel_id": u.channel_id,
                "last_read_message_id": u.last_read_message_id,
                "last_message_id": u.last_message_id,
                "mention_count": u.mention_count,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

#[derive(Deserialize)]
pub struct AckChannelBody {
    pub message_id: String,
}

/// POST /channels/{channel_id}/ack
/// Mark a channel as read up to the given message ID.
pub async fn ack_channel(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<AckChannelBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;

    db::read_states::ack_channel(
        &state.db,
        &auth.user_id,
        &channel_id,
        &input.message_id,
        state.db_is_postgres,
    )
    .await?;

    Ok(Json(serde_json::json!({ "data": null })))
}
