pub mod dispatcher;
pub mod events;
pub mod heartbeat;
pub mod intents;
pub mod session;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use tokio::sync::mpsc;

use crate::config::VoiceBackend;
use crate::db;
use crate::middleware::auth as auth_resolve;
use crate::state::AppState;
use events::{
    GatewayBroadcast, GatewayMessage, IdentifyData, VoiceSignalData, VoiceStateUpdateData,
};
use heartbeat::{HEARTBEAT_INTERVAL, HEARTBEAT_TIMEOUT};
use session::GatewaySession;

pub async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Send HELLO
    let hello = serde_json::json!({
        "op": events::opcode::HELLO,
        "data": {
            "heartbeat_interval": HEARTBEAT_INTERVAL.as_millis() as u64
        }
    });
    if ws_sink
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Wait for IDENTIFY
    let session_id;
    let user_id;
    let user_intents: Vec<String>;
    let space_ids: HashSet<String>;

    // Channel for sending messages to this client
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Give client 30 seconds to identify
    let identify_timeout = tokio::time::sleep(std::time::Duration::from_secs(30));
    tokio::pin!(identify_timeout);

    loop {
        tokio::select! {
            _ = &mut identify_timeout => {
                let close = serde_json::json!({
                    "op": events::opcode::INVALID_SESSION,
                    "data": { "resumable": false }
                });
                let _ = ws_sink.send(Message::Text(close.to_string().into())).await;
                return;
            }
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(gw_msg) = serde_json::from_str::<GatewayMessage>(&text) {
                            if gw_msg.op == events::opcode::IDENTIFY {
                                if let Some(data) = gw_msg.data {
                                    if let Ok(identify) = serde_json::from_value::<IdentifyData>(data) {
                                        // Resolve token
                                        let auth_user = resolve_token(&state, &identify.token).await;
                                        match auth_user {
                                            Some(uid) => {
                                                user_id = uid;
                                                user_intents = identify.intents;
                                                session_id = crate::snowflake::generate();

                                                // Load user's space memberships
                                                space_ids = db::spaces::list_space_ids_for_user(&state.db, &user_id).await
                                                    .map(|sids| sids.into_iter().collect())
                                                    .unwrap_or_default();

                                                break;
                                            }
                                            None => {
                                                let close = serde_json::json!({
                                                    "op": events::opcode::INVALID_SESSION,
                                                    "data": { "resumable": false }
                                                });
                                                let _ = ws_sink.send(Message::Text(close.to_string().into())).await;
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    _ => {}
                }
            }
        }
    }

    // Send READY event
    let ready = serde_json::json!({
        "op": events::opcode::EVENT,
        "seq": 1,
        "type": "ready",
        "data": {
            "session_id": session_id,
            "user_id": user_id,
            "spaces": space_ids.iter().collect::<Vec<_>>(),
            "api_version": "v1",
            "server_version": env!("CARGO_PKG_VERSION")
        }
    });
    if ws_sink
        .send(Message::Text(ready.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Register session with dispatcher
    let session = GatewaySession {
        session_id: session_id.clone(),
        user_id: user_id.clone(),
        intents: user_intents.clone(),
        space_ids: space_ids.clone(),
        sequence: 1,
        tx: tx.clone(),
    };

    if let Some(ref dispatcher) = *state.dispatcher.read().await {
        dispatcher.register_session(session);
    }

    // Subscribe to broadcasts
    let mut broadcast_rx = (*state.dispatcher.read().await)
        .as_ref()
        .map(|dispatcher| dispatcher.subscribe());

    let mut seq: u64 = 1;
    let mut last_heartbeat = tokio::time::Instant::now();
    let mut heartbeat_interval = tokio::time::interval(HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            // Outgoing messages from the session channel
            Some(msg) = rx.recv() => {
                if ws_sink.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            // Broadcast events
            broadcast = async {
                if let Some(ref mut rx) = broadcast_rx {
                    rx.recv().await.ok()
                } else {
                    std::future::pending::<Option<GatewayBroadcast>>().await
                }
            } => {
                if let Some(broadcast) = broadcast {
                    // Check if this session should receive this event
                    let should_receive = match (&broadcast.target_user_ids, &broadcast.space_id) {
                        (Some(targets), _) => targets.contains(&user_id),
                        (None, Some(sid)) => space_ids.contains(sid),
                        (None, None) => true, // global event
                    };

                    if should_receive {
                        // Check intent
                        let event_type = broadcast.event.get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        if intents::has_intent(&user_intents, event_type) {
                            seq += 1;
                            let mut event = broadcast.event.clone();
                            if let Some(obj) = event.as_object_mut() {
                                obj.insert("seq".to_string(), serde_json::json!(seq));
                            }
                            if ws_sink.send(Message::Text(event.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            // Heartbeat check
            _ = heartbeat_interval.tick() => {
                if last_heartbeat.elapsed() > HEARTBEAT_TIMEOUT {
                    // Session timed out
                    break;
                }
            }
            // Incoming messages
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(gw_msg) = serde_json::from_str::<GatewayMessage>(&text) {
                            match gw_msg.op {
                                op if op == events::opcode::HEARTBEAT => {
                                    last_heartbeat = tokio::time::Instant::now();
                                    let ack = serde_json::json!({
                                        "op": events::opcode::HEARTBEAT_ACK
                                    });
                                    if ws_sink.send(Message::Text(ack.to_string().into())).await.is_err() {
                                        break;
                                    }
                                }
                                op if op == events::opcode::VOICE_STATE_UPDATE => {
                                    if let Some(data) = gw_msg.data {
                                        if let Ok(vsu) = serde_json::from_value::<VoiceStateUpdateData>(data) {
                                            if space_ids.contains(&vsu.space_id) {
                                                if let Some(channel_id) = vsu.channel_id {
                                                    // Join/move voice channel
                                                    let self_mute = vsu.self_mute.unwrap_or(false);
                                                    let self_deaf = vsu.self_deaf.unwrap_or(false);
                                                    let (voice_state, _prev) = crate::voice::state::join_voice_channel(
                                                        &state, &user_id, &vsu.space_id, &channel_id,
                                                        &session_id, self_mute, self_deaf,
                                                    );

                                                    // Broadcast voice.state_update to the space
                                                    let event = serde_json::json!({
                                                        "op": events::opcode::EVENT,
                                                        "type": "voice.state_update",
                                                        "data": voice_state
                                                    });
                                                    if let Some(ref gtx) = *state.gateway_tx.read().await {
                                                        let _ = gtx.send(GatewayBroadcast {
                                                            space_id: Some(vsu.space_id.clone()),
                                                            target_user_ids: None,
                                                            event,
                                                            intent: "voice_states".to_string(),
                                                        });
                                                    }

                                                    // Send voice.server_update directly to this session
                                                    let server_update = match state.voice_backend {
                                                        VoiceBackend::LiveKit => {
                                                            if let Some(ref lk) = state.livekit_client {
                                                                let _ = lk.ensure_room(&channel_id).await;
                                                                match lk.generate_token(&user_id, &channel_id) {
                                                                    Ok(token) => serde_json::json!({
                                                                        "op": events::opcode::EVENT,
                                                                        "type": "voice.server_update",
                                                                        "data": {
                                                                            "space_id": vsu.space_id,
                                                                            "channel_id": channel_id,
                                                                            "backend": "livekit",
                                                                            "url": lk.url(),
                                                                            "token": token
                                                                        }
                                                                    }),
                                                                    Err(_) => serde_json::json!({
                                                                        "op": events::opcode::EVENT,
                                                                        "type": "voice.server_update",
                                                                        "data": {
                                                                            "space_id": vsu.space_id,
                                                                            "channel_id": channel_id,
                                                                            "backend": "livekit",
                                                                            "error": "failed to generate token"
                                                                        }
                                                                    }),
                                                                }
                                                            } else {
                                                                continue;
                                                            }
                                                        }
                                                        VoiceBackend::Custom => {
                                                            serde_json::json!({
                                                                "op": events::opcode::EVENT,
                                                                "type": "voice.server_update",
                                                                "data": {
                                                                    "space_id": vsu.space_id,
                                                                    "channel_id": channel_id,
                                                                    "backend": "custom",
                                                                    "endpoint": "gateway"
                                                                }
                                                            })
                                                        }
                                                    };
                                                    let _ = tx.send(server_update.to_string());
                                                } else {
                                                    // Leave voice
                                                    if let Some(old_vs) = crate::voice::state::leave_voice_channel(&state, &user_id) {
                                                        let left_state = crate::models::voice::VoiceState {
                                                            user_id: user_id.clone(),
                                                            space_id: old_vs.space_id.clone(),
                                                            channel_id: None,
                                                            session_id: session_id.clone(),
                                                            deaf: false,
                                                            mute: false,
                                                            self_deaf: false,
                                                            self_mute: false,
                                                            self_stream: false,
                                                            self_video: false,
                                                            suppress: false,
                                                        };
                                                        let event = serde_json::json!({
                                                            "op": events::opcode::EVENT,
                                                            "type": "voice.state_update",
                                                            "data": left_state
                                                        });
                                                        if let Some(ref gtx) = *state.gateway_tx.read().await {
                                                            let _ = gtx.send(GatewayBroadcast {
                                                                space_id: old_vs.space_id.clone(),
                                                                target_user_ids: None,
                                                                event,
                                                                intent: "voice_states".to_string(),
                                                            });
                                                        }

                                                        // Backend cleanup
                                                        match state.voice_backend {
                                                            VoiceBackend::LiveKit => {
                                                                if let (Some(ref lk), Some(ref ch_id)) =
                                                                    (&state.livekit_client, &old_vs.channel_id)
                                                                {
                                                                    lk.remove_participant(ch_id, &user_id).await;
                                                                    lk.delete_room_if_empty(ch_id).await;
                                                                }
                                                            }
                                                            VoiceBackend::Custom => {
                                                                if let (Some(ref sfu), Some(ref ch_id)) =
                                                                    (&state.embedded_sfu, &old_vs.channel_id)
                                                                {
                                                                    sfu.remove_peer(ch_id, &user_id).await;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                op if op == events::opcode::VOICE_SIGNAL => {
                                    // Only handle signals for custom SFU backend;
                                    // LiveKit handles its own signaling.
                                    if state.voice_backend == VoiceBackend::Custom {
                                        if let Some(data) = gw_msg.data {
                                            if let Ok(signal) = serde_json::from_value::<VoiceSignalData>(data) {
                                                if let Some(ref sfu) = state.embedded_sfu {
                                                    // Route to embedded SFU
                                                    if let Some(vs) = crate::voice::state::get_user_voice_state(&state, &user_id) {
                                                        if let (Some(ref ch_id), Some(ref sp_id)) = (&vs.channel_id, &vs.space_id) {
                                                            sfu.handle_signal(
                                                                &user_id, &session_id,
                                                                ch_id, sp_id,
                                                                &signal.signal_type, &signal.payload,
                                                            ).await;
                                                        }
                                                    }
                                                } else {
                                                    // Fallback: relay signals peer-to-peer
                                                    crate::voice::signaling::relay_signal(
                                                        &state, &user_id, &session_id,
                                                        &signal.signal_type, &signal.payload,
                                                    ).await;
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    // Cleanup: remove from voice if connected
    if let Some(old_vs) = crate::voice::state::leave_voice_channel(&state, &user_id) {
        if let Some(ref sid) = old_vs.space_id {
            let left_state = crate::models::voice::VoiceState {
                user_id: user_id.clone(),
                space_id: old_vs.space_id.clone(),
                channel_id: None,
                session_id: session_id.clone(),
                deaf: false,
                mute: false,
                self_deaf: false,
                self_mute: false,
                self_stream: false,
                self_video: false,
                suppress: false,
            };
            let event = serde_json::json!({
                "op": events::opcode::EVENT,
                "type": "voice.state_update",
                "data": left_state
            });
            if let Some(ref gtx) = *state.gateway_tx.read().await {
                let _ = gtx.send(GatewayBroadcast {
                    space_id: Some(sid.clone()),
                    target_user_ids: None,
                    event,
                    intent: "voice_states".to_string(),
                });
            }
        }

        // Backend cleanup on disconnect
        match state.voice_backend {
            VoiceBackend::LiveKit => {
                if let (Some(ref lk), Some(ref ch_id)) = (&state.livekit_client, &old_vs.channel_id) {
                    lk.remove_participant(ch_id, &user_id).await;
                    lk.delete_room_if_empty(ch_id).await;
                }
            }
            VoiceBackend::Custom => {
                if let (Some(ref sfu), Some(ref ch_id)) = (&state.embedded_sfu, &old_vs.channel_id) {
                    sfu.remove_peer(ch_id, &user_id).await;
                }
            }
        }
    }

    // Cleanup: remove session from dispatcher
    if let Some(ref dispatcher) = *state.dispatcher.read().await {
        dispatcher.remove_session(&session_id);
    }
}

async fn resolve_token(state: &AppState, token: &str) -> Option<String> {
    // Token format: "Bot xxx" or "Bearer xxx"
    if let Some(tok) = token.strip_prefix("Bot ") {
        let token_hash = auth_resolve::create_token_hash(tok);
        let row =
            sqlx::query_as::<_, (String,)>("SELECT user_id FROM bot_tokens WHERE token_hash = ?")
                .bind(&token_hash)
                .fetch_optional(&state.db)
                .await
                .ok()??;
        Some(row.0)
    } else if let Some(tok) = token.strip_prefix("Bearer ") {
        let token_hash = auth_resolve::create_token_hash(tok);
        let row =
            sqlx::query_as::<_, (String,)>("SELECT user_id FROM user_tokens WHERE token_hash = ?")
                .bind(&token_hash)
                .fetch_optional(&state.db)
                .await
                .ok()??;
        Some(row.0)
    } else {
        None
    }
}
