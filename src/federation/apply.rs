//! Applies inbound federation content events to the local replica and injects
//! them into the gateway.
//!
//! Loop prevention (S7): appliers inject into the local gateway **only** and
//! never enqueue outbound fanout — events ping-pong otherwise. Every applier
//! re-checks authority (S1) and input caps (S6) before any write, and only ever
//! writes rows marked `origin = <peer>` (S2).

use serde::Deserialize;

use crate::error::AppError;
use crate::federation::{authority, mapping::FederationEnvelope};
use crate::state::AppState;

/// Same caps as local message creation (`routes/messages.rs`).
const MAX_CONTENT_CHARS: usize = 4000;
const MAX_EMBEDS: usize = 10;
const MAX_MENTIONS: usize = 100;

/// Outcome of applying an event, mapped to an HTTP status by the inbox.
pub enum Applied {
    /// Applied (or a harmless duplicate / not-participating no-op): ack 200.
    Ok,
    /// Event type not handled yet: 501.
    Unsupported,
}

pub async fn apply_event(
    state: &AppState,
    peer: &str,
    env: &FederationEnvelope,
) -> Result<Applied, AppError> {
    match env.event_type.as_str() {
        "m.message.create" => {
            apply_message_create(state, peer, env).await?;
            Ok(Applied::Ok)
        }
        "m.message.update" => {
            apply_message_update(state, peer, env).await?;
            Ok(Applied::Ok)
        }
        "m.message.delete" => {
            apply_message_delete(state, peer, env).await?;
            Ok(Applied::Ok)
        }
        "m.reaction.add" => {
            apply_reaction(state, peer, env, true).await?;
            Ok(Applied::Ok)
        }
        "m.reaction.remove" => {
            apply_reaction(state, peer, env, false).await?;
            Ok(Applied::Ok)
        }
        "m.member.join" => {
            apply_member_join(state, peer, env).await?;
            Ok(Applied::Ok)
        }
        "m.member.leave" => {
            apply_member_leave(state, peer, env).await?;
            Ok(Applied::Ok)
        }
        "m.typing" => {
            apply_typing(state, env).await;
            Ok(Applied::Ok)
        }
        _ => Ok(Applied::Unsupported),
    }
}

/// Ephemeral typing indicator: just rebroadcast to local sessions (no DB).
async fn apply_typing(state: &AppState, env: &FederationEnvelope) {
    rebroadcast(
        state,
        env.space_id.clone(),
        "typing.start",
        env.payload.clone(),
        "message_typing",
    )
    .await;
}

/// Re-broadcast a relayed event to local gateway sessions for `space_id`.
async fn rebroadcast(
    state: &AppState,
    space_id: Option<String>,
    event_type: &str,
    data: serde_json::Value,
    intent: &str,
) {
    if let Some(dispatcher) = state.gateway_tx.read().await.as_ref() {
        let event = serde_json::json!({ "op": 0, "type": event_type, "data": data });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id,
            target_user_ids: None,
            event,
            intent: intent.to_string(),
        });
    }
}

#[derive(Deserialize)]
struct RemoteAuthor {
    id: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
}

#[derive(Deserialize)]
struct RemoteMessagePayload {
    id: String,
    channel_id: String,
    #[serde(default)]
    space_id: Option<String>,
    author: RemoteAuthor,
    content: String,
    #[serde(default)]
    mentions: Vec<String>,
    #[serde(default)]
    mention_everyone: bool,
    #[serde(default)]
    embeds: Vec<serde_json::Value>,
    #[serde(default)]
    reply_to: Option<String>,
    created_at: String,
}

