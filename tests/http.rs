mod common;

use axum::body::Body;
use common::{authenticated_json_request, authenticated_request, parse_body, TestServer};
use http::{Method, Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn test_health_endpoint() {
    let app = common::test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn test_health_content_type() {
    let app = common::test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.contains("text/plain"),
        "expected text/plain, got {content_type}"
    );
}

#[tokio::test]
async fn test_not_found() {
    let app = common::test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_cors_headers_present() {
    let app = common::test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("Origin", "http://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response
        .headers()
        .contains_key("access-control-allow-origin"));
}

#[tokio::test]
async fn test_cors_preflight() {
    let app = common::test_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/health")
                .header("Origin", "http://example.com")
                .header("Access-Control-Request-Method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response
        .headers()
        .contains_key("access-control-allow-origin"));
    assert!(response
        .headers()
        .contains_key("access-control-allow-methods"));
}

#[tokio::test]
async fn test_ws_rejects_non_upgrade() {
    let app = common::test_app().await;
    let response = app
        .oneshot(Request::builder().uri("/ws").body(Body::empty()).unwrap())
        .await
        .unwrap();
    // Without WebSocket upgrade headers, the server should reject with a client error
    assert!(
        response.status().is_client_error(),
        "expected client error, got {}",
        response.status()
    );
}

// ---------------------------------------------------------------------------
// Message Search Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_message_search_content() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SearchSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Create messages via DB
    let msg_input = accordserver::models::message::CreateMessage {
        content: "hello world".to_string(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
    };
    accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &alice.user.id,
        Some(&space_id),
        &msg_input,
    )
    .await
    .unwrap();

    let msg_input2 = accordserver::models::message::CreateMessage {
        content: "goodbye world".to_string(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
    };
    accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &alice.user.id,
        Some(&space_id),
        &msg_input2,
    )
    .await
    .unwrap();

    // Search for "hello"
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/messages/search?query=hello"),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["content"], "hello world");
}

#[tokio::test]
async fn test_message_search_author_filter() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SearchSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice sends a message
    let msg = accordserver::models::message::CreateMessage {
        content: "from alice".to_string(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
    };
    accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &alice.user.id,
        Some(&space_id),
        &msg,
    )
    .await
    .unwrap();

    // Bob sends a message
    let msg2 = accordserver::models::message::CreateMessage {
        content: "from bob".to_string(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
    };
    accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &bob.user.id,
        Some(&space_id),
        &msg2,
    )
    .await
    .unwrap();

    // Search by author_id (bob)
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!(
            "/api/v1/spaces/{space_id}/messages/search?author_id={}",
            bob.user.id
        ),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["content"], "from bob");
}

#[tokio::test]
async fn test_message_search_pinned_filter() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SearchSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let msg = accordserver::models::message::CreateMessage {
        content: "pinned message".to_string(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
    };
    let created = accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &alice.user.id,
        Some(&space_id),
        &msg,
    )
    .await
    .unwrap();

    let msg2 = accordserver::models::message::CreateMessage {
        content: "unpinned message".to_string(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
    };
    accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &alice.user.id,
        Some(&space_id),
        &msg2,
    )
    .await
    .unwrap();

    // Pin the first message
    accordserver::db::messages::pin_message(server.pool(), &channel_id, &created.id)
        .await
        .unwrap();

    // Search for pinned messages
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/messages/search?pinned=true"),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["content"], "pinned message");
}

#[tokio::test]
async fn test_message_search_pagination() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SearchSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Create 3 messages
    for i in 0..3 {
        let msg = accordserver::models::message::CreateMessage {
            content: format!("message {i}"),
            tts: None,
            embeds: None,
            reply_to: None,
            thread_id: None,
        };
        accordserver::db::messages::create_message(
            server.pool(),
            &channel_id,
            &alice.user.id,
            Some(&space_id),
            &msg,
        )
        .await
        .unwrap();
    }

    // Search with limit=2
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/messages/search?query=message&limit=2"),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(body["cursor"]["has_more"], true);

    // Use cursor for next page
    let cursor = body["cursor"]["after"].as_str().unwrap();
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!(
            "/api/v1/spaces/{space_id}/messages/search?query=message&limit=2&cursor={cursor}"
        ),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(body["cursor"]["has_more"], false);
}

#[tokio::test]
async fn test_message_search_non_member_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "PrivateSpace").await;
    let _channel_id = server.create_channel(&space_id, "general").await;

    // Bob is not a member — should get 403
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/messages/search?query=hello"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_message_search_empty_filters_bad_request() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SearchSpace").await;

    // No filters → 400
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/messages/search"),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Emoji Tests
// ---------------------------------------------------------------------------

