use serde_json::Value;

use crate::db;
use crate::models::message::CreateMessage;
use crate::state::AppState;

/// Returns the JSON schema definitions for all MCP tools.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool_def(
            "list_spaces",
            "List all spaces (servers) on this Accord instance",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool_def(
            "get_space",
            "Get details about a specific space by ID",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" }
                },
                "required": ["space_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "list_channels",
            "List all channels in a space",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" }
                },
                "required": ["space_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "list_members",
            "List members of a space",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" },
                    "limit": { "type": "integer", "description": "Max members to return (default 50, max 200)" }
                },
                "required": ["space_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "get_user",
            "Get information about a user by ID",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "user_id": { "type": "string", "description": "The user ID" }
                },
                "required": ["user_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "list_messages",
            "List recent messages in a channel",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "The channel ID" },
                    "limit": { "type": "integer", "description": "Max messages to return (default 50, max 100)" },
                    "after": { "type": "string", "description": "Return messages after this message ID (for pagination)" }
                },
                "required": ["channel_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "search_messages",
            "Search messages in a space by content, author, or other filters",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID to search in" },
                    "query": { "type": "string", "description": "Text to search for in message content" },
                    "author_id": { "type": "string", "description": "Filter by author user ID" },
                    "channel_id": { "type": "string", "description": "Filter to a specific channel" },
                    "limit": { "type": "integer", "description": "Max results (default 25, max 100)" }
                },
                "required": ["space_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "send_message",
            "Send a message to a channel",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "The channel ID to send to" },
                    "content": { "type": "string", "description": "Message content" },
                    "reply_to": { "type": "string", "description": "Message ID to reply to (optional)" }
                },
                "required": ["channel_id", "content"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "create_channel",
            "Create a new channel in a space",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" },
                    "name": { "type": "string", "description": "Channel name" },
                    "channel_type": { "type": "string", "description": "Channel type: 'text' or 'voice' (default: 'text')" },
                    "topic": { "type": "string", "description": "Channel topic/description (optional)" },
                    "parent_id": { "type": "string", "description": "Parent category channel ID (optional)" }
                },
                "required": ["space_id", "name"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "delete_channel",
            "Delete a channel",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "The channel ID to delete" }
                },
                "required": ["channel_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "kick_member",
            "Remove a member from a space",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" },
                    "user_id": { "type": "string", "description": "The user ID to kick" }
                },
                "required": ["space_id", "user_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "ban_user",
            "Ban a user from a space",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" },
                    "user_id": { "type": "string", "description": "The user ID to ban" },
                    "reason": { "type": "string", "description": "Reason for the ban (optional)" }
                },
                "required": ["space_id", "user_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "unban_user",
            "Remove a ban from a user in a space",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "space_id": { "type": "string", "description": "The space ID" },
                    "user_id": { "type": "string", "description": "The user ID to unban" }
                },
                "required": ["space_id", "user_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "delete_message",
            "Delete a message by ID",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "message_id": { "type": "string", "description": "The message ID to delete" }
                },
                "required": ["message_id"],
                "additionalProperties": false
            }),
        ),
        tool_def(
            "server_info",
            "Get general server information and statistics",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
    ]
}

fn tool_def(name: &str, description: &str, input_schema: Value) -> Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

/// Execute a tool by name. Returns Ok(text) on success, Err(text) on failure.
pub async fn call_tool(state: &AppState, name: &str, args: Value) -> Result<String, String> {
    match name {
        "list_spaces" => tool_list_spaces(state).await,
        "get_space" => tool_get_space(state, &args).await,
        "list_channels" => tool_list_channels(state, &args).await,
        "list_members" => tool_list_members(state, &args).await,
        "get_user" => tool_get_user(state, &args).await,
        "list_messages" => tool_list_messages(state, &args).await,
        "search_messages" => tool_search_messages(state, &args).await,
        "send_message" => tool_send_message(state, &args).await,
        "create_channel" => tool_create_channel(state, &args).await,
        "delete_channel" => tool_delete_channel(state, &args).await,
        "kick_member" => tool_kick_member(state, &args).await,
        "ban_user" => tool_ban_user(state, &args).await,
        "unban_user" => tool_unban_user(state, &args).await,
        "delete_message" => tool_delete_message(state, &args).await,
        "server_info" => tool_server_info(state).await,
        _ => Err(format!("Unknown tool: {name}")),
    }
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required parameter: {key}"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn to_json(val: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(val).unwrap_or_else(|_| "{}".into())
}

fn map_err(e: crate::error::AppError) -> String {
    format!("Error: {e}")
}

// ── Tool implementations ──────────────────────────────────────────

async fn tool_list_spaces(state: &AppState) -> Result<String, String> {
    let spaces = db::admin::list_all_spaces(&state.db, None, 100, None)
        .await
        .map_err(map_err)?;
    Ok(to_json(&spaces))
}

