mod common;

use accordserver::{db, security};
use common::TestServer;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[tokio::test]
async fn bootstrap_admin_without_memberships_can_login_and_reach_ready() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let space = server.create_space(&owner.user.id, "Existing space").await;
    server.create_channel(&space, "general").await;
    security::bootstrap_admin(server.pool(), "operator", "local-admin-password-123")
        .await
        .unwrap();
    let base = server.spawn().await;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let response = http
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({"username":"operator", "password":"local-admin-password-123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let login: Value = response.json().await.unwrap();
    assert_eq!(login["data"]["user"]["is_admin"], true);
    let user_id = login["data"]["user"]["id"].as_str().unwrap();
    assert!(db::spaces::list_space_ids_for_user(server.pool(), user_id)
        .await
        .unwrap()
        .is_empty());
    let token = login["data"]["token"].as_str().unwrap();
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(5),
        connect_async(format!("{}/ws", base.replace("http://", "ws://"))),
    )
    .await
    .expect("WebSocket connection timed out")
    .unwrap();
    let hello = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("HELLO timed out")
        .unwrap()
        .unwrap();
    let hello: Value = serde_json::from_str(&hello.into_text().unwrap()).unwrap();
    assert_eq!(hello["op"], 5);
    ws.send(Message::Text(
        json!({"op": 2, "data": {
            "token": format!("Bearer {token}"),
            "intents": ["spaces", "messages", "message_content", "message_reactions",
                "message_typing", "members", "presences", "voice_states"]
        }})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("READY timed out")
        .unwrap()
        .unwrap();
    let ready: Value = serde_json::from_str(&ready.into_text().unwrap()).unwrap();
    assert_eq!(ready["op"], 0);
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["data"]["user"]["is_admin"], true);
    assert_eq!(ready["data"]["user_id"], user_id);
    for field in ["spaces", "channels", "members", "roles", "dm_channels"] {
        assert_eq!(ready["data"][field], json!([]), "{field}");
    }

    for endpoint in [
        "users/@me",
        "users/@me/spaces",
        "users/@me/channels",
        "users/@me/relationships",
        "admin/spaces",
        "admin/settings",
    ] {
        let response = http
            .get(format!("{base}/api/v1/{endpoint}"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{endpoint}");
        let body: Value = response.json().await.unwrap();
        if endpoint == "users/@me/spaces" {
            assert_eq!(body["data"], json!([]));
        }
        if endpoint == "admin/spaces" {
            assert_eq!(body["data"][0]["id"], space);
        }
    }
    ws.send(Message::Text(json!({"op":1,"data":1}).to_string().into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let msg = ws.next().await.unwrap().unwrap();
            let msg: Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
            if msg["op"] == 4 {
                break;
            }
        }
    })
    .await
    .expect("heartbeat ACK timed out");
    ws.close(None).await.unwrap();
}