/// A tiny 1x1 red PNG encoded as base64 data URI.
fn test_png_data_uri() -> String {
    // Minimal valid PNG: 1x1 pixel, red
    let png_bytes: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, // 8-bit RGB
        0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
        0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21,
        0xBC, 0x33, // compressed pixel data
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
        0xAE, 0x42, 0x60, 0x82,
    ];
    let b64 = simple_base64_encode(png_bytes);
    format!("data:image/png;base64,{b64}")
}

/// A minimal OGG data URI for audio testing.
fn test_ogg_data_uri() -> String {
    // Just enough bytes to pass validation (not a real OGG, but the server only validates mime type)
    let fake_ogg: &[u8] = &[0x4F, 0x67, 0x67, 0x53, 0x00, 0x02, 0x00, 0x00];
    let b64 = simple_base64_encode(fake_ogg);
    format!("data:audio/ogg;base64,{b64}")
}

fn simple_base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[tokio::test]
async fn test_emoji_create_with_image() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "test_emoji",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let emoji = &body["data"];
    assert_eq!(emoji["name"], "test_emoji");
    assert!(
        emoji["image_url"].as_str().is_some(),
        "expected image_url in response"
    );
    let image_url = emoji["image_url"].as_str().unwrap();
    assert!(
        image_url.starts_with("/cdn/emojis/"),
        "image_url should start with /cdn/emojis/"
    );
    assert!(
        image_url.ends_with(".png"),
        "image_url should end with .png"
    );
}

#[tokio::test]
async fn test_emoji_list_has_image_url() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    // Create an emoji
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "list_emoji",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // List emojis
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let emojis = body["data"].as_array().unwrap();
    assert_eq!(emojis.len(), 1);
    assert!(emojis[0]["image_url"].as_str().is_some());
}

#[tokio::test]
async fn test_emoji_delete_cleans_up_file() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    // Create emoji
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "deleteme",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let emoji_id = body["data"]["id"].as_str().unwrap().to_string();
    let image_url = body["data"]["image_url"].as_str().unwrap().to_string();

    // Verify the file exists on disk
    let file_path = server
        .state
        .storage_path
        .join(image_url.strip_prefix("/cdn/").unwrap());
    assert!(file_path.exists(), "emoji file should exist on disk");

    // Delete emoji
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/emojis/{emoji_id}"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the file is cleaned up
    assert!(
        !file_path.exists(),
        "emoji file should be deleted from disk"
    );
}

#[tokio::test]
async fn test_cdn_serves_emoji_image() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    // Create emoji
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "cdntest",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let image_url = body["data"]["image_url"].as_str().unwrap().to_string();

    // Fetch the image via CDN endpoint
    let req = Request::builder()
        .uri(&image_url)
        .body(Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!bytes.is_empty(), "CDN should serve the image file");
}

// ---------------------------------------------------------------------------
// Soundboard Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_soundboard_crud() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SoundSpace").await;

    // Create sound
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "airhorn",
            "audio": test_ogg_data_uri(),
            "volume": 0.8
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let sound = &body["data"];
    let sound_id = sound["id"].as_str().unwrap().to_string();
    assert_eq!(sound["name"], "airhorn");
    assert_eq!(sound["volume"], 0.8);
    assert!(sound["audio_url"].as_str().is_some());

    // List sounds
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let sounds = body["data"].as_array().unwrap();
    assert_eq!(sounds.len(), 1);
    assert_eq!(sounds[0]["name"], "airhorn");

    // Get sound
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/soundboard/{sound_id}"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "airhorn");

    // Update sound
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/soundboard/{sound_id}"),
        &alice.auth_header(),
        &serde_json::json!({ "name": "renamed-horn", "volume": 1.5 }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "renamed-horn");
    assert_eq!(body["data"]["volume"], 1.5);

    // Delete sound
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/soundboard/{sound_id}"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // List again — should be empty
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_soundboard_play() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SoundSpace").await;

    // Create sound
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "horn",
            "audio": test_ogg_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let sound_id = body["data"]["id"].as_str().unwrap().to_string();

    // Play sound
    let req = authenticated_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard/{sound_id}/play"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Voice REST Endpoint Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_voice_info_returns_backend() {
    let server = TestServer::new().await;
    let req = Request::builder()
        .uri("/api/v1/voice/info")
        .body(Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["backend"], "livekit");
}

