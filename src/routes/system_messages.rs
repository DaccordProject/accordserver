use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::routes::messages::message_row_to_json;
use crate::state::AppState;

/// If the space has a `system_channel_id`, creates a "member_join" system message
/// authored by the joining user and broadcasts it via the gateway.
///
/// Only the first join per (space, user) ever produces an introduction message.
/// Subsequent joins (after leaving, getting kicked, etc.) are silent — the
/// introduction is a one-time event per account.
///
/// Failures are logged but do not propagate — a failed welcome message should
/// never block the join itself.
pub async fn broadcast_member_join_message(state: &AppState, space_id: &str, user_id: &str) {
    let space = match db::spaces::get_space_row(&state.db, space_id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "system message: failed to look up space {}: {:?}",
                space_id,
                e
            );
            return;
        }
    };

    let system_channel_id = match space.system_channel_id {
        Some(ref id) if !id.is_empty() => id.clone(),
        _ => return, // No system channel configured — nothing to do
    };

    // Atomically claim the "has been introduced" slot for this (space, user).
    // If the row already exists, this user has been welcomed before and we
    // must not post another introduction.
    match claim_introduction(state, space_id, user_id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(
                "system message: user {} has already been introduced in space {}, skipping",
                user_id,
                space_id
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                "system message: failed to record introduction for user {} in space {}: {:?}",
                user_id,
                space_id,
                e
            );
            return;
        }
    }

    let user = match db::users::get_user(&state.db, user_id).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(
                "system message: failed to look up user {}: {:?}",
                user_id,
                e
            );
            return;
        }
    };

    let username = user.display_name.as_deref().unwrap_or(&user.username);

    let content = format!("{} joined the server.", username);

    let msg = match db::messages::create_system_message(
        &state.db,
        &system_channel_id,
        user_id,
        space_id,
        &content,
        "member_join",
    )
    .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "system message: failed to create member_join message in channel {}: {:?}",
                system_channel_id,
                e
            );
            return;
        }
    };

    let json = message_row_to_json(&msg);

    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "message.create",
            "data": json
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(space_id.to_string()),
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }
}

/// Records that the given user has now received their introduction message in
/// the given space. Returns `Ok(true)` if this call inserted a new row (the
/// caller should post the welcome), `Ok(false)` if a row already existed
/// (welcome should be skipped).
async fn claim_introduction(
    state: &AppState,
    space_id: &str,
    user_id: &str,
) -> Result<bool, AppError> {
    let sql = if state.db_is_postgres {
        "INSERT INTO space_introductions (space_id, user_id) VALUES (?, ?) ON CONFLICT DO NOTHING"
    } else {
        "INSERT OR IGNORE INTO space_introductions (space_id, user_id) VALUES (?, ?)"
    };
    let result = sqlx::query(&db::q(sql))
        .bind(space_id)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    Ok(result.rows_affected() > 0)
}
