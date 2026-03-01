mod common;

use common::{
    authenticated_json_request, authenticated_request, json_request, parse_body, TestServer,
};
use futures_util::{SinkExt, StreamExt};
use http::{Method, StatusCode};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

// =========================================================================
// Auth validation
// =========================================================================

#[tokio::test]
async fn test_unauthenticated_request_returns_401() {
    let server = TestServer::new().await;
    let app = server.router();
    let req = axum::body::Body::empty();
    let response = app
        .oneshot(
            http::Request::builder()
                .uri("/api/v1/users/@me")
                .body(req)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_invalid_token_returns_401() {
    let server = TestServer::new().await;
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", "Bearer bogus.token.here");
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// =========================================================================
// Users
// =========================================================================

#[tokio::test]
async fn test_get_current_user_with_bearer_token() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let app = server.router();

    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &alice.auth_header());
    let response = app.oneshot(req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["username"], "alice");
    assert_eq!(body["data"]["id"], alice.user.id);
    assert_eq!(body["data"]["bot"], false);
}

#[tokio::test]
async fn test_get_current_user_with_bot_token() {
    let server = TestServer::new().await;
    let (_owner, bot) = server.create_bot_with_token("owner", "TestApp").await;
    let app = server.router();

    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &bot.auth_header());
    let response = app.oneshot(req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["bot"], true);
}

#[tokio::test]
async fn test_update_current_user() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let app = server.router();

    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "display_name": "Alice Wonderland" }),
    );
    let response = app.oneshot(req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["display_name"], "Alice Wonderland");
}

#[tokio::test]
async fn test_get_user_by_id() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let app = server.router();

    let uri = format!("/api/v1/users/{}", bob.user.id);
    let req = authenticated_request(Method::GET, &uri, &alice.auth_header());
    let response = app.oneshot(req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["username"], "bob");
    assert_eq!(body["data"]["id"], bob.user.id);
}

// =========================================================================
// Spaces
// =========================================================================

#[tokio::test]
async fn test_space_crud_lifecycle() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    // CREATE
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "My Space" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let space_id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["name"], "My Space");

    // GET
    let app = server.router();
    let req = authenticated_request(Method::GET, &format!("/api/v1/spaces/{space_id}"), &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "My Space");

    // PATCH (rename)
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &auth,
        &serde_json::json!({ "name": "Renamed Space" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "Renamed Space");

    // DELETE
    let app = server.router();
    let req = authenticated_request(Method::DELETE, &format!("/api/v1/spaces/{space_id}"), &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // GET after delete → 404
    let app = server.router();
    let req = authenticated_request(Method::GET, &format!("/api/v1/spaces/{space_id}"), &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_non_owner_cannot_update_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    server.add_member(&space_id, &bob.user.id).await;

    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &bob.auth_header(),
        &serde_json::json!({ "name": "Hijacked" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_owner_cannot_delete_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    server.add_member(&space_id, &bob.user.id).await;

    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_get_current_user_spaces() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Create two spaces — alice is auto-added as member/owner
    server.create_space(&alice.user.id, "Space A").await;
    server.create_space(&alice.user.id, "Space B").await;

    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        "/api/v1/users/@me/spaces",
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let spaces = body["data"].as_array().unwrap();
    assert_eq!(spaces.len(), 2);
}

// =========================================================================
// Channels
// =========================================================================

#[tokio::test]
async fn test_channel_crud_via_api() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;

    // CREATE channel via space endpoint
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/channels"),
        &auth,
        &serde_json::json!({ "name": "dev-chat", "type": "text" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let channel_id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["name"], "dev-chat");

    // GET channel
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{channel_id}"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "dev-chat");

    // PATCH channel
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel_id}"),
        &auth,
        &serde_json::json!({ "name": "dev-chat-renamed", "topic": "Developers only" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "dev-chat-renamed");
    assert_eq!(body["data"]["topic"], "Developers only");

    // DELETE channel
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{channel_id}"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_channels_in_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    // create_space auto-creates #general, so add one more
    server.create_channel(&space_id, "extra").await;

    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/channels"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let channels = body["data"].as_array().unwrap();
    assert_eq!(channels.len(), 2); // #general + extra
}

// =========================================================================
// Messages
// =========================================================================

#[tokio::test]
async fn test_message_crud_lifecycle() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "MsgSpace").await;
    let channel_id = server.create_channel(&space_id, "chat").await;

    // CREATE message
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &auth,
        &serde_json::json!({ "content": "hello world" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let msg_id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["content"], "hello world");
    assert_eq!(body["data"]["author_id"], alice.user.id);

    // GET message
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{channel_id}/messages/{msg_id}"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["content"], "hello world");

    // PATCH message — check edited_at is set
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel_id}/messages/{msg_id}"),
        &auth,
        &serde_json::json!({ "content": "hello world (edited)" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["content"], "hello world (edited)");
    assert!(
        !body["data"]["edited_at"].is_null(),
        "edited_at should be set after edit"
    );

    // DELETE message
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{channel_id}/messages/{msg_id}"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_messages_with_cursor() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "CursorSpace").await;
    let channel_id = server.create_channel(&space_id, "chat").await;

    // Seed 3 messages
    for i in 1..=3 {
        let app = server.router();
        let req = authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel_id}/messages"),
            &auth,
            &serde_json::json!({ "content": format!("msg {i}") }),
        );
        app.oneshot(req).await.unwrap();
    }

    // List with limit=2
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{channel_id}/messages?limit=2"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let messages = body["data"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(body["cursor"]["has_more"], true);
}

#[tokio::test]
async fn test_other_user_cannot_edit_message() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "MsgSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let channel_id = server.create_channel(&space_id, "chat").await;

    // Alice creates a message
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "Alice's message" }),
    );
    let response = app.oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let msg_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob tries to edit Alice's message → 403
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel_id}/messages/{msg_id}"),
        &bob.auth_header(),
        &serde_json::json!({ "content": "hacked!" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// =========================================================================
// Public Spaces & Space-Level Invites
// =========================================================================

#[tokio::test]
async fn test_create_public_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "Open Community", "public": true }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["name"], "Open Community");
    assert_eq!(body["data"]["public"], true);
}

