use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::{AuthUser, OptionalAuthUser};
use crate::middleware::permissions::{require_channel_membership, require_channel_permission};
use crate::models::message::{BulkDeleteMessages, CreateMessage, MessageRow, UpdateMessage};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListMessagesQuery {
    pub after: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_messages(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: OptionalAuthUser,
    Query(params): Query<ListMessagesQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Allow unauthenticated read for channels in public spaces
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let is_public = if let Some(ref sid) = channel.space_id {
        db::spaces::get_space_row(&state.db, sid)
            .await
            .map(|s| s.public)
            .unwrap_or(false)
    } else {
        false
    };
    if !is_public {
        let user = auth
            .0
            .ok_or_else(|| AppError::Unauthorized("authentication required".into()))?;
        require_channel_membership(&state.db, &channel_id, &user.user_id).await?;
    }
    let limit = params.limit.unwrap_or(50).min(100);
    let mut rows =
        db::messages::list_messages(&state.db, &channel_id, params.after.as_deref(), limit).await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let messages: Vec<serde_json::Value> = rows.iter().map(message_row_to_json).collect();
    let last_id = rows.last().map(|m| m.id.clone());

    let mut response = serde_json::json!({ "data": messages });
    if has_more || last_id.is_some() {
        response["cursor"] = serde_json::json!({
            "after": last_id.unwrap_or_default(),
            "has_more": has_more
        });
    }
    Ok(Json(response))
}

pub async fn get_message(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: OptionalAuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    // Allow unauthenticated read for channels in public spaces
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let is_public = if let Some(ref sid) = channel.space_id {
        db::spaces::get_space_row(&state.db, sid)
            .await
            .map(|s| s.public)
            .unwrap_or(false)
    } else {
        false
    };
    if !is_public {
        let user = auth
            .0
            .ok_or_else(|| AppError::Unauthorized("authentication required".into()))?;
        require_channel_membership(&state.db, &channel_id, &user.user_id).await?;
    }
    let msg = db::messages::get_message_row(&state.db, &message_id).await?;
    if msg.channel_id != channel_id {
        return Err(AppError::NotFound("unknown_message".to_string()));
    }
    Ok(Json(
        serde_json::json!({ "data": message_row_to_json(&msg) }),
    ))
}

pub async fn create_message(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateMessage>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "send_messages").await?;
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let msg = db::messages::create_message(
        &state.db,
        &channel_id,
        &auth.user_id,
        channel.space_id.as_deref(),
        &input,
    )
    .await?;

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.create",
            "data": message_row_to_json(&msg)
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id,
            event,
            intent: "messages".to_string(),
        });
    }

    Ok(Json(
        serde_json::json!({ "data": message_row_to_json(&msg) }),
    ))
}

pub async fn update_message(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<UpdateMessage>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let existing = db::messages::get_message_row(&state.db, &message_id).await?;
    if existing.channel_id != channel_id {
        return Err(AppError::NotFound("unknown_message".to_string()));
    }
    if existing.author_id != auth.user_id {
        return Err(AppError::Forbidden(
            "you can only edit your own messages".to_string(),
        ));
    }
    let msg = db::messages::update_message(&state.db, &message_id, &input).await?;
    Ok(Json(
        serde_json::json!({ "data": message_row_to_json(&msg) }),
    ))
}

pub async fn delete_message(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let existing = db::messages::get_message_row(&state.db, &message_id).await?;
    if existing.channel_id != channel_id {
        return Err(AppError::NotFound("unknown_message".to_string()));
    }
    // Author can always delete their own message; otherwise need manage_messages
    if existing.author_id != auth.user_id {
        require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_messages")
            .await?;
    }
    db::messages::delete_message(&state.db, &message_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn bulk_delete_messages(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<BulkDeleteMessages>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_messages").await?;
    if input.messages.len() > 100 {
        return Err(AppError::BadRequest(
            "cannot bulk delete more than 100 messages".to_string(),
        ));
    }
    db::messages::bulk_delete_messages(&state.db, &input.messages).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn list_pins(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let rows = db::messages::list_pinned_messages(&state.db, &channel_id).await?;
    let messages: Vec<serde_json::Value> = rows.iter().map(message_row_to_json).collect();
    Ok(Json(serde_json::json!({ "data": messages })))
}

pub async fn pin_message(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_messages").await?;
    db::messages::pin_message(&state.db, &channel_id, &message_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn unpin_message(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "manage_messages").await?;
    db::messages::unpin_message(&state.db, &channel_id, &message_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn typing_indicator(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth.user_id, "send_messages").await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
        let event = serde_json::json!({
            "op": 0,
            "type": "typing.start",
            "data": {
                "channel_id": channel_id,
                "user_id": auth.user_id,
                "timestamp": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id,
            event,
            intent: "message_typing".to_string(),
        });
    }
    Ok(Json(serde_json::json!({ "data": null })))
}

pub fn message_row_to_json(row: &MessageRow) -> serde_json::Value {
    let mentions: Vec<String> = serde_json::from_str(&row.mentions).unwrap_or_default();
    let mention_roles: Vec<String> = serde_json::from_str(&row.mention_roles).unwrap_or_default();
    let embeds: Vec<serde_json::Value> = serde_json::from_str(&row.embeds).unwrap_or_default();

    serde_json::json!({
        "id": row.id,
        "channel_id": row.channel_id,
        "space_id": row.space_id,
        "author_id": row.author_id,
        "content": row.content,
        "type": row.message_type,
        "timestamp": row.created_at,
        "edited_at": row.edited_at,
        "tts": row.tts,
        "pinned": row.pinned,
        "mention_everyone": row.mention_everyone,
        "mentions": mentions,
        "mention_roles": mention_roles,
        "attachments": [],
        "embeds": embeds,
        "reactions": null,
        "reply_to": row.reply_to,
        "flags": row.flags,
        "webhook_id": row.webhook_id
    })
}
