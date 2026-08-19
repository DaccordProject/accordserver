use axum::extract::{Path, State};
use axum::Json;
use chrono::Utc;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_hierarchy, require_permission};
use crate::state::AppState;
use crate::storage;

/// Upper bound on a ban's message purge window, matching Discord's: a
/// moderator can wipe at most the last 7 days of the banned user's messages.
/// Larger values are clamped rather than rejected, so a client that offers a
/// coarser set of options can't fail the ban itself.
const MAX_DELETE_MESSAGE_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Deserialize)]
pub struct CreateBanBody {
    pub reason: Option<String>,

    /// Also delete the banned user's messages in this space from the last N
    /// seconds. Absent or 0 keeps their history, which stays the default.
    pub delete_message_seconds: Option<i64>,
}

pub async fn list_bans(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    let bans = db::bans::list_bans(&state.db, &space_id).await?;
    let data: Vec<serde_json::Value> = bans
        .iter()
        .map(|b| {
            serde_json::json!({
                "user_id": b.user_id,
                "space_id": b.space_id,
                "reason": b.reason,
                "banned_by": b.banned_by,
                "created_at": b.created_at
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

pub async fn get_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    let ban = db::bans::get_ban(&state.db, &space_id, &user_id).await?;
    Ok(Json(serde_json::json!({
        "data": {
            "user_id": ban.user_id,
            "space_id": ban.space_id,
            "reason": ban.reason,
            "banned_by": ban.banned_by,
            "created_at": ban.created_at
        }
    })))
}

pub async fn create_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
    body: Option<Json<CreateBanBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    require_hierarchy(&state.db, &space_id, &auth, &user_id).await?;
    let body = body.map(|Json(b)| b);
    let reason = body.as_ref().and_then(|b| b.reason.clone());
    let delete_message_seconds = body
        .as_ref()
        .and_then(|b| b.delete_message_seconds)
        .unwrap_or(0)
        .clamp(0, MAX_DELETE_MESSAGE_SECONDS);
    let ban = db::bans::create_ban(
        &state.db,
        &space_id,
        &user_id,
        reason.as_deref(),
        &auth.user_id,
        state.db_is_postgres,
    )
    .await?;

    // Optional message purge. Runs after the ban so a failure here can't leave
    // the user's history deleted but the user still in the space, and only
    // needs `ban_members`: wiping a banned user's posts is part of the ban, not
    // a separate `manage_messages` action.
    let purged = purge_recent_messages(&state, &space_id, &user_id, delete_message_seconds).await?;

    // `create_ban` deletes the member row, so this is a membership change and
    // has to be announced like one. Without it the banned user's gateway
    // session keeps its stale space set and carries on receiving the space
    // until it happens to reconnect, and everyone else's roster keeps showing
    // them. Kicks already do this (routes/members.rs); bans didn't.
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        // member.leave first: the banned user's own session drops the space on
        // this event (see gateway::Delivery::RefreshSpaces), so anything sent
        // after it no longer reaches them.
        let leave = serde_json::json!({
            "op": 0,
            "type": "member.leave",
            "data": { "space_id": space_id, "user_id": user_id }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event: leave,
            intent: "members".to_string(),
        });

        // And the moderation event the intent table already anticipates
        // (`intent_for_event` maps ban.create/ban.delete to "moderation"),
        // so moderation UIs can update live.
        let created = serde_json::json!({
            "op": 0,
            "type": "ban.create",
            "data": {
                "space_id": ban.space_id,
                "user_id": ban.user_id,
                "reason": ban.reason,
                "banned_by": ban.banned_by,
                "created_at": ban.created_at
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event: created,
            intent: "moderation".to_string(),
        });

        // One `message.delete` per purged message, so open clients drop them
        // through the same path a normal delete takes. There is no bulk-delete
        // gateway event to reuse.
        for (message_id, channel_id) in &purged {
            let event = serde_json::json!({
                "op": 0,
                "type": "message.delete",
                "data": {
                    "id": message_id,
                    "channel_id": channel_id,
                    "space_id": space_id,
                }
            });
            let _ = dispatcher.send(GatewayBroadcast {
                space_id: Some(space_id.clone()),
                target_user_ids: None,
                event,
                intent: "messages".to_string(),
            });
        }
    }

    // Fan the deletions out to federated peers, as `delete_message` does.
    if let Some(fed) = state.federation.as_ref() {
        for (message_id, channel_id) in &purged {
            let payload = serde_json::json!({
                "id": crate::federation::mapping::qualify(message_id, &fed.domain),
                "channel_id": crate::federation::mapping::qualify(channel_id, &fed.domain),
            });
            let _ = crate::federation::outbound::fanout_to_space(
                &state,
                &space_id,
                "m.message.delete",
                payload,
            )
            .await;
        }
    }

    Ok(Json(serde_json::json!({
        "data": {
            "user_id": ban.user_id,
            "space_id": ban.space_id,
            "reason": ban.reason,
            "banned_by": ban.banned_by,
            "created_at": ban.created_at,
            "deleted_message_count": purged.len()
        }
    })))
}

/// Deletes [user_id]'s messages in [space_id] from the last [seconds], newest
/// first, returning the `(message_id, channel_id)` pairs actually removed so
/// the caller can announce them.
///
/// A no-op for `seconds <= 0`. Each row goes through the same steps a single
/// delete does — unlink the attachment files, then drop the row — so a purge
/// can't leave orphaned uploads on disk.
async fn purge_recent_messages(
    state: &AppState,
    space_id: &str,
    user_id: &str,
    seconds: i64,
) -> Result<Vec<(String, String)>, AppError> {
    if seconds <= 0 {
        return Ok(Vec::new());
    }
    let cutoff = (Utc::now() - chrono::Duration::seconds(seconds))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let messages =
        db::messages::list_author_messages_in_space_since(&state.db, space_id, user_id, &cutoff)
            .await?;

    let mut deleted = Vec::with_capacity(messages.len());
    for (message_id, channel_id) in messages {
        let attachments =
            db::attachments::get_attachments_for_message(&state.db, &message_id).await?;
        for att in &attachments {
            let _ = storage::delete_file(&state.storage_path, &att.url).await;
        }
        db::messages::delete_message(&state.db, &message_id).await?;
        deleted.push((message_id, channel_id));
    }
    Ok(deleted)
}

pub async fn delete_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    require_hierarchy(&state.db, &space_id, &auth, &user_id).await?;
    db::bans::delete_ban(&state.db, &space_id, &user_id).await?;

    // Unbanning doesn't restore membership, so there's no member event here —
    // only the moderation one, so a moderation UI's ban list updates live.
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "ban.delete",
            "data": { "space_id": space_id, "user_id": user_id }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event,
            intent: "moderation".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}
