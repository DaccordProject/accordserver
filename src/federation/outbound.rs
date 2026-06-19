//! Builds and enqueues outbound federation events for **locally-homed**
//! resources, where this server is authoritative.
//!
//! Only called from local write paths (S7: never from the inbound applier).
//! Local bare IDs are qualified with our domain at this boundary so peers
//! receive globally-unambiguous qualified IDs.

use crate::error::AppError;
use crate::federation::{mapping, mapping::FederationEnvelope, sender};
use crate::models::message::MessageRow;
use crate::models::user::User;
use crate::state::AppState;

/// Fan an event out to every peer with a member in a **locally-homed** space.
///
/// `local_space_id` is our bare space ID. `payload` should already have its IDs
/// qualified to our domain. No-op when federation is disabled, the space is
/// remote-homed (we are not authoritative — the forward path handles that), or
/// no peer is interested. Callers invoke this only from local write paths
/// (S7: never from the inbound applier).
pub async fn fanout_to_space(
    state: &AppState,
    local_space_id: &str,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<(), AppError> {
    let targets = crate::db::federation::interested_servers(&state.db, local_space_id).await?;
    fanout_to_targets(state, local_space_id, event_type, payload, &targets).await
}

/// Like [`fanout_to_space`] but with a precomputed target set. Use this when the
/// membership that determines interest is about to change — e.g. member leave or
/// kick removes the row *before* fanout, so the departing member's home server
/// must be captured via [`crate::db::federation::interested_servers`] first or it
/// would be dropped from the target set and never learn of the departure.
pub async fn fanout_to_targets(
    state: &AppState,
    local_space_id: &str,
    event_type: &str,
    payload: serde_json::Value,
    targets: &[String],
) -> Result<(), AppError> {
    let Some(fed) = state.federation.as_ref() else {
        return Ok(());
    };
    if targets.is_empty() {
        return Ok(());
    }
    // Only the home server is authoritative for fanout; remote-homed spaces are
    // handled by the forward path instead.
    if crate::db::federation::space_origin(&state.db, local_space_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let envelope = FederationEnvelope::new(
        crate::snowflake::generate(),
        fed.domain.clone(),
        Some(mapping::qualify(local_space_id, &fed.domain)),
        event_type,
        payload,
    );
    sender::enqueue(state, &envelope, targets).await
}

/// Fan a locally-created message out to interested peers.
pub async fn fanout_message_create(state: &AppState, msg: &MessageRow) -> Result<(), AppError> {
    let Some(fed) = state.federation.as_ref() else {
        return Ok(());
    };
    let Some(space_id) = msg.space_id.as_deref() else {
        return Ok(());
    };
    let author = crate::db::users::get_user(&state.db, &msg.author_id).await?;
    let payload = message_payload(&fed.domain, msg, &author);
    fanout_to_space(state, space_id, "m.message.create", payload).await
}

/// `m.member.join` payload with the joining user's profile, qualified to `domain`.
pub fn member_join_payload(domain: &str, user: &User) -> serde_json::Value {
    serde_json::json!({
        "user": {
            "id": mapping::qualify(&user.id, domain),
            "username": user.username,
            "display_name": user.display_name,
            "avatar": user.avatar,
        }
    })
}

/// `m.member.leave` payload, qualifying the leaving user's ID to `domain`.
pub fn member_leave_payload(domain: &str, user_id: &str) -> serde_json::Value {
    serde_json::json!({ "user_id": mapping::qualify(user_id, domain) })
}

/// Qualify a reaction payload to `domain` for fanout/forward.
pub fn reaction_payload(
    domain: &str,
    channel_id: &str,
    message_id: &str,
    user_id: &str,
    emoji: &str,
) -> serde_json::Value {
    let q = |id: &str| mapping::qualify(id, domain);
    serde_json::json!({
        "channel_id": q(channel_id),
        "message_id": q(message_id),
        "user_id": q(user_id),
        "emoji": emoji,
    })
}

/// The `m.message.create` payload (message object) with all IDs qualified to
/// `domain`. Shared by fanout and the remote-homed `/send` response so peers and
/// the originating client see an identical, qualified message.
pub fn message_payload(domain: &str, msg: &MessageRow, author: &User) -> serde_json::Value {
    let q = |id: &str| mapping::qualify(id, domain);

    let mentions: Vec<String> = serde_json::from_str(&msg.mentions).unwrap_or_default();
    let qualified_mentions: Vec<String> = mentions.iter().map(|m| q(m)).collect();
    let embeds: serde_json::Value =
        serde_json::from_str(&msg.embeds).unwrap_or(serde_json::Value::Array(vec![]));

    serde_json::json!({
        "id": q(&msg.id),
        "channel_id": q(&msg.channel_id),
        "space_id": msg.space_id.as_deref().map(q),
        "author": {
            "id": q(&author.id),
            "username": author.username,
            "display_name": author.display_name,
            "avatar": author.avatar,
        },
        "content": msg.content,
        "mentions": qualified_mentions,
        "mention_everyone": msg.mention_everyone,
        "embeds": embeds,
        "reply_to": msg.reply_to.as_deref().map(q),
        "created_at": msg.created_at,
    })
}