async fn tool_get_space(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let space = db::spaces::get_space_row(&state.db, space_id)
        .await
        .map_err(map_err)?;
    Ok(to_json(&space))
}

async fn tool_list_channels(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let channels = db::channels::list_channels_in_space(&state.db, space_id)
        .await
        .map_err(map_err)?;
    // ChannelRow doesn't derive Serialize by default, convert to JSON manually
    let result: Vec<Value> = channels
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "name": c.name,
                "type": c.channel_type,
                "space_id": c.space_id,
                "topic": c.topic,
                "position": c.position,
                "parent_id": c.parent_id,
                "nsfw": c.nsfw,
                "created_at": c.created_at,
            })
        })
        .collect();
    Ok(to_json(&result))
}

async fn tool_list_members(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let limit = opt_i64(args, "limit").unwrap_or(50).min(200);
    let members = db::members::list_members(&state.db, space_id, None, limit)
        .await
        .map_err(map_err)?;
    let result: Vec<Value> = members
        .iter()
        .map(|m| {
            serde_json::json!({
                "user_id": m.user_id,
                "space_id": m.space_id,
                "nickname": m.nickname,
                "joined_at": m.joined_at,
            })
        })
        .collect();
    Ok(to_json(&result))
}

async fn tool_get_user(state: &AppState, args: &Value) -> Result<String, String> {
    let user_id = require_str(args, "user_id")?;
    let user = db::users::get_user(&state.db, user_id)
        .await
        .map_err(map_err)?;
    Ok(to_json(&user))
}

async fn tool_list_messages(state: &AppState, args: &Value) -> Result<String, String> {
    let channel_id = require_str(args, "channel_id")?;
    let limit = opt_i64(args, "limit").unwrap_or(50).min(100);
    let after = opt_str(args, "after");
    let messages = db::messages::list_messages(&state.db, channel_id, after, limit, None)
        .await
        .map_err(map_err)?;
    let result: Vec<Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "channel_id": m.channel_id,
                "author_id": m.author_id,
                "content": m.content,
                "created_at": m.created_at,
                "edited_at": m.edited_at,
                "reply_to": m.reply_to,
                "pinned": m.pinned,
            })
        })
        .collect();
    Ok(to_json(&result))
}

async fn tool_search_messages(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let limit = opt_i64(args, "limit").unwrap_or(25).min(100);

    // If channel_id is provided, restrict to that channel
    let channel_ids: Vec<String> = match opt_str(args, "channel_id") {
        Some(cid) => vec![cid.to_string()],
        None => db::channels::list_channels_in_space(&state.db, space_id)
            .await
            .map_err(map_err)?
            .into_iter()
            .map(|c| c.id)
            .collect(),
    };

    let params = db::messages::SearchMessagesParams {
        channel_ids: &channel_ids,
        query: opt_str(args, "query"),
        author_id: opt_str(args, "author_id"),
        before: None,
        after: None,
        pinned: None,
        cursor: None,
        limit,
    };

    let messages = db::messages::search_messages(&state.db, space_id, &params)
        .await
        .map_err(map_err)?;
    let result: Vec<Value> = messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "channel_id": m.channel_id,
                "author_id": m.author_id,
                "content": m.content,
                "created_at": m.created_at,
            })
        })
        .collect();
    Ok(to_json(&result))
}

async fn tool_send_message(state: &AppState, args: &Value) -> Result<String, String> {
    let channel_id = require_str(args, "channel_id")?;
    let content = require_str(args, "content")?;
    let reply_to = opt_str(args, "reply_to").map(String::from);

    // Look up the channel to get space_id
    let channel = db::channels::get_channel_row(&state.db, channel_id)
        .await
        .map_err(map_err)?;

    // MCP-originated messages are attributed to the System user so they
    // satisfy the messages.author_id → users.id foreign key.
    let system_user_id = db::users::get_or_create_system_user(&state.db)
        .await
        .map_err(map_err)?;

    let input = CreateMessage {
        content: content.to_string(),
        tts: None,
        embeds: None,
        reply_to,
        thread_id: None,
        title: None,
    };

    let msg = db::messages::create_message(
        &state.db,
        channel_id,
        &system_user_id,
        channel.space_id.as_deref(),
        &input,
    )
    .await
    .map_err(map_err)?;

    // Broadcast via gateway if available
    if let Some(ref tx) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.create",
            "data": {
                "id": msg.id,
                "channel_id": msg.channel_id,
                "space_id": msg.space_id,
                "author_id": msg.author_id,
                "content": msg.content,
                "type": msg.message_type,
                "timestamp": msg.created_at,
            }
        });
        let _ = tx.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id.clone(),
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }

    Ok(serde_json::json!({
        "id": msg.id,
        "channel_id": msg.channel_id,
        "content": msg.content,
        "created_at": msg.created_at,
    })
    .to_string())
}

