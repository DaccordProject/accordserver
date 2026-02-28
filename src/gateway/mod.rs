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

use crate::db;
use crate::middleware::auth as auth_resolve;
use crate::state::AppState;
use events::{
    GatewayBroadcast, GatewayMessage, IdentifyData, PresenceUpdateData,
    VoiceStateUpdateData,
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
    let is_bot;
    let is_admin;
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
                                        let resolved = resolve_token(&state, &identify.token).await;
                                        match resolved {
                                            Some(auth) => {
                                                user_id = auth.user_id;
                                                is_bot = auth.is_bot;
                                                is_admin = auth.is_admin;
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

    // Set user presence to online
    crate::presence::set_presence(&state, &user_id, "online", vec![]);

    // Collect presences of online members in the user's spaces
    let mut all_member_ids = std::collections::HashSet::new();
    for sid in &space_ids {
        if let Ok(members) = db::spaces::list_member_ids_for_space(&state.db, sid).await {
            for mid in members {
                all_member_ids.insert(mid);
            }
        }
    }
    let presences = crate::presence::get_space_presences(&state, &all_member_ids);
    let presences_json: Vec<serde_json::Value> = presences
        .iter()
        .map(|p| serde_json::to_value(p).unwrap_or_default())
        .collect();

    // Send READY event
    let ready = serde_json::json!({
        "op": events::opcode::EVENT,
        "seq": 1,
        "type": "ready",
        "data": {
            "session_id": session_id,
            "user_id": user_id,
            "spaces": space_ids.iter().collect::<Vec<_>>(),
            "presences": presences_json,
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

    // Broadcast presence.update (online) to all spaces
    if let Some(ref gtx) = *state.gateway_tx.read().await {
        let presence_data = serde_json::json!({
            "user_id": user_id,
            "status": "online",
            "client_status": { "desktop": "online" },
            "activities": []
        });
        for sid in &space_ids {
            let event = serde_json::json!({
                "op": events::opcode::EVENT,
                "type": "presence.update",
                "data": presence_data
            });
            let _ = gtx.send(GatewayBroadcast {
                space_id: Some(sid.clone()),
                target_user_ids: None,
                event,
                intent: "presences".to_string(),
            });
        }
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
                                op if op == events::opcode::PRESENCE_UPDATE => {
                                    if let Some(data) = gw_msg.data {
                                        if let Ok(psu) = serde_json::from_value::<PresenceUpdateData>(data) {
                                            let valid_statuses = ["online", "idle", "dnd", "invisible"];
                                            let status = if valid_statuses.contains(&psu.status.as_str()) {
                                                psu.status.as_str()
                                            } else {
                                                "online"
                                            };
                                            let activities = match psu.activity {
                                                Some(a) => vec![a],
                                                None => vec![],
                                            };
                                            crate::presence::set_presence(&state, &user_id, status, activities.clone());

                                            // Broadcast to all spaces
                                            if let Some(ref gtx) = *state.gateway_tx.read().await {
                                                let broadcast_status = if status == "invisible" { "offline" } else { status };
                                                let presence_data = serde_json::json!({
                                                    "user_id": user_id,
                                                    "status": broadcast_status,
                                                    "client_status": { "desktop": broadcast_status },
                                                    "activities": activities
                                                });
                                                for sid in &space_ids {
                                                    let event = serde_json::json!({
                                                        "op": events::opcode::EVENT,
                                                        "type": "presence.update",
                                                        "data": presence_data
                                                    });
                                                    let _ = gtx.send(GatewayBroadcast {
                                                        space_id: Some(sid.clone()),
                                                        target_user_ids: None,
                                                        event,
                                                        intent: "presences".to_string(),
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                                op if op == events::opcode::VOICE_STATE_UPDATE => {
                                    if let Some(data) = gw_msg.data {
                                        if let Ok(vsu) = serde_json::from_value::<VoiceStateUpdateData>(data) {
                                            if space_ids.contains(&vsu.space_id) {
                                                let self_mute = vsu.self_mute.unwrap_or(false);
                                                let self_deaf = vsu.self_deaf.unwrap_or(false);
                                                let self_video = vsu.self_video.unwrap_or(false);
                                                let self_stream = vsu.self_stream.unwrap_or(false);

                                                if let Some(channel_id) = vsu.channel_id {
                                                    // Check if user is already in this exact channel (flag-only update)
                                                    let current_channel = crate::voice::state::get_user_voice_state(&state, &user_id)
                                                        .and_then(|vs| vs.channel_id.clone());
                                                    let is_same_channel = current_channel.as_deref() == Some(channel_id.as_str());

                                                    if is_same_channel {
                                                        // Update flags in-place — no LiveKit teardown/rejoin
                                                        if let Some(voice_state) = crate::voice::state::update_voice_state(
                                                            &state, &user_id, self_mute, self_deaf, self_video, self_stream,
                                                        ) {
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
                                                        }
                                                    } else {
                                                        // New join or channel move — full LiveKit flow
                                                        let auth_user = crate::middleware::auth::AuthUser {
                                                            user_id: user_id.clone(),
                                                            is_bot,
                                                            is_admin,
                                                        };
                                                        let channel = match crate::db::channels::get_channel_row(&state.db, &channel_id).await {
                                                            Ok(ch) => ch,
                                                            Err(_) => continue,
                                                        };
                                                        if channel.channel_type != "voice" {
                                                            continue;
                                                        }
                                                        if crate::middleware::permissions::require_channel_permission(
                                                            &state.db, &channel_id, &auth_user, "connect",
                                                        ).await.is_err() {
                                                            continue;
                                                        }

                                                        let (voice_state, prev) = crate::voice::state::join_voice_channel(
                                                            &state, &user_id, &vsu.space_id, &channel_id,
                                                            &session_id, self_mute, self_deaf, self_video, self_stream,
                                                        );

                                                        // Clean up old LiveKit room if the user moved channels
                                                        if let Some(ref prev_ch) = prev {
                                                            if !state.test_mode {
                                                                if let Some(ref lk) = state.livekit_client {
                                                                    lk.remove_participant(prev_ch, &user_id).await;
                                                                    lk.delete_room_if_empty(prev_ch).await;
                                                                }
                                                            }
                                                        }

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
                                                        if let Some(ref lk) = state.livekit_client {
                                                            if !state.test_mode {
                                                                let _ = lk.ensure_room(&channel_id).await;
                                                            }
                                                            let display_name = crate::db::users::get_user(&state.db, &user_id)
                                                                .await
                                                                .ok()
                                                                .and_then(|u| u.display_name.or(Some(u.username)))
                                                                .unwrap_or_else(|| user_id.clone());
                                                            let server_update = match lk.generate_token(&user_id, &display_name, &channel_id) {
                                                                Ok(token) => serde_json::json!({
                                                                    "op": events::opcode::EVENT,
                                                                    "type": "voice.server_update",
                                                                    "data": {
                                                                        "space_id": vsu.space_id,
                                                                        "channel_id": channel_id,
                                                                        "backend": "livekit",
                                                                        "url": lk.external_url(),
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
                                                            };
                                                            let _ = tx.send(server_update.to_string());
                                                        }
                                                    }
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

                                                        // LiveKit cleanup
                                                        if let Some(ref ch_id) = old_vs.channel_id {
                                                            if !state.test_mode {
                                                                if let Some(ref lk) = state.livekit_client {
                                                                    lk.remove_participant(ch_id, &user_id).await;
                                                                    lk.delete_room_if_empty(ch_id).await;
                                                                }
                                                            }
                                                        }
                                                    }
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

        // LiveKit cleanup on disconnect
        if let Some(ref ch_id) = old_vs.channel_id {
            if !state.test_mode {
                if let Some(ref lk) = state.livekit_client {
                    lk.remove_participant(ch_id, &user_id).await;
                    lk.delete_room_if_empty(ch_id).await;
                }
            }
        }
    }

    // Cleanup: remove session from dispatcher
    if let Some(ref dispatcher) = *state.dispatcher.read().await {
        dispatcher.remove_session(&session_id);
    }

    // Cleanup: set presence to offline if no other sessions for this user
    if !crate::presence::user_has_other_sessions(&state, &user_id, &session_id).await {
        crate::presence::remove_presence(&state, &user_id);

        // Broadcast presence.update (offline) to all spaces
        if let Some(ref gtx) = *state.gateway_tx.read().await {
            let presence_data = serde_json::json!({
                "user_id": user_id,
                "status": "offline",
                "client_status": {},
                "activities": []
            });
            for sid in &space_ids {
                let event = serde_json::json!({
                    "op": events::opcode::EVENT,
                    "type": "presence.update",
                    "data": presence_data
                });
                let _ = gtx.send(GatewayBroadcast {
                    space_id: Some(sid.clone()),
                    target_user_ids: None,
                    event,
                    intent: "presences".to_string(),
                });
            }
        }
    }
}

struct ResolvedAuth {
    user_id: String,
    is_bot: bool,
    is_admin: bool,
}

async fn resolve_token(state: &AppState, token: &str) -> Option<ResolvedAuth> {
    // Token format: "Bot xxx" or "Bearer xxx"
    let (user_id, is_bot) = if let Some(tok) = token.strip_prefix("Bot ") {
        let token_hash = auth_resolve::create_token_hash(tok);
        let row =
            sqlx::query_as::<_, (String,)>("SELECT user_id FROM bot_tokens WHERE token_hash = ?")
                .bind(&token_hash)
                .fetch_optional(&state.db)
                .await
                .ok()??;
        (row.0, true)
    } else if let Some(tok) = token.strip_prefix("Bearer ") {
        let token_hash = auth_resolve::create_token_hash(tok);
        let row =
            sqlx::query_as::<_, (String,)>("SELECT user_id FROM user_tokens WHERE token_hash = ?")
                .bind(&token_hash)
                .fetch_optional(&state.db)
                .await
                .ok()??;
        (row.0, false)
    } else {
        return None;
    };

    let is_admin = crate::db::users::get_user(&state.db, &user_id)
        .await
        .map(|u| u.is_admin)
        .unwrap_or(false);

    Some(ResolvedAuth { user_id, is_bot, is_admin })
}