#[tokio::test]
async fn test_space_defaults_to_private() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "Private Space" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["public"], false);
}

#[tokio::test]
async fn test_update_space_public_flag() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "MySpace").await;

    // Verify starts as private
    let app = server.router();
    let req = authenticated_request(Method::GET, &format!("/api/v1/spaces/{space_id}"), &auth);
    let response = app.oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    assert_eq!(body["data"]["public"], false);

    // Update to public
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &auth,
        &serde_json::json!({ "public": true }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["public"], true);

    // Update back to private
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &auth,
        &serde_json::json!({ "public": false }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["public"], false);
}

#[tokio::test]
async fn test_create_space_level_invite() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "InviteSpace").await;

    // Create a space-level invite (no channel)
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &auth,
        &serde_json::json!({}),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["space_id"], space_id);
    assert!(
        body["data"]["channel_id"].is_null(),
        "space-level invite should have null channel_id"
    );
    assert_eq!(body["data"]["inviter_id"], alice.user.id);
}

#[tokio::test]
async fn test_channel_invite_still_has_channel_id() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "InviteSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Create a channel-level invite
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/invites"),
        &auth,
        &serde_json::json!({}),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["channel_id"], channel_id);
    assert_eq!(body["data"]["space_id"], space_id);
}

#[tokio::test]
async fn test_space_level_invite_appears_in_list() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "InviteSpace").await;

    // Create a space-level invite
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &auth,
        &serde_json::json!({}),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // List space invites — should include the space-level invite
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let invites = body["data"].as_array().unwrap();
    assert_eq!(invites.len(), 1);
    assert!(invites[0]["channel_id"].is_null());
}

