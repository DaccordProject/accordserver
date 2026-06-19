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

/// Fan a locally-created message out to every peer with a member in its space.
///
/// No-op when federation is disabled, the message has no space (DM), the space
/// is remote-homed (we are not authoritative — the forward path handles that),
/// or no peer is interested.
pub async fn fanout_message_create(state: &AppState, msg: &MessageRow) -> Result<(), AppError> {
    let Some(fed) = state.federation.as_ref() else {
        return Ok(());
    };
    let Some(space_id) = msg.space_id.as_deref() else {
        return Ok(());
    };

    // Only fan out for spaces we home (origin IS NULL). Remote-homed spaces are
    // handled by the forward path, and fanning out from a replica would loop.
    if crate::db::federation::space_origin(&state.db, space_id)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let targets = crate::db::federation::interested_servers(&state.db, space_id).await?;
    if targets.is_empty() {
        return Ok(());
    }

    let author = crate::db::users::get_user(&state.db, &msg.author_id).await?;
    let envelope = build_message_create(&fed.domain, msg, &author);
    sender::enqueue(state, &envelope, &targets).await
}

/// Construct an `m.message.create` envelope with all IDs qualified to `domain`.
fn build_message_create(domain: &str, msg: &MessageRow, author: &User) -> FederationEnvelope {
    let q = |id: &str| mapping::qualify(id, domain);

    let mentions: Vec<String> = serde_json::from_str(&msg.mentions).unwrap_or_default();
    let qualified_mentions: Vec<String> = mentions.iter().map(|m| q(m)).collect();
    let embeds: serde_json::Value =
        serde_json::from_str(&msg.embeds).unwrap_or(serde_json::Value::Array(vec![]));
    let space_id = msg.space_id.as_deref().map(q);

    let payload = serde_json::json!({
        "id": q(&msg.id),
        "channel_id": q(&msg.channel_id),
        "space_id": space_id,
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
    });

    FederationEnvelope::new(
        q(&msg.id),
        domain.to_string(),
        msg.space_id.as_deref().map(q),
        "m.message.create",
        payload,
    )
}
