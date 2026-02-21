use axum::extract::{Multipart, Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::db::messages::ReactionAggregate;
use crate::error::AppError;
use crate::middleware::auth::{AuthUser, OptionalAuthUser};
use crate::middleware::permissions::{
    require_channel_membership, require_channel_permission, resolve_channel_permissions,
};
use crate::models::attachment::Attachment;
use crate::models::message::{BulkDeleteMessages, CreateMessage, MessageRow, UpdateMessage};
use crate::state::AppState;
use crate::storage;

const MAX_ATTACHMENTS: usize = 10;

#[derive(Deserialize)]
pub struct ListMessagesQuery {
    pub after: Option<String>,
    pub limit: Option<i64>,
    pub thread_id: Option<String>,
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
        db::messages::list_messages(&state.db, &channel_id, params.after.as_deref(), limit, params.thread_id.as_deref()).await?;

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
    require_channel_permission(&state.db, &channel_id, &auth, "send_messages").await?;
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let msg = db::messages::create_message(
        &state.db,
        &channel_id,
        &auth.user_id,
        channel.space_id.as_deref(),
        &input,
    )
    .await?;

    let json = message_row_to_json_with_attachments(&msg, &[], None);

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.create",
            "data": json
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id.clone(),
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }

    // Spawn URL unfurling in the background -- if the message has no embeds
    // already and its content contains URLs, fetch OpenGraph metadata and
    // update the message with generated embeds.
    if input.embeds.as_ref().map_or(true, |e| e.is_empty()) {
        let content = input.content.clone();
        let msg_id = msg.id.clone();
        let space_id = channel.space_id.clone();
        let db = state.db.clone();
        let gateway_tx = state.gateway_tx.clone();
        tokio::spawn(async move {
            let embeds = crate::unfurl::unfurl_message_urls(&content).await;
            if embeds.is_empty() {
                return;
            }
            let update = UpdateMessage {
                content: None,
                embeds: Some(embeds),
            };
            if let Ok(updated_msg) = db::messages::update_message(&db, &msg_id, &update).await {
                let attachments = db::attachments::get_attachments_for_message(&db, &msg_id)
                    .await
                    .unwrap_or_default();
                let json = message_row_to_json_with_attachments(&updated_msg, &attachments, None);
                if let Some(ref dispatcher) = *gateway_tx.read().await {
                    let event = serde_json::json!({
                        "op": 0,
                        "type": "message.update",
                        "data": json
                    });
                    let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
                        space_id,
                        target_user_ids: None,
                        event,
                        intent: "messages".to_string(),
                    });
                }
            }
        });
    }

    Ok(Json(serde_json::json!({ "data": json })))
}