#[tokio::test]
async fn test_accept_space_level_invite() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "InviteSpace").await;

    // Alice creates a space-level invite
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &alice.auth_header(),
        &serde_json::json!({}),
    );
    let response = app.oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let code = body["data"]["code"].as_str().unwrap().to_string();

    // Bob accepts the invite
    let app = server.router();
    let req = authenticated_request(
        Method::POST,
        &format!("/api/v1/invites/{code}/accept"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify Bob is now a member
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/members/{}", bob.user.id),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_join_public_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server
        .create_public_space(&alice.user.id, "Open Space")
        .await;

    // Bob joins the public space without an invite
    let app = server.router();
    let req = authenticated_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/join"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["space_id"], space_id);

    // Verify Bob is now a member
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/members/{}", bob.user.id),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_join_private_space_forbidden() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Private Space").await;

    // Bob tries to join a private space
    let app = server.router();
    let req = authenticated_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/join"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_banned_user_cannot_join_public_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server
        .create_public_space(&alice.user.id, "Open Space")
        .await;

    // Alice bans Bob
    server
        .ban_user(&space_id, &bob.user.id, &alice.user.id)
        .await;

    // Bob tries to join — should be forbidden
    let app = server.router();
    let req = authenticated_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/join"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_join_public_space_idempotent() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server
        .create_public_space(&alice.user.id, "Open Space")
        .await;

    // Bob joins twice — both should succeed (INSERT OR IGNORE)
    for _ in 0..2 {
        let app = server.router();
        let req = authenticated_request(
            Method::POST,
            &format!("/api/v1/spaces/{space_id}/join"),
            &bob.auth_header(),
        );
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn test_join_nonexistent_space_returns_404() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let app = server.router();
    let req = authenticated_request(
        Method::POST,
        "/api/v1/spaces/99999/join",
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// =========================================================================
// Gateway (WebSocket)
// =========================================================================

#[tokio::test]
async fn test_gateway_identify_ready_flow() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "GW Space").await;

    let base_url = server.spawn().await;
    let ws_url = base_url.replace("http://", "ws://");

    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();

    // Receive HELLO
    let msg = ws.next().await.unwrap().unwrap();
    let hello: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(hello["op"], 5);

    // Send IDENTIFY
    let identify = serde_json::json!({
        "op": 2,
        "data": {
            "token": alice.gateway_token(),
            "intents": ["messages"]
        }
    });
    ws.send(Message::Text(identify.to_string().into()))
        .await
        .unwrap();

    // Receive READY
    let msg = ws.next().await.unwrap().unwrap();
    let ready: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(ready["op"], 0);
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["data"]["user_id"], alice.user.id);
    let spaces = ready["data"]["spaces"].as_array().unwrap();
    assert!(
        spaces.iter().any(|s| s.as_str() == Some(&space_id)),
        "READY should include user's space"
    );

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_gateway_bot_identify() {
    let server = TestServer::new().await;
    let (_owner, bot) = server.create_bot_with_token("botowner", "MyBot").await;

    let base_url = server.spawn().await;
    let ws_url = base_url.replace("http://", "ws://");

    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();

    // Consume HELLO
    let _ = ws.next().await.unwrap().unwrap();

    // Send IDENTIFY with bot token
    let identify = serde_json::json!({
        "op": 2,
        "data": {
            "token": bot.gateway_token(),
            "intents": ["messages"]
        }
    });
    ws.send(Message::Text(identify.to_string().into()))
        .await
        .unwrap();

    // Receive READY
    let msg = ws.next().await.unwrap().unwrap();
    let ready: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(ready["op"], 0);
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["data"]["user_id"], bot.user.id);

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn test_gateway_heartbeat_ack() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let base_url = server.spawn().await;
    let ws_url = base_url.replace("http://", "ws://");

    let (mut ws, _) = connect_async(format!("{ws_url}/ws")).await.unwrap();

    // Consume HELLO
    let _ = ws.next().await.unwrap().unwrap();

    // IDENTIFY
    let identify = serde_json::json!({
        "op": 2,
        "data": {
            "token": alice.gateway_token(),
            "intents": ["messages"]
        }
    });
    ws.send(Message::Text(identify.to_string().into()))
        .await
        .unwrap();

    // Consume READY
    let _ = ws.next().await.unwrap().unwrap();

    // Send HEARTBEAT (op=1)
    let heartbeat = serde_json::json!({ "op": 1 });
    ws.send(Message::Text(heartbeat.to_string().into()))
        .await
        .unwrap();

    // Expect HEARTBEAT_ACK (op=4)
    let msg = ws.next().await.unwrap().unwrap();
    let ack: serde_json::Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
    assert_eq!(ack["op"], 4, "expected HEARTBEAT_ACK opcode (4)");

    ws.close(None).await.unwrap();
}

// =========================================================================
// Authorization enforcement
// =========================================================================