async fn apply_message_create(
    state: &AppState,
    peer: &str,
    env: &FederationEnvelope,
) -> Result<(), AppError> {
    let payload: RemoteMessagePayload = serde_json::from_value(env.payload.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid message payload: {e}")))?;

    // Authority (S1): the message, its channel, and its space must be homed on
    // the signing peer — the peer is authoritative for the space, so it owns the
    // message stream. The *author* may be homed elsewhere (the home server
    // relays messages from members on other servers), so its origin is taken
    // from its own qualified ID rather than bound to the signer.
    authority::require_homed_on(&payload.id, peer, "message")?;
    authority::require_homed_on(&payload.channel_id, peer, "channel")?;
    if let Some(sid) = &payload.space_id {
        authority::require_homed_on(sid, peer, "space")?;
    }
    // The author may be homed on any server, but MUST be a qualified remote ID
    // so it can never collide with / overwrite a local (bare-ID) user row (S2).
    authority::require_remote_target(&payload.author.id)?;
    let author_domain = crate::federation::mapping::domain_of(&payload.author.id).unwrap_or(peer);

    // Input caps (S6): never trust remote-supplied sizes.
    if payload.content.chars().count() > MAX_CONTENT_CHARS {
        return Err(AppError::BadRequest(
            "remote message content too long".to_string(),
        ));
    }
    if payload.embeds.len() > MAX_EMBEDS {
        return Err(AppError::BadRequest("too many embeds".to_string()));
    }
    if payload.mentions.len() > MAX_MENTIONS {
        return Err(AppError::BadRequest("too many mentions".to_string()));
    }

    // We must already mirror this channel (i.e. one of our users joined the
    // space). If not, we are not a participant — acknowledge and ignore.
    if crate::db::channels::get_channel_row(&state.db, &payload.channel_id)
        .await
        .is_err()
    {
        tracing::debug!(
            "ignoring federated message for unmirrored channel {}",
            payload.channel_id
        );
        return Ok(());
    }

    // Cache the remote author's profile under its own home domain.
    let handle = match &payload.author.username {
        Some(name) => crate::federation::mapping::handle(name, author_domain),
        None => payload.author.id.clone(),
    };
    crate::db::users::upsert_remote_user(
        &state.db,
        &payload.author.id,
        author_domain,
        &handle,
        payload.author.display_name.as_deref(),
        payload.author.avatar.as_deref(),
    )
    .await?;

    // Store the replica message (idempotent on the qualified ID).
    let mentions_json =
        serde_json::to_string(&payload.mentions).unwrap_or_else(|_| "[]".to_string());
    let embeds_json = serde_json::to_string(&payload.embeds).unwrap_or_else(|_| "[]".to_string());
    let insert = crate::db::messages::RemoteMessageInsert {
        id: &payload.id,
        channel_id: &payload.channel_id,
        space_id: payload.space_id.as_deref(),
        author_id: &payload.author.id,
        content: &payload.content,
        created_at: &payload.created_at,
        mention_everyone: payload.mention_everyone,
        mentions_json: &mentions_json,
        embeds_json: &embeds_json,
        reply_to: payload.reply_to.as_deref(),
        origin: peer,
    };

    let Some(row) = crate::db::messages::insert_remote_message(&state.db, &insert).await? else {
        // Duplicate delivery: already stored and broadcast.
        return Ok(());
    };

    // Inject into the local gateway at the same seam local writes use. This is
    // delivery-only: it MUST NOT trigger outbound fanout (S7).
    let json = crate::routes::messages::message_row_to_json_with_attachments(&row, &[], None);
    if let Some(dispatcher) = state.gateway_tx.read().await.as_ref() {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.create",
            "data": json,
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: payload.space_id.clone(),
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }

    Ok(())
}

#[derive(Deserialize)]
struct RemoteMessageEdit {
    id: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    edited_at: Option<String>,
}

async fn apply_message_update(
    state: &AppState,
    peer: &str,
    env: &FederationEnvelope,
) -> Result<(), AppError> {
    let payload: RemoteMessageEdit = serde_json::from_value(env.payload.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid update payload: {e}")))?;
    authority::require_homed_on(&payload.id, peer, "message")?;

    if let Some(content) = &payload.content {
        if content.chars().count() > MAX_CONTENT_CHARS {
            return Err(AppError::BadRequest(
                "remote message content too long".to_string(),
            ));
        }
    }
    // Only touch a replica row homed on this peer (S2).
    let Ok(existing) = crate::db::messages::get_message_row(&state.db, &payload.id).await else {
        return Ok(());
    };
    if existing.origin.as_deref() != Some(peer) {
        return Ok(());
    }
    crate::db::messages::edit_remote_message(
        &state.db,
        &payload.id,
        payload.content.as_deref(),
        payload.edited_at.as_deref(),
    )
    .await?;

    if let Ok(row) = crate::db::messages::get_message_row(&state.db, &payload.id).await {
        let json = crate::routes::messages::message_row_to_json_with_attachments(&row, &[], None);
        rebroadcast(
            state,
            row.space_id.clone(),
            "message.update",
            json,
            "messages",
        )
        .await;
    }
    Ok(())
}

#[derive(Deserialize)]
struct RemoteMessageDelete {
    id: String,
    #[serde(default)]
    channel_id: Option<String>,
}

async fn apply_message_delete(
    state: &AppState,
    peer: &str,
    env: &FederationEnvelope,
) -> Result<(), AppError> {
    let payload: RemoteMessageDelete = serde_json::from_value(env.payload.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid delete payload: {e}")))?;
    authority::require_homed_on(&payload.id, peer, "message")?;

    let Ok(existing) = crate::db::messages::get_message_row(&state.db, &payload.id).await else {
        return Ok(());
    };
    if existing.origin.as_deref() != Some(peer) {
        return Ok(());
    }
    let channel_id = payload
        .channel_id
        .clone()
        .unwrap_or(existing.channel_id.clone());
    crate::db::messages::delete_message(&state.db, &payload.id).await?;

    rebroadcast(
        state,
        existing.space_id.clone(),
        "message.delete",
        serde_json::json!({ "id": payload.id, "channel_id": channel_id }),
        "messages",
    )
    .await;
    Ok(())
}

#[derive(Deserialize)]
struct RemoteMemberJoin {
    user: crate::federation::handshake::RemoteUserRef,
}

async fn apply_member_join(
    state: &AppState,
    peer: &str,
    env: &FederationEnvelope,
) -> Result<(), AppError> {
    let Some(space_id) = env.space_id.clone() else {
        return Err(AppError::BadRequest(
            "member event missing space_id".to_string(),
        ));
    };
    let payload: RemoteMemberJoin = serde_json::from_value(env.payload.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid member.join payload: {e}")))?;

    // Only act if we mirror this space (the envelope's space_id was already
    // bound to the signing peer by authority::check).
    if crate::db::spaces::get_space_row(&state.db, &space_id)
        .await
        .is_err()
    {
        return Ok(());
    }

    // The joining member must be a qualified remote ID (never a bare local ID — S2).
    authority::require_remote_target(&payload.user.id)?;
    let domain = crate::federation::mapping::domain_of(&payload.user.id).unwrap_or(peer);
    crate::db::users::upsert_remote_user(
        &state.db,
        &payload.user.id,
        domain,
        &crate::federation::mapping::handle(&payload.user.username, domain),
        payload.user.display_name.as_deref(),
        payload.user.avatar.as_deref(),
    )
    .await?;
    crate::db::federation::add_member_with_origin(
        &state.db,
        &space_id,
        &payload.user.id,
        Some(domain),
    )
    .await?;

    rebroadcast(
        state,
        Some(space_id.clone()),
        "member.join",
        serde_json::json!({ "space_id": space_id, "user_id": payload.user.id }),
        "members",
    )
    .await;
    Ok(())
}

#[derive(Deserialize)]
struct RemoteMemberLeave {
    user_id: String,
}

async fn apply_member_leave(
    state: &AppState,
    _peer: &str,
    env: &FederationEnvelope,
) -> Result<(), AppError> {
    let Some(space_id) = env.space_id.clone() else {
        return Err(AppError::BadRequest(
            "member event missing space_id".to_string(),
        ));
    };
    let payload: RemoteMemberLeave = serde_json::from_value(env.payload.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid member.leave payload: {e}")))?;

    if crate::db::spaces::get_space_row(&state.db, &space_id)
        .await
        .is_err()
    {
        return Ok(());
    }
    crate::db::members::remove_member(&state.db, &space_id, &payload.user_id).await?;

    rebroadcast(
        state,
        Some(space_id.clone()),
        "member.leave",
        serde_json::json!({ "space_id": space_id, "user_id": payload.user_id }),
        "members",
    )
    .await;
    Ok(())
}

#[derive(Deserialize)]
struct RemoteReaction {
    channel_id: String,
    message_id: String,
    user_id: String,
    emoji: String,
}

async fn apply_reaction(
    state: &AppState,
    peer: &str,
    env: &FederationEnvelope,
    add: bool,
) -> Result<(), AppError> {
    let payload: RemoteReaction = serde_json::from_value(env.payload.clone())
        .map_err(|e| AppError::BadRequest(format!("invalid reaction payload: {e}")))?;
    // The message/channel belong to the peer's space; the reactor may be remote
    // but MUST be a qualified remote ID (never a bare local ID — S2).
    authority::require_homed_on(&payload.message_id, peer, "message")?;
    authority::require_homed_on(&payload.channel_id, peer, "channel")?;
    authority::require_remote_target(&payload.user_id)?;

    // Must mirror the message; otherwise ignore.
    let Ok(msg) = crate::db::messages::get_message_row(&state.db, &payload.message_id).await else {
        return Ok(());
    };

    // Ensure the reactor exists locally (FK), under its own home domain.
    let reactor_domain = crate::federation::mapping::domain_of(&payload.user_id).unwrap_or(peer);
    crate::db::users::upsert_remote_user(
        &state.db,
        &payload.user_id,
        reactor_domain,
        &payload.user_id,
        None,
        None,
    )
    .await?;

    if add {
        crate::db::messages::add_reaction(
            &state.db,
            &payload.message_id,
            &payload.user_id,
            &payload.emoji,
        )
        .await?;
    } else {
        crate::db::messages::remove_reaction(
            &state.db,
            &payload.message_id,
            &payload.user_id,
            &payload.emoji,
        )
        .await?;
    }

    let event_type = if add {
        "reaction.add"
    } else {
        "reaction.remove"
    };
    rebroadcast(
        state,
        msg.space_id.clone(),
        event_type,
        serde_json::json!({
            "channel_id": payload.channel_id,
            "message_id": payload.message_id,
            "user_id": payload.user_id,
            "emoji": payload.emoji,
        }),
        "message_reactions",
    )
    .await;
    Ok(())
}
