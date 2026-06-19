use axum::extract::{Path, State};
use axum::Json;

use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{
    require_channel_membership, require_channel_permission, require_not_timed_out,
};
use crate::state::AppState;

/// Convert the space_id string returned by permission helpers into the
/// `Option<String>` that `GatewayBroadcast` expects.  DM channels return an
/// empty string from `require_channel_permission`, which maps to `None`.
fn space_id_opt(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub async fn add_reaction(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_id =
        require_channel_permission(&state.db, &channel_id, &auth, "add_reactions").await?;
    // Block timed-out members from reacting in a space (DMs have no timeout).
    if !space_id.is_empty() {
        require_not_timed_out(&state.db, &space_id, &auth).await?;
    }

    // Remote-homed space: forward to the authoritative home server. The reaction
    // returns to us via the home's fanout.
    if !space_id.is_empty() {
        if let Some(home) = crate::db::federation::space_origin(&state.db, &space_id).await? {
            let actor = crate::db::users::get_user(&state.db, &auth.user_id).await?;
            crate::federation::forward::forward_reaction(
                &state,
                &home,
                &channel_id,
                &message_id,
                &actor,
                &emoji,
                false,
            )
            .await?;
            return Ok(Json(serde_json::json!({ "data": null })));
        }
    }

    let sql = if state.db_is_postgres {
        "INSERT INTO reactions (message_id, user_id, emoji_name) VALUES (?, ?, ?) ON CONFLICT DO NOTHING"
    } else {
        "INSERT OR IGNORE INTO reactions (message_id, user_id, emoji_name) VALUES (?, ?, ?)"
    };
    sqlx::query(&crate::db::q(sql))
        .bind(&message_id)
        .bind(&auth.user_id)
        .bind(&emoji)
        .execute(&state.db)
        .await
        .map_err(crate::error::AppError::from)?;

    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "reaction.add",
            "data": {
                "channel_id": channel_id,
                "message_id": message_id,
                "user_id": auth.user_id,
                "emoji": emoji,
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: space_id_opt(space_id.clone()),
            target_user_ids: None,
            event,
            intent: "message_reactions".to_string(),
        });
    }

    // Fan out to interested peers for a locally-homed space.
    if let Some(fed) = state.federation.as_ref() {
        if !space_id.is_empty() {
            let payload = crate::federation::outbound::reaction_payload(
                &fed.domain,
                &channel_id,
                &message_id,
                &auth.user_id,
                &emoji,
            );
            let _ = crate::federation::outbound::fanout_to_space(
                &state,
                &space_id,
                "m.reaction.add",
                payload,
            )
            .await;
        }
    }

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_own_reaction(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_id = require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;

    // Remote-homed space: forward the removal to the home authority.
    if !space_id.is_empty() {
        if let Some(home) = crate::db::federation::space_origin(&state.db, &space_id).await? {
            let actor = crate::db::users::get_user(&state.db, &auth.user_id).await?;
            crate::federation::forward::forward_reaction(
                &state,
                &home,
                &channel_id,
                &message_id,
                &actor,
                &emoji,
                true,
            )
            .await?;
            return Ok(Json(serde_json::json!({ "data": null })));
        }
    }

    sqlx::query(&crate::db::q(
        "DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji_name = ?",
    ))
    .bind(&message_id)
    .bind(&auth.user_id)
    .bind(&emoji)
    .execute(&state.db)
    .await?;

    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "reaction.remove",
            "data": {
                "channel_id": channel_id,
                "message_id": message_id,
                "user_id": auth.user_id,
                "emoji": emoji,
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: space_id_opt(space_id.clone()),
            target_user_ids: None,
            event,
            intent: "message_reactions".to_string(),
        });
    }

    if let Some(fed) = state.federation.as_ref() {
        if !space_id.is_empty() {
            let payload = crate::federation::outbound::reaction_payload(
                &fed.domain,
                &channel_id,
                &message_id,
                &auth.user_id,
                &emoji,
            );
            let _ = crate::federation::outbound::fanout_to_space(
                &state,
                &space_id,
                "m.reaction.remove",
                payload,
            )
            .await;
        }
    }

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_user_reaction(
    state: State<AppState>,
    Path((channel_id, message_id, emoji, user_id)): Path<(String, String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_id =
        require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    sqlx::query(&crate::db::q(
        "DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji_name = ?",
    ))
    .bind(&message_id)
    .bind(&user_id)
    .bind(&emoji)
    .execute(&state.db)
    .await?;

    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "reaction.remove",
            "data": {
                "channel_id": channel_id,
                "message_id": message_id,
                "user_id": user_id,
                "emoji": emoji,
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: space_id_opt(space_id),
            target_user_ids: None,
            event,
            intent: "message_reactions".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn list_reactions(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let users = sqlx::query_as::<_, (String,)>(&crate::db::q(
        "SELECT user_id FROM reactions WHERE message_id = ? AND emoji_name = ?",
    ))
    .bind(&message_id)
    .bind(&emoji)
    .fetch_all(&state.db)
    .await?;

    let user_ids: Vec<String> = users.into_iter().map(|r| r.0).collect();
    Ok(Json(serde_json::json!({ "data": user_ids })))
}

pub async fn remove_all_reactions(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_id =
        require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    sqlx::query(&crate::db::q("DELETE FROM reactions WHERE message_id = ?"))
        .bind(&message_id)
        .execute(&state.db)
        .await?;

    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "reaction.clear",
            "data": {
                "channel_id": channel_id,
                "message_id": message_id,
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: space_id_opt(space_id),
            target_user_ids: None,
            event,
            intent: "message_reactions".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_all_reactions_emoji(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let space_id =
        require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    sqlx::query(&crate::db::q(
        "DELETE FROM reactions WHERE message_id = ? AND emoji_name = ?",
    ))
    .bind(&message_id)
    .bind(&emoji)
    .execute(&state.db)
    .await?;

    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "reaction.clear_emoji",
            "data": {
                "channel_id": channel_id,
                "message_id": message_id,
                "emoji": emoji,
            }
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: space_id_opt(space_id),
            target_user_ids: None,
            event,
            intent: "message_reactions".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": null })))
}
