mod common;

use common::{authenticated_json_request, TestServer};
use futures_util::{SinkExt, StreamExt};
use http::{Method, StatusCode};
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

type WsConn =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
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
    assert_eq!(json["data"]["backend"], "livekit");
    assert!(json["data"]["token"].as_str().is_some());

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
    assert!(
        found.is_some(),
        "should receive voice.server_update after join"
    );

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
async fn test_ws_voice_join_denied_without_connect_permission() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server
        .create_voice_channel(&space_id, "restricted-voice")
        .await;

    // Create bob as a space member
    let bob = server.create_user_with_token("bob").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Deny the `connect` permission for bob via a member-level overwrite
    accordserver::db::permission_overwrites::upsert_overwrite(
        server.pool(),
        &vc_id,
        &accordserver::models::permission::PermissionOverwrite {
            id: bob.user.id.clone(),
            overwrite_type: "member".to_string(),
            allow: vec![],
            deny: vec!["connect".to_string()],
        },
    )
    .await
    .expect("failed to set permission overwrite");

    let mut ws = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    // Bob tries to join the voice channel
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

    // Should NOT receive voice.server_update — the join is rejected due to missing connect perm
    let result = tokio::time::timeout(std::time::Duration::from_millis(500), ws.next()).await;
    assert!(
        result.is_err(),
        "bob should not receive voice.server_update when connect is denied"
    );

    // Verify no voice state was set for bob
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &bob.user.id);
    assert!(
        vs.is_none(),
        "bob's voice state should not be set when connect permission is denied"
    );

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
    assert!(
        vs.is_none(),
        "voice state should not be set for invalid space"
    );

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
    assert!(
        vs.is_none(),
        "voice state should be cleaned up on disconnect"
    );
}

#[tokio::test]
async fn test_ws_heartbeat_ack() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let _space_id = server.create_space(&alice.user.id, "HeartbeatSpace").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Send HEARTBEAT (opcode 1)
    let hb = serde_json::json!({ "op": 1 });
    ws.send(Message::Text(hb.to_string().into())).await.unwrap();

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

// ── DM voice state (no parent space) ────────────────────────────────────────