#[tokio::test]
async fn test_non_member_cannot_access_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Secret Space").await;

    // Bob (not a member) tries to GET the space → 403
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}"),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_member_without_permission_cannot_create_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Bob (member, but no manage_channels permission) tries to create a channel → 403
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/channels"),
        &bob.auth_header(),
        &serde_json::json!({ "name": "bobs-channel", "type": "text" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_member_can_send_message_with_default_permissions() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    let channel_id = server.create_channel(&space_id, "chat").await;

    // Bob (member with default @everyone permissions) sends a message → 200
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &bob.auth_header(),
        &serde_json::json!({ "content": "hello from bob" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["content"], "hello from bob");
}

#[tokio::test]
async fn test_non_member_cannot_send_message() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_id = server.create_channel(&space_id, "chat").await;

    // Bob (not a member) tries to send a message → 403
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &bob.auth_header(),
        &serde_json::json!({ "content": "unauthorized" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_member_cannot_kick_without_permission() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    server.add_member(&space_id, &charlie.user.id).await;

    // Bob (member, no kick_members permission) tries to kick Charlie → 403
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/members/{}", charlie.user.id),
        &bob.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_owner_can_kick_member() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Alice (owner, implicit administrator) kicks Bob → 200
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/members/{}", bob.user.id),
        &alice.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_admin_role_can_kick_member() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let admin_user = server.create_user_with_token("admin_user").await;
    let target = server.create_user_with_token("target").await;
    let space_id = server.create_space(&owner.user.id, "TestSpace").await;
    server.add_member(&space_id, &admin_user.user.id).await;
    server.add_member(&space_id, &target.user.id).await;

    // Get the default Admin role (created at space creation, position 2)
    let roles = accordserver::db::roles::list_roles(server.pool(), &space_id)
        .await
        .unwrap();
    let admin_role = roles
        .iter()
        .find(|r| r.name == "Admin")
        .expect("Admin role should exist");

    // Assign Admin role to admin_user
    server
        .assign_role(&space_id, &admin_user.user.id, &admin_role.id)
        .await;

    // admin_user (with Admin role, position 2) kicks target (no roles, position 0) → 200
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/members/{}", target.user.id),
        &admin_user.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_instance_admin_can_kick_member() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let admin = server.create_admin_with_token("instance_admin").await;
    let target = server.create_user_with_token("target").await;
    let space_id = server.create_space(&owner.user.id, "TestSpace").await;
    server.add_member(&space_id, &admin.user.id).await;
    server.add_member(&space_id, &target.user.id).await;

    // Instance admin (is_admin=true, but no space roles) kicks target → should be 200
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/members/{}", target.user.id),
        &admin.auth_header(),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_non_member_cannot_create_ban() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &charlie.user.id).await;

    // Bob (not a member) tries to ban Charlie → 403
    let app = server.router();
    let req = authenticated_json_request(
        Method::PUT,
        &format!("/api/v1/spaces/{space_id}/bans/{}", charlie.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "reason": "no authority" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// =========================================================================
// Rate limiting
// =========================================================================

#[tokio::test]
async fn test_rate_limit_headers_present() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &alice.auth_header());
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-ratelimit-limit"));
    assert!(response.headers().contains_key("x-ratelimit-remaining"));
    assert!(response.headers().contains_key("x-ratelimit-reset"));
}

#[tokio::test]
async fn test_rate_limit_returns_429() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    // Exhaust the rate limit (capacity = 70)
    for _ in 0..70 {
        let app = server.router();
        let req = authenticated_request(Method::GET, "/api/v1/users/@me", &alice.auth_header());
        let response = app.oneshot(req).await.unwrap();
        assert_ne!(
            response.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "should not be rate limited yet"
        );
    }

    // 71st request should be rate limited
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &alice.auth_header());
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key("retry-after"));
}

// =========================================================================
// Auth (register / login / logout)
// =========================================================================

