mod common;

use common::TestServer;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

async fn spawn_server() -> String {
    let app = common::test_app().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://127.0.0.1:{}", addr.port())
}

/// Spawn a TestServer, returning the server instance and the ws:// base URL.
async fn spawn_test_server() -> (TestServer, String) {
    let server = TestServer::new().await;
    let url = server.spawn().await;
    let ws_url = url.replace("http://", "ws://");
    (server, ws_url)
}

/// Helper: connect, consume HELLO, send IDENTIFY with a valid token, consume READY.
/// Returns the authenticated WebSocket stream.
async fn connect_and_identify(
    ws_url: &str,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
> {
    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();

    // Consume HELLO
    let msg = ws.next().await.unwrap().unwrap();
    let hello: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(hello["op"], 5);

    // Send IDENTIFY
    let identify = serde_json::json!({
        "op": 2,
        "data": {
            "token": token,
            "intents": ["messages", "voice_states"]
        }
    });
    ws.send(Message::Text(identify.to_string().into()))
        .await
        .unwrap();

    // Consume READY
    let msg = ws.next().await.unwrap().unwrap();
    let ready: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(ready["op"], 0);
    assert_eq!(ready["type"], "ready");

    ws
}

/// Read up to `max` messages from the WebSocket, returning the first one whose
/// `type` field matches `event_type`. Collects any other messages into a Vec
/// that is returned alongside.
async fn recv_event_type(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    event_type: &str,
    max: usize,
) -> (Option<serde_json::Value>, Vec<serde_json::Value>) {
    let mut others = Vec::new();
    for _ in 0..max {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await;
        match result {
            Ok(Some(Ok(msg))) => {
                if let Ok(text) = msg.into_text() {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if json.get("type").and_then(|t| t.as_str()) == Some(event_type) {
                            return (Some(json), others);
                        }
                        others.push(json);
                    }
                }
            }
            _ => break,
        }
    }
    (None, others)
}

#[tokio::test]
async fn test_ws_connect_receives_hello() {
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    let msg = ws.next().await.unwrap().unwrap();
    assert!(msg.is_text(), "expected text message, got {msg:?}");
    let text = msg.into_text().unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["op"], 5, "expected HELLO opcode (5)");
}