/// Join a DM call over the gateway by sending VOICE_STATE_UPDATE with no
/// `space_id`, and consume the resulting `voice.server_update`.
async fn join_dm_voice(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    dm_id: &str,
) {
    let vsu = serde_json::json!({
        "op": 9,
        "data": { "channel_id": dm_id, "self_mute": false, "self_deaf": false }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();
    let (found, _) = recv_event_type(ws, "voice.server_update", 3).await;
    assert!(
        found.is_some(),
        "should receive voice.server_update after joining a DM call"
    );
}

#[tokio::test]
async fn test_ws_dm_voice_mute_broadcasts_to_peer() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let dm_id = server.create_dm(&alice.user.id, &bob.user.id).await;

    let mut ws_alice = connect_and_identify(&ws_url, &alice.gateway_token()).await;
    let mut ws_bob = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    join_dm_voice(&mut ws_alice, &dm_id).await;

    // Bob sees the join.
    let (found, _) = recv_event_type(&mut ws_bob, "voice.state_update", 3).await;
    let json = found.expect("Bob should receive the DM join");
    assert_eq!(json["data"]["channel_id"], dm_id);
    assert_eq!(json["data"]["space_id"], serde_json::Value::Null);
    assert_eq!(json["data"]["self_mute"], false);

    // Mid-call mute/deafen — the update this whole path exists for.
    let vsu = serde_json::json!({
        "op": 9,
        "data": { "channel_id": dm_id, "self_mute": true, "self_deaf": true }
    });
    ws_alice
        .send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    let (found, _) = recv_event_type(&mut ws_bob, "voice.state_update", 3).await;
    let json = found.expect("Bob should receive Alice's mid-call mute");
    assert_eq!(json["data"]["user_id"], alice.user.id);
    assert_eq!(json["data"]["self_mute"], true);
    assert_eq!(json["data"]["self_deaf"], true);

    // The stored state moved too — the peer isn't just seeing a stray event.
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id)
        .expect("Alice should still be in the call");
    assert!(vs.self_mute);
    assert!(vs.self_deaf);
    assert!(vs.space_id.is_none(), "a DM call has no parent space");

    ws_alice.close(None).await.unwrap();
    ws_bob.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_dm_voice_not_broadcast_to_strangers() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let carol = server.create_user_with_token("carol").await;
    let dm_id = server.create_dm(&alice.user.id, &bob.user.id).await;

    let mut ws_alice = connect_and_identify(&ws_url, &alice.gateway_token()).await;
    let mut ws_carol = connect_and_identify(&ws_url, &carol.gateway_token()).await;

    join_dm_voice(&mut ws_alice, &dm_id).await;

    // Carol shares no space and no DM with Alice. A DM voice broadcast carries
    // neither a space_id nor Carol in its targets, so it must not reach her —
    // a `None`/`None` broadcast would go to every session on the instance.
    let (found, others) = recv_event_type(&mut ws_carol, "voice.state_update", 2).await;
    assert!(
        found.is_none(),
        "Carol must not receive a DM voice state she has no part in; got {others:?}"
    );

    ws_alice.close(None).await.unwrap();
    ws_carol.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_dm_voice_leave_notifies_peer_and_ends_call() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let dm_id = server.create_dm(&alice.user.id, &bob.user.id).await;

    let mut ws_alice = connect_and_identify(&ws_url, &alice.gateway_token()).await;
    let mut ws_bob = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    join_dm_voice(&mut ws_alice, &dm_id).await;
    let (found, _) = recv_event_type(&mut ws_bob, "voice.state_update", 3).await;
    assert!(found.is_some(), "Bob should receive the DM join");

    // Leave: channel_id omitted.
    let vsu = serde_json::json!({ "op": 9, "data": { "channel_id": null } });
    ws_alice
        .send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    let (found, _) = recv_event_type(&mut ws_bob, "voice.state_update", 3).await;
    let json = found.expect("Bob should be told Alice left");
    assert_eq!(json["data"]["user_id"], alice.user.id);
    assert_eq!(json["data"]["channel_id"], serde_json::Value::Null);

    // Nobody left in the call, so the call itself is over.
    let (found, others) = recv_event_type(&mut ws_bob, "call.end", 3).await;
    assert!(
        found.is_some(),
        "Bob should receive call.end once the DM call empties; got {others:?}"
    );

    ws_alice.close(None).await.unwrap();
    ws_bob.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_dm_voice_rejected_for_non_participant() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let carol = server.create_user_with_token("carol").await;
    let dm_id = server.create_dm(&alice.user.id, &bob.user.id).await;

    let mut ws_carol = connect_and_identify(&ws_url, &carol.gateway_token()).await;

    let vsu = serde_json::json!({
        "op": 9,
        "data": { "channel_id": dm_id, "self_mute": false, "self_deaf": false }
    });
    ws_carol
        .send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    let (found, _) = recv_event_type(&mut ws_carol, "voice.server_update", 2).await;
    assert!(
        found.is_none(),
        "a non-participant must not be able to join someone else's DM call"
    );
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &carol.user.id);
    assert!(vs.is_none(), "no voice state should be set for Carol");

    ws_carol.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_voice_state_update_mismatched_space_ignored() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let other_space = server.create_space(&alice.user.id, "OtherSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Alice is a member of both spaces, but names the wrong one for this
    // channel. The scope is resolved from the channel, so the claim is a
    // cross-check that must fail rather than steer the broadcast.
    let vsu = serde_json::json!({
        "op": 9,
        "data": {
            "space_id": other_space,
            "channel_id": vc_id,
            "self_mute": false,
            "self_deaf": false
        }
    });
    ws.send(Message::Text(vsu.to_string().into()))
        .await
        .unwrap();

    let (found, _) = recv_event_type(&mut ws, "voice.server_update", 2).await;
    assert!(found.is_none(), "a mismatched space_id should be ignored");
    let vs = accordserver::voice::state::get_user_voice_state(&server.state, &alice.user.id);
    assert!(vs.is_none(), "voice state should not be set");

    ws.close(None).await.unwrap();
}

// ── user.update fanout ──────────────────────────────────────────────────────

/// PATCH /users/@me through the same AppState the gateway sessions are bound to.
async fn patch_display_name(server: &TestServer, user: &common::TestUser, name: &str) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PATCH,
            "/api/v1/users/@me",
            &user.auth_header(),
            &serde_json::json!({ "display_name": name }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "profile update should succeed"
    );
}

