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
        _ => Ok(Applied::Unsupported),
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

    // Authority (S1): the message, its author, and its channel must all be
    // homed on the signing peer. (The envelope's space_id was already bound to
    // the peer by `authority::check`.)
    authority::require_homed_on(&payload.id, peer, "message")?;
    authority::require_homed_on(&payload.author.id, peer, "user")?;
    authority::require_homed_on(&payload.channel_id, peer, "channel")?;
    if let Some(sid) = &payload.space_id {
        authority::require_homed_on(sid, peer, "space")?;
    }

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

    // Cache the remote author's profile.
    let handle = match &payload.author.username {
        Some(name) => format!("{name}@{peer}"),
        None => payload.author.id.clone(),
    };
    crate::db::users::upsert_remote_user(
        &state.db,
        &payload.author.id,
        peer,
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
