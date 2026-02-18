use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::db::messages::ReactionAggregate;
use crate::error::AppError;
use crate::middleware::auth::{AuthUser, OptionalAuthUser};
use crate::middleware::permissions::{
    require_channel_membership, require_channel_permission, resolve_channel_permissions,
};
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
    let current_user_id = auth.0.as_ref().map(|u| u.user_id.clone());
    let is_public = if let Some(ref sid) = channel.space_id {
        db::spaces::get_space_row(&state.db, sid)
            .await
            .map(|s| s.public)
            .unwrap_or(false)
    } else {
        false
    };
    if !is_public {
        let uid = current_user_id
            .as_deref()
            .ok_or_else(|| AppError::Unauthorized("authentication required".into()))?;
        require_channel_membership(&state.db, &channel_id, uid).await?;
    }
    let limit = params.limit.unwrap_or(50).min(100);
    let mut rows =
        db::messages::list_messages(&state.db, &channel_id, params.after.as_deref(), limit).await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let messages = messages_to_json(&state.db, &rows, current_user_id.as_deref()).await?;
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
    let current_user_id = auth.0.as_ref().map(|u| u.user_id.clone());
    let is_public = if let Some(ref sid) = channel.space_id {
        db::spaces::get_space_row(&state.db, sid)
            .await
            .map(|s| s.public)
            .unwrap_or(false)
    } else {
        false
    };
    if !is_public {
        let uid = current_user_id
            .as_deref()
            .ok_or_else(|| AppError::Unauthorized("authentication required".into()))?;
        require_channel_membership(&state.db, &channel_id, uid).await?;
    }
    let msg = db::messages::get_message_row(&state.db, &message_id).await?;
    if msg.channel_id != channel_id {
        return Err(AppError::NotFound("unknown_message".to_string()));
    }
    let msgs = messages_to_json(&state.db, &[msg], current_user_id.as_deref()).await?;
    Ok(Json(
        serde_json::json!({ "data": msgs.into_iter().next().unwrap() }),
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

    // Broadcast to gateway
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.update",
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

    // Broadcast to gateway
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.delete",
            "data": {
                "id": message_id,
                "channel_id": channel_id,
                "space_id": channel.space_id,
            }
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id.clone(),
            event,
            intent: "messages".to_string(),
        });
    }

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
    db::messages::bulk_delete_messages(&state.db, &channel_id, &input.messages).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn list_pins(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let rows = db::messages::list_pinned_messages(&state.db, &channel_id).await?;
    let messages = messages_to_json(&state.db, &rows, Some(&auth.user_id)).await?;
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

#[derive(Deserialize)]
pub struct SearchMessagesQuery {
    pub query: Option<String>,
    pub author_id: Option<String>,
    pub channel_id: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub pinned: Option<bool>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

pub async fn search_messages(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: OptionalAuthUser,
    Query(params): Query<SearchMessagesQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    // At least one filter must be present
    if params.query.is_none()
        && params.author_id.is_none()
        && params.channel_id.is_none()
        && params.before.is_none()
        && params.after.is_none()
        && params.pinned.is_none()
    {
        return Err(AppError::BadRequest(
            "at least one search filter is required".to_string(),
        ));
    }

    // Check space existence and publicity
    let space = db::spaces::get_space_row(&state.db, &space_id).await?;
    let is_public = space.public;

    // Determine accessible channel IDs
    let all_channels = db::channels::list_channels_in_space(&state.db, &space_id).await?;

    let accessible_channel_ids: Vec<String> = if let Some(ref user) = auth.0 {
        // Authenticated: filter channels by view_channel permission
        let mut ids = Vec::new();
        for ch in &all_channels {
            let perms =
                resolve_channel_permissions(&state.db, &ch.id, &space_id, &user.user_id).await;
            if let Ok(perms) = perms {
                if perms.iter().any(|p| p == "administrator" || p == "view_channel") {
                    ids.push(ch.id.clone());
                }
            }
        }
        if ids.is_empty() {
            return Err(AppError::Forbidden(
                "you are not a member of this space".to_string(),
            ));
        }
        ids
    } else if is_public {
        // Unauthenticated on public space: all channels
        all_channels.iter().map(|c| c.id.clone()).collect()
    } else {
        return Err(AppError::Unauthorized("authentication required".into()));
    };

    // If channel_id param given, validate and intersect
    let final_channel_ids = if let Some(ref cid) = params.channel_id {
        if !accessible_channel_ids.contains(cid) {
            return Err(AppError::Forbidden(
                "you do not have access to that channel".to_string(),
            ));
        }
        // Also verify the channel belongs to this space
        let ch = db::channels::get_channel_row(&state.db, cid).await?;
        if ch.space_id.as_deref() != Some(&space_id) {
            return Err(AppError::NotFound("unknown_channel".to_string()));
        }
        vec![cid.clone()]
    } else {
        accessible_channel_ids
    };

    let limit = params.limit.unwrap_or(25).min(100);

    let search_params = db::messages::SearchMessagesParams {
        channel_ids: &final_channel_ids,
        query: params.query.as_deref(),
        author_id: params.author_id.as_deref(),
        before: params.before.as_deref(),
        after: params.after.as_deref(),
        pinned: params.pinned,
        cursor: params.cursor.as_deref(),
        limit,
    };

    let mut rows = db::messages::search_messages(&state.db, &space_id, &search_params).await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let user_id = auth.0.as_ref().map(|u| u.user_id.as_str());
    let messages = messages_to_json(&state.db, &rows, user_id).await?;
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

pub fn message_row_to_json(row: &MessageRow) -> serde_json::Value {
    message_row_to_json_with_reactions(row, None)
}

pub fn message_row_to_json_with_reactions(
    row: &MessageRow,
    reactions: Option<&Vec<ReactionAggregate>>,
) -> serde_json::Value {
    let mentions: Vec<String> = serde_json::from_str(&row.mentions).unwrap_or_default();
    let mention_roles: Vec<String> = serde_json::from_str(&row.mention_roles).unwrap_or_default();
    let embeds: Vec<serde_json::Value> = serde_json::from_str(&row.embeds).unwrap_or_default();

    let reactions_json = match reactions {
        Some(rs) if !rs.is_empty() => {
            let arr: Vec<serde_json::Value> = rs
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "emoji": { "id": null, "name": r.emoji_name },
                        "count": r.count,
                        "me": r.includes_me,
                    })
                })
                .collect();
            serde_json::Value::Array(arr)
        }
        _ => serde_json::Value::Null,
    };

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
        "reactions": reactions_json,
        "reply_to": row.reply_to,
        "flags": row.flags,
        "webhook_id": row.webhook_id
    })
}

/// Converts a batch of message rows to JSON, enriching each with its reactions.
pub async fn messages_to_json(
    pool: &sqlx::SqlitePool,
    rows: &[MessageRow],
    current_user_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, crate::error::AppError> {
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let reactions_map = db::messages::get_reactions_for_messages(pool, &ids, current_user_id).await?;
    Ok(rows
        .iter()
        .map(|row| message_row_to_json_with_reactions(row, reactions_map.get(&row.id)))
        .collect())
}