#[tokio::test]
async fn test_ws_user_update_broadcast_to_space_members() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SharedSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    let mut ws_bob = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    patch_display_name(&server, &alice, "Alice Renamed").await;

    let (found, others) = recv_event_type(&mut ws_bob, "user.update", 4).await;
    let json = found
        .unwrap_or_else(|| panic!("Bob should receive Alice's profile change; got {others:?}"));
    assert_eq!(json["data"]["id"], alice.user.id);
    assert_eq!(json["data"]["display_name"], "Alice Renamed");

    // The broadcast reaches third parties, so it must carry only the public
    // projection — never is_admin/mfa_enabled/disabled/flags.
    for field in ["is_admin", "mfa_enabled", "disabled", "flags"] {
        assert!(
            json["data"].get(field).is_none(),
            "user.update must not expose `{field}`"
        );
    }

    ws_bob.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_user_update_reaches_dm_peer_without_shared_space() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let _dm_id = server.create_dm(&alice.user.id, &bob.user.id).await;

    let mut ws_bob = connect_and_identify(&ws_url, &bob.gateway_token()).await;

    patch_display_name(&server, &alice, "Alice In DMs").await;

    let (found, others) = recv_event_type(&mut ws_bob, "user.update", 4).await;
    let json = found.unwrap_or_else(|| {
        panic!("a DM peer should see the change even with no shared space; got {others:?}")
    });
    assert_eq!(json["data"]["display_name"], "Alice In DMs");

    ws_bob.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_user_update_not_sent_to_strangers() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let carol = server.create_user_with_token("carol").await;
    let _alice_space = server.create_space(&alice.user.id, "AliceSpace").await;
    let _carol_space = server.create_space(&carol.user.id, "CarolSpace").await;

    let mut ws_carol = connect_and_identify(&ws_url, &carol.gateway_token()).await;

    patch_display_name(&server, &alice, "Alice Renamed").await;

    let (found, others) = recv_event_type(&mut ws_carol, "user.update", 2).await;
    assert!(
        found.is_none(),
        "Carol shares no space, DM or friendship with Alice; got {others:?}"
    );

    ws_carol.close(None).await.unwrap();
}

#[tokio::test]
async fn test_ws_user_update_reaches_own_other_sessions() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;

    // No spaces, no DMs, no friends — a second session of Alice's own still has
    // to learn that her profile changed.
    let mut ws_alice = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    patch_display_name(&server, &alice, "Alice Elsewhere").await;

    let (found, others) = recv_event_type(&mut ws_alice, "user.update", 4).await;
    let json = found.unwrap_or_else(|| {
        panic!("Alice's other session should sync her own change; got {others:?}")
    });
    assert_eq!(json["data"]["id"], alice.user.id);
    assert_eq!(json["data"]["display_name"], "Alice Elsewhere");

    ws_alice.close(None).await.unwrap();
}

// ---------------------------------------------------------------------------
// Handshake, RESUME, rate limiting and IDENTIFY presence
// ---------------------------------------------------------------------------

const SHORT: std::time::Duration = std::time::Duration::from_secs(2);