/// Handles multipart/form-data message creation with file attachments.
/// Expects a `payload_json` field with the message metadata and zero or more
/// file fields named `files[0]`, `files[1]`, etc.
pub async fn create_message_multipart(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "send_messages").await?;

    let mut payload_json: Option<CreateMessage> = None;
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new(); // (filename, content_type, bytes)

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("failed to read multipart field: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();

        if name == "payload_json" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::BadRequest(format!("failed to read payload_json: {e}")))?;
            payload_json = Some(
                serde_json::from_str(&text)
                    .map_err(|e| AppError::BadRequest(format!("invalid payload_json: {e}")))?,
            );
        } else if name.starts_with("files[") {
            if files.len() >= MAX_ATTACHMENTS {
                return Err(AppError::BadRequest(format!(
                    "maximum {MAX_ATTACHMENTS} attachments per message"
                )));
            }
            let filename = field
                .file_name()
                .unwrap_or("attachment")
                .to_string();
            let content_type = field
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(format!("failed to read file: {e}")))?;
            files.push((filename, content_type, bytes.to_vec()));
        }
    }

    let input = payload_json.ok_or_else(|| {
        AppError::BadRequest("missing payload_json field in multipart request".to_string())
    })?;

    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let msg = db::messages::create_message(
        &state.db,
        &channel_id,
        &auth.user_id,
        channel.space_id.as_deref(),
        &input,
    )
    .await?;

    // Save files and create attachment records
    let mut attachments: Vec<Attachment> = Vec::new();
    for (filename, content_type, bytes) in &files {
        let (url, size) = storage::save_attachment(
            &state.storage_path,
            &channel_id,
            &msg.id,
            filename,
            bytes,
        )
        .await?;

        // Detect image dimensions for image content types
        let (width, height) = if content_type.starts_with("image/") {
            detect_image_dimensions(bytes)
        } else {
            (None, None)
        };

        let attachment = db::attachments::insert_attachment(
            &state.db,
            &msg.id,
            &channel_id,
            filename,
            Some(content_type.as_str()),
            size as i64,
            &url,
            width,
            height,
        )
        .await?;
        attachments.push(attachment);
    }

    let json = message_row_to_json_with_attachments(&msg, &attachments, None);

    // Broadcast to gateway
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.create",
            "data": json
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id,
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": json })))
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

    // Load existing attachments for the response
    let attachments =
        db::attachments::get_attachments_for_message(&state.db, &message_id).await?;
    let json = message_row_to_json_with_attachments(&msg, &attachments, None);

    // Broadcast to gateway
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.update",
            "data": json
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id,
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": json })))
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
        require_channel_permission(&state.db, &channel_id, &auth, "manage_messages")
            .await?;
    }

    // Delete attachment files from disk before deleting the message
    let attachments =
        db::attachments::get_attachments_for_message(&state.db, &message_id).await?;
    for att in &attachments {
        let _ = storage::delete_file(&state.storage_path, &att.url).await;
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
            target_user_ids: None,
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
    require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
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
    require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    db::messages::pin_message(&state.db, &channel_id, &message_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn unpin_message(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    db::messages::unpin_message(&state.db, &channel_id, &message_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn typing_indicator(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "send_messages").await?;
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
            target_user_ids: None,
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

pub async fn get_thread_info(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let msg = db::messages::get_message_row(&state.db, &message_id).await?;
    if msg.channel_id != channel_id {
        return Err(AppError::NotFound("unknown_message".to_string()));
    }
    let metadata = db::messages::get_thread_metadata(&state.db, &message_id).await?;
    Ok(Json(serde_json::json!({ "data": metadata })))
}

pub async fn list_active_threads(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let rows = db::messages::list_active_threads(&state.db, &channel_id).await?;
    let messages = messages_to_json(&state.db, &rows, Some(&auth.user_id)).await?;
    Ok(Json(serde_json::json!({ "data": messages })))
}

// --- JSON serialization helpers ---

pub fn message_row_to_json(row: &MessageRow) -> serde_json::Value {
    message_row_to_json_with_attachments(row, &[], None)
}

pub fn message_row_to_json_with_attachments(
    row: &MessageRow,
    attachments: &[Attachment],
    reactions: Option<&Vec<ReactionAggregate>>,
) -> serde_json::Value {
    message_row_to_json_full(row, attachments, reactions, None)
}

pub fn message_row_to_json_full(
    row: &MessageRow,
    attachments: &[Attachment],
    reactions: Option<&Vec<ReactionAggregate>>,
    reply_count: Option<i64>,
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

    let attachments_json: Vec<serde_json::Value> = attachments
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();

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
        "attachments": attachments_json,
        "embeds": embeds,
        "reactions": reactions_json,
        "reply_to": row.reply_to,
        "flags": row.flags,
        "webhook_id": row.webhook_id,
        "thread_id": row.thread_id,
        "reply_count": reply_count.unwrap_or(0)
    })
}

/// Converts a batch of message rows to JSON, enriching each with its
/// reactions, attachments, and thread reply counts.
pub async fn messages_to_json(
    pool: &sqlx::SqlitePool,
    rows: &[MessageRow],
    current_user_id: Option<&str>,
) -> Result<Vec<serde_json::Value>, crate::error::AppError> {
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let reactions_map =
        db::messages::get_reactions_for_messages(pool, &ids, current_user_id).await?;
    let attachments_map = db::attachments::get_attachments_for_messages(pool, &ids).await?;
    let reply_counts = db::messages::get_thread_reply_counts(pool, &ids).await?;
    Ok(rows
        .iter()
        .map(|row| {
            let atts = attachments_map
                .get(&row.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let count = reply_counts.get(&row.id).copied();
            message_row_to_json_full(row, atts, reactions_map.get(&row.id), count)
        })
        .collect())
}

/// Try to detect image dimensions from raw bytes (PNG and JPEG).
fn detect_image_dimensions(bytes: &[u8]) -> (Option<i64>, Option<i64>) {
    // PNG: bytes 16-19 = width, 20-23 = height (big-endian u32 in IHDR)
    if bytes.len() >= 24 && bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as i64;
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]) as i64;
        return (Some(width), Some(height));
    }

    // JPEG: scan for SOF0 (0xFF 0xC0) or SOF2 (0xFF 0xC2) marker
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2;
        while i + 1 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xC0 || marker == 0xC2 {
                if i + 9 < bytes.len() {
                    let height =
                        u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as i64;
                    let width =
                        u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as i64;
                    return (Some(width), Some(height));
                }
                break;
            }
            // Skip marker segment
            if i + 3 < bytes.len() {
                let seg_len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
                i += 2 + seg_len;
            } else {
                break;
            }
        }
    }

    (None, None)
}