#[tokio::test]
async fn test_voice_join_voice_channel_success() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({ "self_mute": false, "self_deaf": false }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let data = &body["data"];
    assert_eq!(data["backend"], "livekit");
    assert_eq!(data["voice_state"]["user_id"], alice.user.id);
    assert_eq!(data["voice_state"]["channel_id"], vc_id);
    assert_eq!(data["voice_state"]["self_mute"], false);
    assert_eq!(data["voice_state"]["self_deaf"], false);
    assert!(data["token"].as_str().is_some());
}

#[tokio::test]
async fn test_voice_join_text_channel_rejected() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let text_id = server.create_channel(&space_id, "general").await;

    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{text_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({}),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_voice_join_with_self_mute_and_deaf() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({ "self_mute": true, "self_deaf": true }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["voice_state"]["self_mute"], true);
    assert_eq!(body["data"]["voice_state"]["self_deaf"], true);
}

#[tokio::test]
async fn test_voice_leave_after_join() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Join first
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({}),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Leave
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{vc_id}/voice/leave"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["ok"], true);
}

#[tokio::test]
async fn test_voice_status_empty() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{vc_id}/voice-status"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let states = body["data"].as_array().unwrap();
    assert!(states.is_empty());
}

#[tokio::test]
async fn test_voice_status_after_join() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Join voice
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({}),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Check status
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{vc_id}/voice-status"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let states = body["data"].as_array().unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0]["user_id"], alice.user.id);
    assert_eq!(states[0]["channel_id"], vc_id);
}

#[tokio::test]
async fn test_voice_status_cleared_after_leave() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Join
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({}),
    );
    server.router().oneshot(req).await.unwrap();

    // Leave
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{vc_id}/voice/leave"),
        &alice.auth_header(),
    );
    server.router().oneshot(req).await.unwrap();

    // Status should be empty again
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{vc_id}/voice-status"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let states = body["data"].as_array().unwrap();
    assert!(states.is_empty());
}

#[tokio::test]
async fn test_voice_regions_returns_data() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;

    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/voice-regions"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let regions = body["data"].as_array().unwrap();
    assert!(!regions.is_empty());
    assert_eq!(regions[0]["id"], "livekit");
}

#[tokio::test]
async fn test_voice_join_non_member_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Bob (non-member) tries to join voice → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &bob.auth_header(),
        &serde_json::json!({}),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_voice_leave_non_member_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Bob (non-member) tries to leave voice → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{vc_id}/voice/leave"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_voice_status_non_member_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Bob (non-member) tries to check voice status → 403
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{vc_id}/voice-status"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_voice_regions_non_member_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;

    // Bob (non-member) tries to list voice regions → 403
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/voice-regions"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_voice_multiple_users_in_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // Both join
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &alice.auth_header(),
        &serde_json::json!({}),
    );
    server.router().oneshot(req).await.unwrap();

    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{vc_id}/voice/join"),
        &bob.auth_header(),
        &serde_json::json!({}),
    );
    server.router().oneshot(req).await.unwrap();

    // Status should show both users
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{vc_id}/voice-status"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let states = body["data"].as_array().unwrap();
    assert_eq!(states.len(), 2);
}

#[tokio::test]
async fn test_voice_join_unauthenticated() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "VoiceSpace").await;
    let vc_id = server.create_voice_channel(&space_id, "voice-chat").await;

    // No auth header
    let req = Request::builder()
        .method(Method::POST)
        .uri(&format!("/api/v1/channels/{vc_id}/voice/join"))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&serde_json::json!({})).unwrap()))
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Avatar Upload Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_user_avatar_upload() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let avatar = body["data"]["avatar"].as_str().unwrap();
    assert!(
        avatar.starts_with("/cdn/avatars/"),
        "avatar should be a CDN path, got: {avatar}"
    );
    assert!(avatar.ends_with(".png"), "avatar should end with .png");

    // Verify the file exists on disk
    let file_path = server
        .state
        .storage_path
        .join(avatar.strip_prefix("/cdn/").unwrap());
    assert!(file_path.exists(), "avatar file should exist on disk");
}

#[tokio::test]
async fn test_user_avatar_replace() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Upload first avatar
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let first_avatar = body["data"]["avatar"].as_str().unwrap().to_string();

    let first_path = server
        .state
        .storage_path
        .join(first_avatar.strip_prefix("/cdn/").unwrap());
    assert!(first_path.exists(), "first avatar should exist");

    // Upload replacement avatar
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let second_avatar = body["data"]["avatar"].as_str().unwrap().to_string();
    assert!(second_avatar.starts_with("/cdn/avatars/"));

    // New file should exist
    let second_path = server
        .state
        .storage_path
        .join(second_avatar.strip_prefix("/cdn/").unwrap());
    assert!(second_path.exists(), "replacement avatar should exist");
}