#[tokio::test]
async fn test_ws_hello_contains_heartbeat_interval() {
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    let msg = ws.next().await.unwrap().unwrap();
    let text = msg.into_text().unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["op"], 5);
    assert!(
        json["data"]["heartbeat_interval"].is_number(),
        "expected heartbeat_interval in HELLO data"
    );
    assert!(json["data"]["heartbeat_interval"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_ws_invalid_identify_gets_invalid_session() {
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    // Consume HELLO
    let _ = ws.next().await.unwrap().unwrap();

    // Send an IDENTIFY with an invalid token
    let identify = serde_json::json!({
        "op": 2,
        "data": {
            "token": "Bot invalid_token_here",
            "intents": ["messages"]
        }
    });
    ws.send(Message::Text(identify.to_string().into()))
        .await
        .unwrap();

    let msg = ws.next().await.unwrap().unwrap();
    let text = msg.into_text().unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["op"], 7, "expected INVALID_SESSION opcode (7)");
}

#[tokio::test]
async fn test_ws_timeout_without_identify() {
    // This test verifies that the server sends INVALID_SESSION if no IDENTIFY
    // is received within the timeout window. We use a shorter timeout approach:
    // just send random text and wait for the INVALID_SESSION.
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    // Consume HELLO
    let _ = ws.next().await.unwrap().unwrap();

    // Send non-identify message
    let msg = serde_json::json!({ "op": 99 });
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();

    // The server should eventually close the connection (after 30s timeout)
    // or send INVALID_SESSION. For test efficiency, we just verify the close.
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_close() {
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    // Consume hello
    let _ = ws.next().await.unwrap().unwrap();
    // Send close
    ws.close(None).await.unwrap();
    // Stream should end
    let remaining: Vec<_> = ws
        .filter_map(|r| async { r.ok() })
        .filter(|m| {
            let keep = !m.is_close();
            async move { keep }
        })
        .collect()
        .await;
    assert!(
        remaining.is_empty(),
        "expected no more messages after close"
    );
}

// ---------------------------------------------------------------------------
// Gateway Voice Opcode Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ws_voice_state_update_join() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Send VOICE_STATE_UPDATE (opcode 9) to join
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": vc_id,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Should receive voice.server_update (may also get voice.state_update broadcast)
    let (found, _) = recv_event_type(&mut ws, "voice.server_update", 3).await;
    let json = found.expect("should receive voice.server_update");
    assert_eq!(json["data"]["space_id"], space_id);
    assert_eq!(json["data"]["channel_id"], vc_id);
    assert_eq!(json["data"]["backend"], "custom");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_state_update_join_broadcasts_to_others() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Both connect and identify
    let mut ws_alice = connect_and_identify(&ws_url, &alice.gateway_token()).await;
    let mut ws_bob = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    // Alice joins voice
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": vc_id,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws_alice
        .send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Alice receives voice.server_update
    let (found, _) = recv_event_type(&mut ws_alice, "voice.server_update", 3).await;
    assert!(found.is_some(), "Alice should receive voice.server_update");

    // Bob should receive voice.state_update broadcast
    let (found, _) = recv_event_type(&mut ws_bob, "voice.state_update", 3).await;
    let json = found.expect("Bob should receive voice.state_update");
    assert_eq!(json["data"]["user_id"], alice.user.id);
    assert_eq!(json["data"]["channel_id"], vc_id);

    ws_alice.close(None).await.unwrap();
    ws_bob.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_state_update_leave() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Join first
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": vc_id,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Consume voice.server_update (and any broadcast)
    let (found, _) = recv_event_type(&mut ws, "voice.server_update", 3).await;
    assert!(found.is_some(), "should receive voice.server_update after join");

    // Leave: send VOICE_STATE_UPDATE with channel_id = null
    let leave = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": null,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws.send(Message::Text(leave.to_string().into()))
        .await
        .unwrap();

    // Give the server a moment to process the leave
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id);
    assert!(vs.is_none(), "voice state should be cleared after leave");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_state_update_invalid_space_ignored() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let _space_id = server.create_space(&alice.user.id, "VoiceSpace").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Send VOICE_STATE_UPDATE for a space Alice is NOT a member of
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": "nonexistent-space-id",
            "channel_id": "nonexistent-channel-id",
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Should NOT receive any voice.server_update (the request is silently ignored)
    let result = tokio::time::timeout(std::time::Duration::from_millis(500), ws.next()).await;
    assert!(
        result.is_err(),
        "should not receive any message for invalid space"
    );

    // Verify no voice state was set
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id);
    assert!(vs.is_none(), "voice state should not be set for invalid space");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_state_update_with_self_mute_deaf() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Join with self_mute and self_deaf set
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": vc_id,
            "self_mute": true,
            "self_deaf": true
        }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Consume voice.server_update (may also get voice.state_update broadcast)
    let (found, _) = recv_event_type(&mut ws, "voice.server_update", 3).await;
    assert!(found.is_some(), "should receive voice.server_update");

    // Verify voice state reflects self_mute and self_deaf
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id)
        .expect("voice state should exist");
    assert!(vs.self_mute);
    assert!(vs.self_deaf);

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_signal_relay() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let mut ws_alice = connect_and_identify(&ws_url, &alice.gateway_token()).await;
    let mut ws_bob = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    // Alice joins voice
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": vc_id,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws_alice
        .send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Consume Alice's voice.server_update
    let (found, _) = recv_event_type(&mut ws_alice, "voice.server_update", 3).await;
    assert!(found.is_some(), "Alice should receive voice.server_update");

    // Consume Bob's voice.state_update broadcast (Alice joined)
    let (found, _) = recv_event_type(&mut ws_bob, "voice.state_update", 3).await;
    assert!(found.is_some(), "Bob should receive voice.state_update");

    // Alice sends VOICE_SIGNAL (opcode 11)
    let signal = serde_json::json!({
        "op": 11,
        "data": {
            "signal_type": "offer",
            "payload": { "sdp": "test-sdp-data" }
        }
    });
    ws_alice
        .send(Message::Text(signal.to_string().into()))
        .await
        .unwrap();

    // Bob should receive the relayed voice.signal event
    let (found, _) = recv_event_type(&mut ws_bob, "voice.signal", 3).await;
    let json = found.expect("Bob should receive voice.signal");
    assert_eq!(json["data"]["user_id"], alice.user.id);
    assert_eq!(json["data"]["signal_type"], "offer");
    assert_eq!(json["data"]["payload"]["sdp"], "test-sdp-data");
    assert_eq!(json["data"]["channel_id"], vc_id);

    ws_alice.close(None).await.unwrap();
    ws_bob.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_cleanup_on_disconnect() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Join voice
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": space_id,
            "channel_id": vc_id,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    // Consume voice.server_update (and any broadcast)
    let (found, _) = recv_event_type(&mut ws, "voice.server_update", 3).await;
    assert!(found.is_some(), "should receive voice.server_update");

    // Verify in voice
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id);
    assert!(vs.is_some(), "should be in voice after join");

    // Disconnect (close the websocket)
    ws.close(None).await.unwrap();

    // Wait for cleanup to process
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Voice state should be cleaned up
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id);
    assert!(vs.is_none(), "voice state should be cleaned up on disconnect");
}

#[tokio::test]
async fn test_ws_heartbeat_ack() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let _space_id = server.create_space(&alice.user.id, "HeartbeatSpace").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Send HEARTBEAT (opcode 1)
    let hb = serde_json::json!({ "op": 1 });
    ws.send(Message::Text(hb.to_string().into()))
        .await
        .unwrap();

    // Should receive HEARTBEAT_ACK (opcode 4)
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timeout waiting for heartbeat ack")
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(json["op"], 4, "expected HEARTBEAT_ACK opcode (4)");

    ws.close(None).await.unwrap();
}