async fn tool_create_channel(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let name = require_str(args, "name")?;
    let channel_type = opt_str(args, "channel_type").unwrap_or("text");
    let topic = opt_str(args, "topic");
    let parent_id = opt_str(args, "parent_id");

    let input = crate::models::channel::CreateChannel {
        name: name.to_string(),
        channel_type: channel_type.to_string(),
        topic: topic.map(String::from),
        parent_id: parent_id.map(String::from),
        position: None,
        nsfw: None,
        bitrate: None,
        user_limit: None,
        rate_limit: None,
        allow_anonymous_read: None,
    };

    let channel = db::channels::create_channel(&state.db, space_id, &input)
        .await
        .map_err(map_err)?;

    // Broadcast channel.create so connected clients live-update their sidebar.
    if let Some(ref tx) = *state.gateway_tx.read().await {
        let json = crate::routes::spaces::channel_row_to_json_pub(&state.db, &channel).await;
        let event = serde_json::json!({
            "op": 0,
            "type": "channel.create",
            "data": json,
        });
        let _ = tx.send(crate::gateway::events::GatewayBroadcast {
            space_id: channel.space_id.clone(),
            target_user_ids: None,
            event,
            intent: "channels".to_string(),
        });
    }

    Ok(serde_json::json!({
        "id": channel.id,
        "name": channel.name,
        "type": channel.channel_type,
        "space_id": channel.space_id,
        "parent_id": channel.parent_id,
    })
    .to_string())
}

async fn tool_delete_channel(state: &AppState, args: &Value) -> Result<String, String> {
    let channel_id = require_str(args, "channel_id")?;

    // Look up the channel before deleting so we know which space to broadcast to.
    let existing = db::channels::get_channel_row(&state.db, channel_id)
        .await
        .map_err(map_err)?;

    if let Some(ref space_id) = existing.space_id {
        if let Some(ref tx) = *state.gateway_tx.read().await {
            let event = serde_json::json!({
                "op": 0,
                "type": "channel.delete",
                "data": { "id": channel_id, "space_id": space_id },
            });
            let _ = tx.send(crate::gateway::events::GatewayBroadcast {
                space_id: Some(space_id.clone()),
                target_user_ids: None,
                event,
                intent: "channels".to_string(),
            });
        }
    }

    db::channels::delete_channel(&state.db, channel_id)
        .await
        .map_err(map_err)?;
    Ok(format!("Channel {channel_id} deleted"))
}

async fn tool_kick_member(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let user_id = require_str(args, "user_id")?;
    db::members::remove_member(&state.db, space_id, user_id)
        .await
        .map_err(map_err)?;
    Ok(format!("User {user_id} kicked from space {space_id}"))
}

async fn tool_ban_user(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let user_id = require_str(args, "user_id")?;
    let reason = opt_str(args, "reason");

    // Remove membership first, then ban (attributed to the System user)
    let system_user_id = db::users::get_or_create_system_user(&state.db)
        .await
        .map_err(map_err)?;
    let _ = db::members::remove_member(&state.db, space_id, user_id).await;
    let ban = db::bans::create_ban(
        &state.db,
        space_id,
        user_id,
        reason,
        &system_user_id,
        state.db_is_postgres,
    )
    .await
    .map_err(map_err)?;

    Ok(serde_json::json!({
        "user_id": ban.user_id,
        "space_id": ban.space_id,
        "reason": ban.reason,
        "created_at": ban.created_at,
    })
    .to_string())
}

async fn tool_unban_user(state: &AppState, args: &Value) -> Result<String, String> {
    let space_id = require_str(args, "space_id")?;
    let user_id = require_str(args, "user_id")?;
    db::bans::delete_ban(&state.db, space_id, user_id)
        .await
        .map_err(map_err)?;
    Ok(format!("User {user_id} unbanned from space {space_id}"))
}

async fn tool_delete_message(state: &AppState, args: &Value) -> Result<String, String> {
    let message_id = require_str(args, "message_id")?;
    db::messages::delete_message(&state.db, message_id)
        .await
        .map_err(map_err)?;
    Ok(format!("Message {message_id} deleted"))
}

async fn tool_server_info(state: &AppState) -> Result<String, String> {
    let spaces = db::admin::list_all_spaces(&state.db, None, 1000, None)
        .await
        .map_err(map_err)?;

    let online_users = state.presences.len();
    let voice_connections = state.voice_states.len();
    let voice_backend = if state.livekit_client.is_some() {
        "livekit"
    } else {
        "none"
    };

    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "spaces_count": spaces.len(),
        "online_users": online_users,
        "voice_connections": voice_connections,
        "voice_backend": voice_backend,
    })
    .to_string())
}