#[tokio::test]
async fn test_register_and_login() {
    let server = TestServer::new().await;

    // Register a new user
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123",
            "display_name": "Alice"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let reg_token = body["data"]["token"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["user"]["username"], "alice");
    assert_eq!(body["data"]["user"]["display_name"], "Alice");

    // Use the registration token to GET /users/@me
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        "/api/v1/users/@me",
        &format!("Bearer {reg_token}"),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["username"], "alice");

    // Login with the same credentials
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let login_token = body["data"]["token"].as_str().unwrap().to_string();
    assert_ne!(login_token, reg_token, "login should produce a new token");

    // Verify the new token works
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        "/api/v1/users/@me",
        &format!("Bearer {login_token}"),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_register_duplicate_username() {
    let server = TestServer::new().await;

    // Register first user
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Register again with same username → 409
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "differentpassword1"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn test_register_short_password() {
    let server = TestServer::new().await;

    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "short"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_login_wrong_password() {
    let server = TestServer::new().await;

    // Register
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Login with wrong password → 401
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": "alice",
            "password": "wrongpassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_login_nonexistent_user() {
    let server = TestServer::new().await;

    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": "nobody",
            "password": "doesntmatter1"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_logout_invalidates_token() {
    let server = TestServer::new().await;

    // Register
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let token = body["data"]["token"].as_str().unwrap().to_string();
    let auth = format!("Bearer {token}");

    // Logout
    let app = server.router();
    let req = authenticated_request(Method::POST, "/api/v1/auth/logout", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Use the same token → 401
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// =========================================================================
// Slugs
// =========================================================================

#[tokio::test]
async fn test_space_auto_generated_slug() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "My Cool Space" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["slug"], "my-cool-space");
}

#[tokio::test]
async fn test_space_custom_slug() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "My Space", "slug": "custom-slug" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["slug"], "custom-slug");
}

#[tokio::test]
async fn test_space_slug_uniqueness() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    // Create first space
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "Duplicate" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["slug"], "duplicate");

    // Create second space with same name → slug gets -2 suffix
    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "Duplicate" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["slug"], "duplicate-2");
}

#[tokio::test]
async fn test_space_invalid_slug_rejected() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();

    let app = server.router();
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &auth,
        &serde_json::json!({ "name": "Test", "slug": "INVALID SLUG!" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_get_space_by_slug() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "Slug Lookup").await;

    // GET by slug instead of ID
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/spaces/slug-lookup", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["id"], space_id);
    assert_eq!(body["data"]["slug"], "slug-lookup");
}

#[tokio::test]
async fn test_update_space_slug() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let auth = alice.auth_header();
    let space_id = server.create_space(&alice.user.id, "Original").await;

    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}"),
        &auth,
        &serde_json::json!({ "slug": "new-slug" }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["slug"], "new-slug");

    // Verify the new slug works for lookup
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/spaces/new-slug", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["id"], space_id);
}

// =========================================================================
// Admin API
// =========================================================================

#[tokio::test]
async fn test_admin_list_spaces() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();

    // Create a couple of spaces
    server.create_space(&admin.user.id, "Space A").await;
    server.create_space(&admin.user.id, "Space B").await;

    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/admin/spaces", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let spaces = body["data"].as_array().unwrap();
    assert_eq!(spaces.len(), 2);
}

#[tokio::test]
async fn test_admin_list_spaces_non_admin_forbidden() {
    let server = TestServer::new().await;
    let user = server.create_user_with_token("regular").await;
    let auth = user.auth_header();

    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/admin/spaces", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_admin_update_space_owner_transfer() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;

    let space_id = server.create_space(&admin.user.id, "Transfer Test").await;

    // Transfer ownership to bob
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/spaces/{space_id}"),
        &auth,
        &serde_json::json!({ "owner_id": bob.user.id }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["owner_id"], bob.user.id);

    // Verify bob was added as member
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/members/{}", bob.user.id),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_admin_list_users() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    server.create_user_with_token("alice").await;
    server.create_user_with_token("bob").await;

    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/admin/users", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let users = body["data"].as_array().unwrap();
    assert_eq!(users.len(), 3); // admin + alice + bob
}

#[tokio::test]
async fn test_admin_list_users_with_search() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    server.create_user_with_token("alice").await;
    server.create_user_with_token("bob").await;

    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        "/api/v1/admin/users?search=ali",
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let users = body["data"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["username"], "alice");
}

#[tokio::test]
async fn test_admin_list_users_pagination() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    server.create_user_with_token("alice").await;
    server.create_user_with_token("bob").await;
    server.create_user_with_token("charlie").await;

    // Request with limit=2
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/admin/users?limit=2", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let users = body["data"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    assert!(body["cursor"]["has_more"].as_bool().unwrap());

    // Request next page
    let after = body["cursor"]["after"].as_str().unwrap();
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/admin/users?limit=2&after={after}"),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let users = body["data"].as_array().unwrap();
    assert_eq!(users.len(), 2);
}

#[tokio::test]
async fn test_admin_toggle_admin_flag() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;

    // Promote bob to admin
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
        &serde_json::json!({ "is_admin": true }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["is_admin"], true);

    // Demote bob
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
        &serde_json::json!({ "is_admin": false }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["is_admin"], false);
}

