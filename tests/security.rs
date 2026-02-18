mod common;

use common::{
    authenticated_json_request, authenticated_request, json_request, parse_body, TestServer,
};
use http::{Method, StatusCode};
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