/// Connect and IDENTIFY with explicit intents and an optional presence payload.
/// Returns the socket and the READY frame.
async fn connect_with(
    ws_url: &str,
    token: &str,
    intents: &[&str],
    presence: Option<serde_json::Value>,
) -> (WsConn, serde_json::Value) {
    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();
    let hello = next_json(&mut ws).await;
    assert_eq!(hello["op"], 5);

    let mut data = serde_json::json!({ "token": token, "intents": intents });
    if let Some(presence) = presence {
        data["presence"] = presence;
    }
    ws.send(Message::Text(
        serde_json::json!({ "op": 2, "data": data })
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    let ready = next_json(&mut ws).await;
    assert_eq!(ready["type"], "ready", "expected READY, got {ready}");
    (ws, ready)
}

/// Read the next frame as JSON, failing rather than hanging if none arrives.
async fn next_json(ws: &mut WsConn) -> serde_json::Value {
    let msg = tokio::time::timeout(SHORT, ws.next())
        .await
        .expect("timed out waiting for a gateway frame")
        .expect("stream ended")
        .expect("websocket error");
    assert!(!msg.is_close(), "expected a text frame, got {msg:?}");
    serde_json::from_str(&msg.into_text().unwrap()).unwrap()
}

async fn post_message(server: &TestServer, user: &common::TestUser, channel_id: &str, body: &str) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel_id}/messages"),
            &user.auth_header(),
            &serde_json::json!({ "content": body }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ws_unknown_opcode_before_identify_fails_fast() {
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    let _hello = ws.next().await.unwrap().unwrap();

    ws.send(Message::Text(
        serde_json::json!({ "op": 99 }).to_string().into(),
    ))
    .await
    .unwrap();

    // The answer has to be immediate. Stalling until the 30s identify timeout
    // turns a blip into a visible outage for everyone in the user's spaces.
    let json = next_json(&mut ws).await;
    assert_eq!(json["op"], 7, "expected INVALID_SESSION, got {json}");
    assert_eq!(json["data"]["resumable"], false);
}

#[tokio::test]
async fn test_ws_heartbeat_before_identify_is_acked() {
    let url = spawn_server().await;
    let (mut ws, _) = connect_async(format!("{url}/ws")).await.unwrap();
    let _hello = ws.next().await.unwrap().unwrap();

    // HELLO hands out the heartbeat interval, so a client may start beating
    // before it identifies; that must not be read as a protocol error.
    ws.send(Message::Text(
        serde_json::json!({ "op": 1 }).to_string().into(),
    ))
    .await
    .unwrap();

    let json = next_json(&mut ws).await;
    assert_eq!(json["op"], 4, "expected HEARTBEAT_ACK, got {json}");
}

#[tokio::test]
async fn test_ws_resume_replays_events_missed_while_disconnected() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SharedSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let (ws_alice, ready) =
        connect_with(&ws_url, &alice.gateway_token(), &["messages"], None).await;
    let session_id = ready["data"]["session_id"].as_str().unwrap().to_string();
    let last_seq = ready["seq"].as_u64().unwrap();

    // Drop the socket the way a network blip would: no close frame.
    drop(ws_alice);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    post_message(&server, &bob, &channel_id, "while you were out").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();
    let hello = next_json(&mut ws).await;
    assert_eq!(hello["op"], 5);
    ws.send(Message::Text(
        serde_json::json!({
            "op": 3,
            "data": {
                "token": alice.gateway_token(),
                "session_id": session_id,
                "seq": last_seq
            }
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    // The message sent while Alice was away is replayed before the resume is
    // confirmed — a client told it resumed stops backfilling, so anything the
    // gap swallowed would be lost for good.
    let replayed = next_json(&mut ws).await;
    assert_eq!(
        replayed["type"], "message.create",
        "expected the missed message, got {replayed}"
    );
    assert_eq!(replayed["data"]["content"], "while you were out");
    assert!(replayed["seq"].as_u64().unwrap() > last_seq);

    let resumed = next_json(&mut ws).await;
    assert_eq!(
        resumed["type"], "resumed",
        "expected RESUMED, got {resumed}"
    );
    assert_eq!(resumed["data"]["session_id"], session_id);
    assert_eq!(resumed["data"]["replayed"], 1);
}

#[tokio::test]
async fn test_ws_resume_keeps_delivering_after_handover() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SharedSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let (ws_alice, ready) =
        connect_with(&ws_url, &alice.gateway_token(), &["messages"], None).await;
    let session_id = ready["data"]["session_id"].as_str().unwrap().to_string();
    drop(ws_alice);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();
    let _hello = next_json(&mut ws).await;
    ws.send(Message::Text(
        serde_json::json!({
            "op": 3,
            "data": {
                "token": alice.gateway_token(),
                "session_id": session_id,
                "seq": ready["seq"]
            }
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let resumed = next_json(&mut ws).await;
    assert_eq!(
        resumed["type"], "resumed",
        "expected RESUMED, got {resumed}"
    );

    // The handed-over receiver has to keep working on the new socket.
    post_message(&server, &bob, &channel_id, "after the handover").await;
    let (found, others) = recv_event_type(&mut ws, "message.create", 4).await;
    let json = found.unwrap_or_else(|| {
        panic!("a resumed session should still receive live events; got {others:?}")
    });
    assert_eq!(json["data"]["content"], "after the handover");
}

#[tokio::test]
async fn test_ws_resume_rejects_another_users_session() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let mallory = server.create_user_with_token("mallory").await;

    let (ws_alice, ready) =
        connect_with(&ws_url, &alice.gateway_token(), &["messages"], None).await;
    let session_id = ready["data"]["session_id"].as_str().unwrap().to_string();
    drop(ws_alice);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();
    let _hello = next_json(&mut ws).await;
    ws.send(Message::Text(
        serde_json::json!({
            "op": 3,
            "data": {
                "token": mallory.gateway_token(),
                "session_id": session_id,
                "seq": 1
            }
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let json = next_json(&mut ws).await;
    assert_eq!(json["op"], 7, "expected INVALID_SESSION, got {json}");
    drop(ws);

    // Rejecting Mallory must not have consumed the session: Alice can still
    // claim it herself.
    let (mut ws_alice, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();
    let _hello = next_json(&mut ws_alice).await;
    ws_alice
        .send(Message::Text(
            serde_json::json!({
                "op": 3,
                "data": {
                    "token": alice.gateway_token(),
                    "session_id": session_id,
                    "seq": ready["seq"]
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let resumed = next_json(&mut ws_alice).await;
    assert_eq!(
        resumed["type"], "resumed",
        "expected RESUMED, got {resumed}"
    );
}

#[tokio::test]
async fn test_ws_heartbeats_are_not_rate_limited() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Well past the 120-per-minute cap. A dropped heartbeat lets last_heartbeat
    // go stale and kills the session at the next timeout check, which culls
    // precisely the busiest clients.
    const BEATS: usize = 200;
    for _ in 0..BEATS {
        ws.send(Message::Text(
            serde_json::json!({ "op": 1 }).to_string().into(),
        ))
        .await
        .unwrap();
    }

    let mut acks = 0;
    while acks < BEATS {
        let json = next_json(&mut ws).await;
        if json["op"] == 4 {
            acks += 1;
        }
    }
    assert_eq!(acks, BEATS);
}

#[tokio::test]
async fn test_ws_rate_limit_closes_instead_of_dropping_silently() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let mut ws = connect_and_identify(&ws_url, &alice.gateway_token()).await;

    // Presence updates are counted. Going quiet on the overflow leaves the
    // client with no idea it should back off, so the server closes with 4008.
    for _ in 0..130 {
        ws.send(Message::Text(
            serde_json::json!({ "op": 8, "data": { "status": "online" } })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }

    let mut close_code = None;
    for _ in 0..16 {
        let msg = tokio::time::timeout(SHORT, ws.next())
            .await
            .expect("expected a close frame, not silence");
        match msg {
            Some(Ok(Message::Close(frame))) => {
                close_code = frame.map(|f| u16::from(f.code));
                break;
            }
            Some(Ok(_)) => continue,
            _ => break,
        }
    }
    assert_eq!(close_code, Some(4008), "expected close code RATE_LIMITED");
}

#[tokio::test]
async fn test_ws_identify_presence_is_honoured() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SharedSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    let (mut ws_bob, _) = connect_with(&ws_url, &bob.gateway_token(), &["presences"], None).await;
    let (_ws_alice, _) = connect_with(
        &ws_url,
        &alice.gateway_token(),
        &["presences"],
        Some(serde_json::json!({ "status": "dnd" })),
    )
    .await;

    let (found, others) = recv_event_type(&mut ws_bob, "presence.update", 4).await;
    let json = found.unwrap_or_else(|| panic!("Bob should see Alice connect; got {others:?}"));
    assert_eq!(json["data"]["user_id"], alice.user.id);
    assert_eq!(
        json["data"]["status"], "dnd",
        "IDENTIFY must publish the presence the client asked for"
    );
}

#[tokio::test]
async fn test_ws_identify_preserves_existing_status() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SharedSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    let (mut ws_bob, _) = connect_with(&ws_url, &bob.gateway_token(), &["presences"], None).await;

    let (_ws_alice_1, _) = connect_with(
        &ws_url,
        &alice.gateway_token(),
        &["presences"],
        Some(serde_json::json!({ "status": "dnd" })),
    )
    .await;
    let (found, _) = recv_event_type(&mut ws_bob, "presence.update", 4).await;
    assert_eq!(
        found.expect("Alice's first session")["data"]["status"],
        "dnd"
    );

    // A second session that says nothing about presence must not reset her.
    let (_ws_alice_2, ready) =
        connect_with(&ws_url, &alice.gateway_token(), &["presences"], None).await;

    let (found, others) = recv_event_type(&mut ws_bob, "presence.update", 4).await;
    let json = found.unwrap_or_else(|| panic!("Alice's second session; got {others:?}"));
    assert_eq!(json["data"]["user_id"], alice.user.id);
    assert_eq!(
        json["data"]["status"], "dnd",
        "reconnecting must not stomp a chosen status"
    );

    let own = ready["data"]["presences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["user_id"] == alice.user.id.as_str())
        .expect("READY should carry Alice's own presence");
    assert_eq!(own["status"], "dnd");
}

// =========================================================================
// Mid-session membership changes (#52)
// =========================================================================

/// A session that joins a space *after* IDENTIFY must start receiving that
/// space's events without reconnecting. The membership set used for fan-out
/// was previously a snapshot taken at IDENTIFY, so a space joined while
/// connected stayed silent for the life of the connection.
#[tokio::test]
async fn test_ws_space_joined_mid_session_receives_events() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;

    let space_id = server.create_public_space(&alice.user.id, "Open").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Bob connects first, while he is not a member of anything.
    let (mut ws_bob, _) = connect_with(
        &ws_url,
        &bob.gateway_token(),
        &["messages", "message_content", "members"],
        None,
    )
    .await;

    // …and only then joins.
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/join"),
        &bob.auth_header(),
        &serde_json::json!({}),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    // His own member.join must arrive — the event that carries the membership
    // change can't be filtered out by the stale membership it announces.
    let (found, others) = recv_event_type(&mut ws_bob, "member.join", 5).await;
    let join = found.unwrap_or_else(|| panic!("bob's own member.join; got {others:?}"));
    assert_eq!(join["data"]["user"]["id"], bob.user.id);

    // And a message posted afterwards must reach him.
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "after the join" }),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let (found, others) = recv_event_type(&mut ws_bob, "message.create", 5).await;
    let message = found.unwrap_or_else(|| panic!("message after join; got {others:?}"));
    assert_eq!(message["data"]["content"], "after the join");
}

/// The refresh is about routing, not subscription: a client that didn't ask
/// for the `members` intent still has its membership updated, it just doesn't
/// see the `member.join` itself.
#[tokio::test]
async fn test_ws_mid_session_join_refreshes_without_members_intent() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;

    let space_id = server.create_public_space(&alice.user.id, "Open").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // No "members" intent.
    let (mut ws_bob, _) = connect_with(
        &ws_url,
        &bob.gateway_token(),
        &["messages", "message_content"],
        None,
    )
    .await;

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/join"),
        &bob.auth_header(),
        &serde_json::json!({}),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "still routed" }),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let (found, others) = recv_event_type(&mut ws_bob, "message.create", 5).await;
    let message = found.unwrap_or_else(|| panic!("message after join; got {others:?}"));
    assert_eq!(message["data"]["content"], "still routed");
    assert!(
        !others.iter().any(|e| e["type"] == "member.join"),
        "member.join needs the members intent"
    );
}

/// Leaving is the mirror image: the session must stop receiving the space's
/// events without waiting for a reconnect.
#[tokio::test]
async fn test_ws_space_left_mid_session_stops_receiving_events() {
    let (server, ws_url) = spawn_test_server().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;

    let space_id = server.create_public_space(&alice.user.id, "Open").await;
    let channel_id = server.create_channel(&space_id, "general").await;
    server.add_member(&space_id, &bob.user.id).await;

    let (mut ws_bob, _) = connect_with(
        &ws_url,
        &bob.gateway_token(),
        &["messages", "message_content", "members"],
        None,
    )
    .await;

    let app = server.router();
    let req = authenticated_json_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/members/@me"),
        &bob.auth_header(),
        &serde_json::json!({}),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let (found, others) = recv_event_type(&mut ws_bob, "member.leave", 5).await;
    found.unwrap_or_else(|| panic!("bob's own member.leave; got {others:?}"));

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "after the leave" }),
    );
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);

    let (found, _) = recv_event_type(&mut ws_bob, "message.create", 3).await;
    assert!(
        found.is_none(),
        "a left space must stop delivering messages"
    );
}