#[tokio::test]
async fn test_admin_self_demotion_prevented() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();

    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", admin.user.id),
        &auth,
        &serde_json::json!({ "is_admin": false }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_admin_last_admin_protection() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;

    // Promote bob
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
        &serde_json::json!({ "is_admin": true }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Now demote bob — should work since admin is still an admin
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
        &serde_json::json!({ "is_admin": false }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_admin_disable_user() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;

    // Disable bob
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
        &serde_json::json!({ "disabled": true }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["disabled"], true);

    // Disabled user gets 401 on next request
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &bob.auth_header());
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_admin_delete_user() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;

    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify user is gone
    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/users/{}", bob.user.id),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_admin_delete_user_owns_spaces_rejected() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;
    server.create_space(&bob.user.id, "Bob's Space").await;

    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_admin_delete_self_prevented() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();

    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/admin/users/{}", admin.user.id),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_admin_delete_admin_user_prevented() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    let bob = server.create_user_with_token("bob").await;

    // Promote bob
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
        &serde_json::json!({ "is_admin": true }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Try to delete bob (admin) → should fail
    let app = server.router();
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/admin/users/{}", bob.user.id),
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_registration_policy_closed() {
    let server = TestServer::new().await;

    // Set policy to closed
    sqlx::query("UPDATE server_settings SET registration_policy = 'closed' WHERE id = 1")
        .execute(server.pool())
        .await
        .unwrap();
    // Reload settings into state
    let settings = accordserver::db::settings::get_settings(server.pool())
        .await
        .unwrap();
    server
        .state
        .settings
        .store(std::sync::Arc::new(settings));

    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "newuser",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_registration_policy_open() {
    let server = TestServer::new().await;

    // Default policy is open
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "newuser",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_settings_new_fields_roundtrip() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();

    // Update new settings fields via admin endpoint
    let app = server.router();
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/admin/settings",
        &auth,
        &serde_json::json!({
            "server_name": "My Server",
            "registration_policy": "invite_only",
            "max_spaces": 100,
            "max_members_per_space": 500,
            "motd": "Welcome to the server!",
            "public_listing": true
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["server_name"], "My Server");
    assert_eq!(body["data"]["registration_policy"], "invite_only");
    assert_eq!(body["data"]["max_spaces"], 100);
    assert_eq!(body["data"]["max_members_per_space"], 500);
    assert_eq!(body["data"]["motd"], "Welcome to the server!");
    assert_eq!(body["data"]["public_listing"], true);

    // Read them back via admin endpoint (full settings)
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/admin/settings", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["server_name"], "My Server");
    assert_eq!(body["data"]["motd"], "Welcome to the server!");

    // Public settings endpoint returns subset
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/settings", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["server_name"], "My Server");
    assert_eq!(body["data"]["motd"], "Welcome to the server!");
    // Public endpoint should NOT include admin-only fields
    assert!(body["data"]["max_spaces"].is_null());
    assert!(body["data"]["max_members_per_space"].is_null());
}

#[tokio::test]
async fn test_force_password_reset_in_login_response() {
    let server = TestServer::new().await;

    // Register a user
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Set force_password_reset flag
    sqlx::query("UPDATE users SET force_password_reset = 1 WHERE username = 'alice'")
        .execute(server.pool())
        .await
        .unwrap();

    // Login — should include force_password_reset in response
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["force_password_reset"], true);
}

#[tokio::test]
async fn test_disabled_user_cannot_login() {
    let server = TestServer::new().await;

    // Register a user
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Disable the user
    sqlx::query("UPDATE users SET disabled = 1 WHERE username = 'alice'")
        .execute(server.pool())
        .await
        .unwrap();

    // Login attempt → 403
    let app = server.router();
    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": "alice",
            "password": "securepassword123"
        }),
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_admin_settings_alias() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();

    // GET /admin/settings should return the same as /settings
    let app = server.router();
    let req = authenticated_request(Method::GET, "/api/v1/admin/settings", &auth);
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert!(body["data"]["server_name"].is_string());
}

#[tokio::test]
async fn test_admin_list_spaces_with_search() {
    let server = TestServer::new().await;
    let admin = server.create_admin_with_token("admin").await;
    let auth = admin.auth_header();
    server.create_space(&admin.user.id, "Alpha Space").await;
    server.create_space(&admin.user.id, "Beta Space").await;

    let app = server.router();
    let req = authenticated_request(
        Method::GET,
        "/api/v1/admin/spaces?search=Alpha",
        &auth,
    );
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let spaces = body["data"].as_array().unwrap();
    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0]["name"], "Alpha Space");
}
