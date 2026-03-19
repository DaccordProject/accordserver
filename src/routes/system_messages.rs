use crate::db;
use crate::gateway::events::GatewayBroadcast;
use crate::routes::messages::message_row_to_json;
use crate::state::AppState;

/// If the space has a `system_channel_id`, creates a "member_join" system message
/// authored by the joining user and broadcasts it via the gateway.
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

    let username = user
        .display_name
        .as_deref()
        .unwrap_or(&user.username);

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
