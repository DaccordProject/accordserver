mod common;

use common::{
    authenticated_json_request, authenticated_request, json_request, parse_body, TestServer,
};
use http::{Method, StatusCode};
use serde_json::json;
use tower::ServiceExt;

// =========================================================================
// 1. Authorization Tests
//
// Pattern: Alice creates resources in her space, Bob (a non-member outsider)
// attempts privileged operations. These tests verify that non-members are
// rejected with 403 FORBIDDEN.
// =========================================================================

#[tokio::test]
async fn test_non_member_can_delete_others_message() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice sends a message
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "secret message" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let msg_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob (non-member) tries to delete Alice's message → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{channel_id}/messages/{msg_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_pin_message() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice sends a message
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "pin me" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let msg_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob (non-member) tries to pin Alice's message → 403
    let req = authenticated_request(
        Method::PUT,
        &format!("/api/v1/channels/{channel_id}/pins/{msg_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_unpin_message() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice sends and pins a message
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "pinned msg" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let msg_id = body["data"]["id"].as_str().unwrap().to_string();

    let req = authenticated_request(
        Method::PUT,
        &format!("/api/v1/channels/{channel_id}/pins/{msg_id}"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Bob (non-member) tries to unpin the message → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{channel_id}/pins/{msg_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_bulk_delete_messages() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice sends messages
    let mut msg_ids = Vec::new();
    for i in 0..3 {
        let req = authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel_id}/messages"),
            &alice.auth_header(),
            &serde_json::json!({ "content": format!("msg {i}") }),
        );
        let response = server.router().oneshot(req).await.unwrap();
        let body = parse_body(response).await;
        msg_ids.push(body["data"]["id"].as_str().unwrap().to_string());
    }

    // Bob (non-member) tries to bulk delete Alice's messages → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages/bulk-delete"),
        &bob.auth_header(),
        &serde_json::json!({ "messages": msg_ids }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_update_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Bob (non-member) tries to rename Alice's channel → 403
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel_id}"),
        &bob.auth_header(),
        &serde_json::json!({ "name": "hacked-channel" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_delete_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Bob (non-member) tries to delete Alice's channel → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/channels/{channel_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_create_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Bob (non-member) tries to create a channel in Alice's space → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/channels"),
        &bob.auth_header(),
        &serde_json::json!({ "name": "bobs-backdoor", "type": "text" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_kick_member() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    server.add_member(&space_id, &charlie.user.id).await;

    // Bob (non-member) tries to kick Charlie from Alice's space → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/members/{}", charlie.user.id),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_update_member() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Bob (non-member) tries to update Alice's member profile → 403
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/members/{}", alice.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "nickname": "hacked-nickname" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_assign_role() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Alice creates a role
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &alice.auth_header(),
        &serde_json::json!({ "name": "admin" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let role_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob (non-member) tries to assign the role to Alice → 403
    let req = authenticated_request(
        Method::PUT,
        &format!(
            "/api/v1/spaces/{space_id}/members/{}/roles/{role_id}",
            alice.user.id
        ),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_remove_role() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Alice creates a role and assigns it to herself
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &alice.auth_header(),
        &serde_json::json!({ "name": "moderator" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let role_id = body["data"]["id"].as_str().unwrap().to_string();

    let req = authenticated_request(
        Method::PUT,
        &format!(
            "/api/v1/spaces/{space_id}/members/{}/roles/{role_id}",
            alice.user.id
        ),
        &alice.auth_header(),
    );
    server.router().oneshot(req).await.unwrap();

    // Bob (non-member) tries to remove the role from Alice → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!(
            "/api/v1/spaces/{space_id}/members/{}/roles/{role_id}",
            alice.user.id
        ),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_create_ban() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    server.add_member(&space_id, &charlie.user.id).await;

    // Bob (non-member) tries to ban Charlie from Alice's space → 403
    let req = authenticated_json_request(
        Method::PUT,
        &format!("/api/v1/spaces/{space_id}/bans/{}", charlie.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "reason": "hostile takeover" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_delete_ban() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Alice bans Charlie
    let req = authenticated_json_request(
        Method::PUT,
        &format!("/api/v1/spaces/{space_id}/bans/{}", charlie.user.id),
        &alice.auth_header(),
        &serde_json::json!({ "reason": "misbehavior" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Bob (non-member) tries to lift Charlie's ban → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/bans/{}", charlie.user.id),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_list_bans() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Bob (non-member) tries to list bans in Alice's space → 403
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/bans"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_create_role() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Bob (non-member) tries to create a role in Alice's space → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &bob.auth_header(),
        &serde_json::json!({ "name": "evil-admin" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_delete_role() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Alice creates a role
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &alice.auth_header(),
        &serde_json::json!({ "name": "moderator" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    let body = parse_body(response).await;
    let role_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob (non-member) tries to delete the role → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/roles/{role_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_delete_invite() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice creates an invite
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/invites"),
        &alice.auth_header(),
        &serde_json::json!({ "max_uses": 10 }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let code = body["data"]["code"].as_str().unwrap().to_string();

    // Bob (non-member) tries to delete the invite → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/invites/{code}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_list_space_invites() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;

    // Bob (non-member) tries to list invites for Alice's space → 403
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_can_create_invite() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Bob (non-member) tries to create an invite for Alice's channel → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/invites"),
        &bob.auth_header(),
        &serde_json::json!({ "max_uses": 50 }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// =========================================================================
// 2. Input Validation Tests
// =========================================================================

#[tokio::test]
async fn test_oversized_message_content() {
    // No content length limit on messages (documented behavior)
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let huge_content = "A".repeat(100_000);
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": huge_content }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["content"].as_str().unwrap().len(), 100_000);
}

#[tokio::test]
async fn test_oversized_username() {
    // No username length limit on PATCH /users/@me (documented behavior)
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let huge_name = "X".repeat(10_000);
    let req = authenticated_json_request(
        Method::PATCH,
        "/api/v1/users/@me",
        &alice.auth_header(),
        &serde_json::json!({ "display_name": huge_name }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["display_name"].as_str().unwrap().len(), 10_000);
}

#[tokio::test]
async fn test_oversized_space_name() {
    // No space name length limit (documented behavior)
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let huge_name = "S".repeat(10_000);
    let req = authenticated_json_request(
        Method::POST,
        "/api/v1/spaces",
        &alice.auth_header(),
        &serde_json::json!({ "name": huge_name }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_sql_injection_in_message_content() {
    // Verifies parameterized queries: SQL injection in content is stored literally
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let payload = "'; DROP TABLE messages; --";
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": payload }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let msg_id = body["data"]["id"].as_str().unwrap().to_string();

    // Verify the message was stored literally (table not dropped)
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{channel_id}/messages/{msg_id}"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert_eq!(body["data"]["content"], payload);
}

#[tokio::test]
async fn test_html_script_in_message_content() {
    // Verifies XSS payloads are stored as-is (no server-side sanitization needed
    // since Godot client doesn't render HTML, but good to document behavior)
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let payload = "<script>alert('xss')</script>";
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": payload }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    // Content stored literally — no escaping or stripping
    assert_eq!(body["data"]["content"], payload);
}

#[tokio::test]
async fn test_bulk_delete_exceeds_limit() {
    // Verifies the existing 100-message limit on bulk delete
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Send 101 fake message IDs
    let msg_ids: Vec<String> = (0..101).map(|i| format!("fake-id-{i}")).collect();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_id}/messages/bulk-delete"),
        &alice.auth_header(),
        &serde_json::json!({ "messages": msg_ids }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// =========================================================================
// 3. Authentication Edge Case Tests
// =========================================================================

#[tokio::test]
async fn test_no_auth_header() {
    // Request without Authorization header should return 401
    let server = TestServer::new().await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/api/v1/users/@me")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_empty_bearer_token() {
    // "Bearer " with empty token should return 401
    let server = TestServer::new().await;

    let req = authenticated_request(Method::GET, "/api/v1/users/@me", "Bearer ");
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_wrong_prefix() {
    // "Token <valid_token>" (wrong prefix) should return 401
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;

    let wrong_auth = format!("Token {}", alice.token);
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &wrong_auth);
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_bearer_with_bot_token() {
    // Bot token sent with "Bearer" prefix should return 401
    // (bot tokens are only valid with "Bot" prefix)
    let server = TestServer::new().await;
    let (_owner, bot) = server.create_bot_with_token("owner", "TestBot").await;

    let wrong_auth = format!("Bearer {}", bot.token);
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &wrong_auth);
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_expired_token() {
    // Token with past expires_at should return 401
    let server = TestServer::new().await;

    let user = server.create_user_with_token("alice").await;

    // Create a second token with an expired timestamp
    let expired_token = accordserver::middleware::auth::generate_token();
    let token_hash = accordserver::middleware::auth::create_token_hash(&expired_token);
    sqlx::query(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, '2020-01-01T00:00:00')",
    )
    .bind(&token_hash)
    .bind(&user.user.id)
    .execute(server.pool())
    .await
    .expect("failed to insert expired token");

    let auth = format!("Bearer {expired_token}");
    let req = authenticated_request(Method::GET, "/api/v1/users/@me", &auth);
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 4. Auth Endpoint Security Tests
// =========================================================================

#[tokio::test]
async fn test_register_empty_username() {
    let server = TestServer::new().await;

    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "",
            "password": "securepassword123"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_register_whitespace_only_username() {
    let server = TestServer::new().await;

    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "   ",
            "password": "securepassword123"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_register_username_too_long() {
    let server = TestServer::new().await;

    let long_name = "A".repeat(33);
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": long_name,
            "password": "securepassword123"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_register_password_too_long() {
    let server = TestServer::new().await;

    let long_pass = "P".repeat(129);
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "alice",
            "password": long_pass
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_logout_without_auth() {
    // Logout without an Authorization header → 401
    let server = TestServer::new().await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/logout")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_login_against_bot_user() {
    // Bot users don't have password_hash, so login should fail with 401
    let server = TestServer::new().await;
    let (_owner, bot) = server.create_bot_with_token("owner", "TestBot").await;

    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": bot.user.username,
            "password": "anypassword123"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_sql_injection_in_register_username() {
    // Verifies parameterized queries prevent SQL injection in registration
    let server = TestServer::new().await;

    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "'; DROP TABLE users; --",
            "password": "securepassword123"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    // Should succeed (the username is just a string) or fail gracefully
    assert!(
        response.status() == StatusCode::OK || response.status() == StatusCode::BAD_REQUEST,
        "SQL injection should not cause a server error"
    );

    // Verify users table still exists by creating a normal user
    let req = json_request(
        Method::POST,
        "/api/v1/auth/register",
        &serde_json::json!({
            "username": "normaluser",
            "password": "securepassword123"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_sql_injection_in_login_username() {
    // Verifies parameterized queries prevent SQL injection in login
    let server = TestServer::new().await;

    let req = json_request(
        Method::POST,
        "/api/v1/auth/login",
        &serde_json::json!({
            "username": "' OR '1'='1",
            "password": "anything12345"
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// =========================================================================
// 5. Privilege Escalation Regression Tests
//
// These tests verify fixes for authorization bypass vulnerabilities found
// during security audit. Each test targets a specific exploit path.
// =========================================================================

// -- Fix #1: update_member must not allow role/mute/deaf changes with only
//    manage_nicknames. ---------------------------------------------------

#[tokio::test]
async fn test_update_member_role_escalation_via_manage_nicknames() {
    // A member with only manage_nicknames must NOT be able to replace roles
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Give Bob only manage_nicknames
    let nickname_role = server
        .create_role(&space_id, "nicknamer", &["manage_nicknames"])
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &nickname_role)
        .await;

    // Create a powerful admin role
    let admin_role = server
        .create_role(&space_id, "admin", &["administrator"])
        .await;

    // Bob tries to assign the admin role to himself via update_member → 403
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/members/{}", bob.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "roles": [admin_role] }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_update_member_mute_requires_mute_members() {
    // A member with only manage_nicknames must NOT be able to server-mute
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    server.add_member(&space_id, &charlie.user.id).await;

    // Give Bob only manage_nicknames
    let nickname_role = server
        .create_role(&space_id, "nicknamer", &["manage_nicknames"])
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &nickname_role)
        .await;

    // Bob tries to mute Charlie via update_member → 403
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/members/{}", charlie.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "mute": true }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_update_member_deaf_requires_deafen_members() {
    // A member with only manage_nicknames must NOT be able to server-deafen
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;
    server.add_member(&space_id, &charlie.user.id).await;

    let nickname_role = server
        .create_role(&space_id, "nicknamer", &["manage_nicknames"])
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &nickname_role)
        .await;

    // Bob tries to deafen Charlie via update_member → 403
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/members/{}", charlie.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "deaf": true }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_update_member_role_respects_hierarchy() {
    // Even with manage_roles, a lower-ranked member cannot change roles on
    // a higher-ranked member
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Bob gets manage_roles at position 1 (low)
    let mod_role = server
        .create_role(&space_id, "mod", &["manage_roles", "manage_nicknames"])
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &mod_role)
        .await;

    // Alice (owner, position MAX) is above Bob — Bob cannot change Alice's roles
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/members/{}", alice.user.id),
        &bob.auth_header(),
        &serde_json::json!({ "roles": [] }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// -- Fix #2: create_role/update_role must not allow granting permissions
//    the actor doesn't hold. --------------------------------------------

#[tokio::test]
async fn test_create_role_cannot_grant_permissions_actor_lacks() {
    // A member with manage_roles (but not administrator) must NOT be able
    // to create a role with administrator permission
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Give Bob manage_roles + some basic perms, but NOT administrator
    let mod_role = server
        .create_role(
            &space_id,
            "mod",
            &["manage_roles", "view_channel", "send_messages"],
        )
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &mod_role)
        .await;

    // Bob tries to create a role with administrator → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &bob.auth_header(),
        &serde_json::json!({
            "name": "escalation-role",
            "permissions": ["administrator"]
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_create_role_rejects_unknown_permissions() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;

    // Alice (owner) tries to create a role with a bogus permission → 400
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "bogus-role",
            "permissions": ["nonexistent_permission"]
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_update_role_cannot_grant_permissions_actor_lacks() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Give Bob manage_roles
    let mod_role = server
        .create_role(&space_id, "mod", &["manage_roles", "view_channel"])
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &mod_role)
        .await;

    // Create a low-privilege role Bob can manage
    let low_role = server
        .create_role(&space_id, "low-role", &["view_channel"])
        .await;

    // Bob tries to add administrator to the low role → 403
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/roles/{low_role}"),
        &bob.auth_header(),
        &serde_json::json!({ "permissions": ["administrator"] }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_owner_can_create_role_with_administrator() {
    // Space owner (implicit administrator) CAN grant administrator
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;

    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/roles"),
        &alice.auth_header(),
        &serde_json::json!({
            "name": "super-admin",
            "permissions": ["administrator"]
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// -- Fix #3: reorder_roles/reorder_channels must not modify resources
//    belonging to other spaces. ------------------------------------------

#[tokio::test]
async fn test_reorder_roles_cannot_affect_other_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_a = server.create_space(&alice.user.id, "Space A").await;
    let space_b = server.create_space(&alice.user.id, "Space B").await;

    // Create a role in Space B
    let role_b = server
        .create_role(&space_b, "target-role", &["view_channel"])
        .await;

    // Get the role's original position
    let role_before =
        accordserver::db::roles::get_role_row(server.pool(), &role_b)
            .await
            .unwrap();

    // Alice tries to reorder Space B's role from Space A's endpoint
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_a}/roles"),
        &alice.auth_header(),
        &serde_json::json!([{ "id": role_b, "position": 99 }]),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the role in Space B was NOT changed
    let role_after =
        accordserver::db::roles::get_role_row(server.pool(), &role_b)
            .await
            .unwrap();
    assert_eq!(role_before.position, role_after.position);
}

#[tokio::test]
async fn test_reorder_channels_cannot_affect_other_space() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_a = server.create_space(&alice.user.id, "Space A").await;
    let space_b = server.create_space(&alice.user.id, "Space B").await;

    // Create a channel in Space B
    let channel_b = server.create_channel(&space_b, "secret").await;

    // Get the channel's original position
    let ch_before =
        accordserver::db::channels::get_channel_row(server.pool(), &channel_b)
            .await
            .unwrap();

    // Alice tries to reorder Space B's channel from Space A's endpoint
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_a}/channels"),
        &alice.auth_header(),
        &serde_json::json!([{ "id": channel_b, "position": 99 }]),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the channel in Space B was NOT changed
    let ch_after =
        accordserver::db::channels::get_channel_row(server.pool(), &channel_b)
            .await
            .unwrap();
    assert_eq!(ch_before.position, ch_after.position);
}

// -- Fix #4: update_role must strip position field to prevent hierarchy
//    bypass. ------------------------------------------------------------

#[tokio::test]
async fn test_update_role_ignores_position_field() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;

    // Create a role
    let role_id = server
        .create_role(&space_id, "test-role", &["view_channel"])
        .await;

    let role_before =
        accordserver::db::roles::get_role_row(server.pool(), &role_id)
            .await
            .unwrap();

    // Try to set position via update_role
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space_id}/roles/{role_id}"),
        &alice.auth_header(),
        &serde_json::json!({ "position": 999 }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Position should remain unchanged
    let role_after =
        accordserver::db::roles::get_role_row(server.pool(), &role_id)
            .await
            .unwrap();
    assert_eq!(role_before.position, role_after.position);
}

// -- Fix #5: bulk_delete_messages must not delete messages from other
//    channels. -----------------------------------------------------------

#[tokio::test]
async fn test_bulk_delete_cannot_delete_cross_channel_messages() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_a = server.create_channel(&space_id, "channel-a").await;
    let channel_b = server.create_channel(&space_id, "channel-b").await;

    // Alice sends a message in channel B
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_b}/messages"),
        &alice.auth_header(),
        &serde_json::json!({ "content": "secret message in channel B" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let msg_b_id = body["data"]["id"].as_str().unwrap().to_string();

    // Alice tries to bulk-delete channel B's message from channel A's endpoint
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel_a}/messages/bulk-delete"),
        &alice.auth_header(),
        &serde_json::json!({ "messages": [msg_b_id] }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Verify the message in channel B still exists
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{channel_b}/messages/{msg_b_id}"),
        &alice.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// -- Fix #6: invite listing must require manage_channels, not just
//    membership. ---------------------------------------------------------

#[tokio::test]
async fn test_regular_member_cannot_list_space_invites() {
    // A member without manage_channels cannot enumerate space invites
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Bob (regular member, no manage_channels) tries to list invites → 403
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_regular_member_cannot_list_channel_invites() {
    // A member without manage_channels cannot enumerate channel invites
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    let channel_id = server.create_channel(&space_id, "general").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Bob (regular member) tries to list channel invites → 403
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/channels/{channel_id}/invites"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_manager_can_list_space_invites() {
    // A member WITH manage_channels CAN list invites (positive control)
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "TestSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    let mgr_role = server
        .create_role(&space_id, "channel-mgr", &["manage_channels"])
        .await;
    server
        .assign_role(&space_id, &bob.user.id, &mgr_role)
        .await;

    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space_id}/invites"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// =========================================================================
// 7. Emoji Authorization Tests
// =========================================================================

/// A tiny 1x1 PNG as base64 data URI for testing.
fn test_png_data_uri() -> String {
    let png_bytes: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
        0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
        0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC,
        0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let b64 = simple_base64_encode(png_bytes);
    format!("data:image/png;base64,{b64}")
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
async fn test_non_member_cannot_create_emoji() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    // Bob (non-member) tries to create emoji → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &bob.auth_header(),
        &json!({
            "name": "evil_emoji",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_cannot_delete_emoji() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    // Alice creates an emoji
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
        &json!({
            "name": "test_emoji",
            "image": test_png_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let emoji_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob (non-member) tries to delete the emoji → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/emojis/{emoji_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_invalid_image_type_rejected() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "EmojiSpace").await;

    // Try to upload a BMP (unsupported type)
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/emojis"),
        &alice.auth_header(),
        &json!({
            "name": "bad_emoji",
            "image": "data:image/bmp;base64,Qk0="
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// =========================================================================
// 8. Soundboard Authorization Tests
// =========================================================================

fn test_ogg_data_uri() -> String {
    let fake_ogg: &[u8] = &[0x4F, 0x67, 0x67, 0x53, 0x00, 0x02, 0x00, 0x00];
    let b64 = simple_base64_encode(fake_ogg);
    format!("data:audio/ogg;base64,{b64}")
}

#[tokio::test]
async fn test_non_member_cannot_create_sound() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SoundSpace").await;

    // Bob (non-member) tries to create a sound → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &bob.auth_header(),
        &json!({
            "name": "evil_sound",
            "audio": test_ogg_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_member_without_manage_soundboard_cannot_create_sound() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SoundSpace").await;
    server.add_member(&space_id, &bob.user.id).await;

    // Bob (regular member without manage_soundboard) tries to create a sound → 403
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &bob.auth_header(),
        &json!({
            "name": "unauthorized_sound",
            "audio": test_ogg_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_non_member_cannot_delete_sound() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let space_id = server.create_space(&alice.user.id, "SoundSpace").await;

    // Alice creates a sound
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &alice.auth_header(),
        &json!({
            "name": "test_sound",
            "audio": test_ogg_data_uri()
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let sound_id = body["data"]["id"].as_str().unwrap().to_string();

    // Bob (non-member) tries to delete the sound → 403
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space_id}/soundboard/{sound_id}"),
        &bob.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_invalid_audio_type_rejected() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "SoundSpace").await;

    // Try to upload an unsupported audio type
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/spaces/{space_id}/soundboard"),
        &alice.auth_header(),
        &json!({
            "name": "bad_sound",
            "audio": "data:audio/flac;base64,Qk0="
        }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_owner_can_update_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    // Alice (space owner) updates her channel → should succeed
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel_id}"),
        &alice.auth_header(),
        &json!({ "name": "renamed-channel", "topic": "new topic" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "space owner should be able to update channel");
}

#[tokio::test]
async fn test_instance_admin_can_update_channel() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let admin = server.create_admin_with_token("admin").await;
    let space_id = server.create_space(&alice.user.id, "Alice's Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;
    server.add_member(&space_id, &admin.user.id).await;

    // Admin (instance admin, not space owner) updates the channel → should succeed
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel_id}"),
        &admin.auth_header(),
        &json!({ "name": "admin-renamed", "topic": "admin topic" }),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "instance admin should be able to update channel");
}
