//! A space ban must not leave the banned user a DM side channel to the
//! members of that space, and a block must stop sends in an existing DM.

mod common;

use accordserver::db;
use axum::body::Body;
use common::{
    authenticated_json_request, build_multipart_upload_body, parse_body, TestServer, TestUser,
};
use http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const DM_REFUSED: &str = "you cannot send a DM to this user";

async fn open_dm(server: &TestServer, from: &TestUser, to: &TestUser) -> (StatusCode, Value) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            "/api/v1/users/@me/channels",
            &from.auth_header(),
            &json!({ "recipient_id": to.user.id }),
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, parse_body(response).await)
}

async fn send(server: &TestServer, from: &TestUser, channel_id: &str) -> (StatusCode, Value) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel_id}/messages"),
            &from.auth_header(),
            &json!({ "content": "hello" }),
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, parse_body(response).await)
}

fn error_message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

/// The consent gate passed: the send then fails only on the (absent) E2EE
/// envelope, which every DM requires.
fn assert_passed_consent(status: StatusCode, body: &Value) {
    assert_ne!(status, StatusCode::FORBIDDEN, "unexpected refusal: {body}");
    assert_ne!(error_message(body), DM_REFUSED);
}

/// owner runs `space`; `member` is in it; `banned` was banned from it.
async fn banned_setup(server: &TestServer) -> (TestUser, TestUser, TestUser, String) {
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let banned = server.create_user_with_token("banned").await;
    let space = server.create_space(&owner.user.id, "Space").await;
    server.add_member(&space, &member.user.id).await;
    server.add_member(&space, &banned.user.id).await;
    server
        .ban_user(&space, &banned.user.id, &owner.user.id)
        .await;
    (owner, member, banned, space)
}

#[tokio::test]
async fn banned_user_cannot_open_dm_with_member_of_banning_space() {
    let server = TestServer::new().await;
    let (_owner, member, banned, _space) = banned_setup(&server).await;

    let (status, body) = open_dm(&server, &banned, &member).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_message(&body), DM_REFUSED);

    // An unrelated user is not affected by the ban.
    let stranger = server.create_user_with_token("stranger").await;
    let (status, _) = open_dm(&server, &banned, &stranger).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn shared_space_or_friendship_still_allows_dm() {
    let server = TestServer::new().await;
    let (owner, member, banned, _space) = banned_setup(&server).await;

    // Sharing another space keeps them connected.
    let other_space = server.create_space(&owner.user.id, "Other").await;
    server.add_member(&other_space, &member.user.id).await;
    server.add_member(&other_space, &banned.user.id).await;
    let (status, _) = open_dm(&server, &banned, &member).await;
    assert_eq!(status, StatusCode::OK);

    // So does a friendship, even with no shared space.
    let friend = server.create_user_with_token("friend").await;
    let space = server.create_space(&owner.user.id, "Third").await;
    server.add_member(&space, &friend.user.id).await;
    server.add_member(&space, &banned.user.id).await;
    server
        .ban_user(&space, &banned.user.id, &owner.user.id)
        .await;
    // Friend and banned share no space now, so only the friendship connects them.
    assert!(
        db::bans::ban_separates(server.pool(), &banned.user.id, &friend.user.id)
            .await
            .unwrap()
    );
    db::relationships::upsert_relationship(server.pool(), &banned.user.id, &friend.user.id, 1)
        .await
        .unwrap();
    db::relationships::upsert_relationship(server.pool(), &friend.user.id, &banned.user.id, 1)
        .await
        .unwrap();
    let (status, _) = open_dm(&server, &banned, &friend).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn existing_dm_send_is_refused_after_ban_but_recipient_can_reply() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let banned = server.create_user_with_token("banned").await;
    let space = server.create_space(&owner.user.id, "Space").await;
    server.add_member(&space, &member.user.id).await;
    server.add_member(&space, &banned.user.id).await;
    let dm = server.create_dm(&banned.user.id, &member.user.id).await;

    // Before the ban, the gate lets the send through.
    let (status, body) = send(&server, &banned, &dm).await;
    assert_passed_consent(status, &body);

    server
        .ban_user(&space, &banned.user.id, &owner.user.id)
        .await;

    let (status, body) = send(&server, &banned, &dm).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_message(&body), DM_REFUSED);

    // The multipart upload path is gated the same way.
    let boundary = "dmbanboundary";
    let upload = build_multipart_upload_body(
        boundary,
        &json!({ "content": "hello" }),
        "attachment-0.bin",
        "application/octet-stream",
        &[0u8; 64],
    );
    let response = server
        .router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/v1/channels/{dm}/messages/upload"))
                .header("Authorization", banned.auth_header())
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(upload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_message(&parse_body(response).await), DM_REFUSED);

    // The recipient is not punished for the sender's ban.
    let (status, body) = send(&server, &member, &dm).await;
    assert_passed_consent(status, &body);
}

#[tokio::test]
async fn existing_dm_send_is_refused_after_recipient_blocks_sender() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let dm = server.create_dm(&alice.user.id, &bob.user.id).await;

    // Bob blocks Alice after the DM already exists.
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PUT,
            &format!("/api/v1/users/@me/relationships/{}", alice.user.id),
            &bob.auth_header(),
            &json!({ "type": 2 }),
        ))
        .await
        .unwrap();
    assert!(response.status().is_success(), "block failed");
    assert!(
        db::relationships::is_blocked_by(server.pool(), &bob.user.id, &alice.user.id)
            .await
            .unwrap()
    );

    let (status, body) = send(&server, &alice, &dm).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_message(&body), DM_REFUSED);

    // The blocker can still write.
    let (status, body) = send(&server, &bob, &dm).await;
    assert_passed_consent(status, &body);
}

#[tokio::test]
async fn group_dm_send_is_not_gated() {
    let server = TestServer::new().await;
    let (_owner, member, banned, _space) = banned_setup(&server).await;
    let third = server.create_user_with_token("third").await;
    let group = db::dm_participants::create_dm_channel(
        server.pool(),
        &banned.user.id,
        &[member.user.id.clone(), third.user.id.clone()],
        server.state.db_is_postgres,
    )
    .await
    .unwrap();

    let (status, body) = send(&server, &banned, &group.id).await;
    assert_passed_consent(status, &body);
}