#[tokio::test]
async fn test_user_avatar_remove() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Upload avatar
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let avatar_path = body["data"]["avatar"].as_str().unwrap().to_string();

    let file_path = server
        .state
        .storage_path
        .join(avatar_path.strip_prefix("/cdn/").unwrap());
    assert!(file_path.exists(), "avatar file should exist before removal");

    // Remove avatar by sending empty string
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": "" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert!(
        body["data"]["avatar"].is_null(),
        "avatar should be null after removal"
    );

    // Verify the file is deleted
    assert!(
        !file_path.exists(),
        "avatar file should be deleted from disk"
    );
}

#[tokio::test]
async fn test_member_avatar_upload() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "AvatarSpace").await;

    // Upload member avatar via own member endpoint
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/members/@me"),
        &alice.auth_header(),
        &serde_json::json!({ "avatar": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let avatar = body["data"]["avatar"].as_str().unwrap();
    assert!(
        avatar.starts_with("/cdn/avatars/"),
        "member avatar should be a CDN path"
    );
    assert!(avatar.ends_with(".png"));
}

#[tokio::test]
async fn test_space_icon_upload() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "IconSpace").await;

    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &alice.auth_header(),
        &serde_json::json!({ "icon": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let icon = body["data"]["icon"].as_str().unwrap();
    assert!(
        icon.starts_with("/cdn/icons/"),
        "space icon should be a CDN path, got: {icon}"
    );
    assert!(icon.ends_with(".png"));

    // Verify the file exists on disk
    let file_path = server
        .state
        .storage_path
        .join(icon.strip_prefix("/cdn/").unwrap());
    assert!(file_path.exists(), "icon file should exist on disk");
}

#[tokio::test]
async fn test_avatar_invalid_format() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Send an invalid data URI (text/plain instead of image)
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": "data:text/plain;base64,SGVsbG8=" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_avatar_too_large() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Create a data URI that exceeds 2 MB
    let large_data = vec![0u8; 3 * 1024 * 1024]; // 3 MB
    let b64 = simple_base64_encode(&large_data);
    let data_uri = format!("data:image/png;base64,{b64}");

    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": data_uri }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn test_space_icon_remove() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "IconSpace").await;

    // Upload icon
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &alice.auth_header(),
        &serde_json::json!({ "icon": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let icon_path = body["data"]["icon"].as_str().unwrap().to_string();

    let file_path = server
        .state
        .storage_path
        .join(icon_path.strip_prefix("/cdn/").unwrap());
    assert!(file_path.exists(), "icon file should exist");

    // Remove icon
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &alice.auth_header(),
        &serde_json::json!({ "icon": "" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert!(
        body["data"]["icon"].is_null(),
        "icon should be null after removal"
    );
    assert!(!file_path.exists(), "icon file should be deleted");
}

#[tokio::test]
async fn test_cdn_serves_avatar_image() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Upload avatar
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "avatar": test_png_data_uri() }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let avatar_url = body["data"]["avatar"].as_str().unwrap().to_string();

    // Fetch via CDN
    let req = Request::builder()
        .uri(&avatar_url)
        .body(Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!bytes.is_empty(), "CDN should serve the avatar file");
}

// ---------------------------------------------------------------------------
// Server settings tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_get_settings() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let req = authenticated_request(Method::GET, "/api/v1/settings", &alice.auth_header());
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["max_emoji_size"], 262144);
    assert_eq!(body["data"]["max_avatar_size"], 2097152);
    assert_eq!(body["data"]["max_sound_size"], 2097152);
    assert_eq!(body["data"]["max_attachment_size"], 26214400);
    assert_eq!(body["data"]["max_attachments_per_message"], 10);
}

#[tokio::test]
async fn test_update_settings_admin() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;

    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/settings",
        &admin.auth_header(),
        &serde_json::json!({
            "max_emoji_size": 512000,
            "max_attachments_per_message": 5
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["max_emoji_size"], 512000);
    assert_eq!(body["data"]["max_attachments_per_message"], 5);
    // Unchanged fields keep defaults
    assert_eq!(body["data"]["max_avatar_size"], 2097152);
}

#[tokio::test]
async fn test_update_settings_non_admin_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/settings",
        &alice.auth_header(),
        &serde_json::json!({ "max_emoji_size": 512000 }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_upload_respects_custom_limit() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let space_id = server.create_space(&admin.user.id, "test-space").await;

    // Lower the emoji limit to 10 bytes
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/settings",
        &admin.auth_header(),
        &serde_json::json!({ "max_emoji_size": 10 }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Attempt to upload an emoji (the PNG is larger than 10 bytes)
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &admin.auth_header(),
        &serde_json::json!({
            "name": "test_emoji",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
