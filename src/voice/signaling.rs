use crate::gateway::events::GatewayBroadcast;
use crate::state::AppState;
use crate::voice::state::get_user_voice_state;

/// Relay a WebRTC signaling message from a user to their voice channel's space.
pub async fn relay_signal(
    state: &AppState,
    user_id: &str,
    session_id: &str,
    signal_type: &str,
    payload: &serde_json::Value,
) {
    let voice_state = match get_user_voice_state(state, user_id) {
        Some(vs) => vs,
        None => return, // User not in voice — drop silently
    };

    let space_id = match voice_state.space_id {
        Some(ref sid) => sid.clone(),
        None => return,
    };

    let event = serde_json::json!({
        "op": 0,
        "type": "voice.signal",
        "data": {
            "user_id": user_id,
            "session_id": session_id,
            "channel_id": voice_state.channel_id,
            "type": signal_type,
            "payload": payload
        }
    });

    if let Some(ref tx) = *state.gateway_tx.read().await {
        let _ = tx.send(GatewayBroadcast {
            space_id: Some(space_id),
            target_user_ids: None,
            event,
            intent: "voice_states".to_string(),
        });
    }
}
