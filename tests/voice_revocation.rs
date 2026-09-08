//! Every access-revocation path must eject the user from LiveKit, not just from
//! the database. A participant who keeps their WebRTC session after being
//! kicked, banned, removed or disabled goes on sending and receiving media in a
//! room they no longer have access to.
mod common;

use common::{authenticated_json_request, authenticated_request, parse_body, TestServer};
use http::{Method, StatusCode};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// A stand-in for LiveKit's Twirp RoomService that records every eviction.
/// Bodies are protobuf, so requests are matched on the raw bytes: room names
/// and participant identities appear verbatim in the encoded message.
struct LiveKitMock {
    removals: Arc<Mutex<Vec<Vec<u8>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl LiveKitMock {
    async fn start() -> (Self, String) {
        let removals = Arc::new(Mutex::new(Vec::new()));
        let recorded = removals.clone();
        let app = axum::Router::new().route(
            "/twirp/livekit.RoomService/RemoveParticipant",
            axum::routing::post(move |body: axum::body::Bytes| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().unwrap().push(body.to_vec());
                    (StatusCode::OK, "")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (Self { removals, task }, url)
    }

    /// True when the mock saw a removal naming both this room and this identity.
    fn evicted(&self, channel_id: &str, user_id: &str) -> bool {
        let room = format!("channel_{channel_id}");
        self.removals
            .lock()
            .unwrap()
            .iter()
            .any(|body| contains(body, room.as_bytes()) && contains(body, user_id.as_bytes()))
    }
}

impl Drop for LiveKitMock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A test server wired to `mock` instead of the in-process voice stub.
async fn server_with_livekit(url: &str) -> TestServer {
    let mut server = TestServer::new().await;
    server.state.test_mode = false;
    server.state.livekit_client = Some(accordserver::voice::livekit::LiveKitClient::new(
        url,
        url,
        "test",
        "mock-secret",
    ));
    server
}

fn join(server: &TestServer, user_id: &str, space_id: Option<&str>, channel_id: &str) {
    accordserver::voice::state::join_voice_channel(
        &server.state,
        user_id,
        space_id,
        channel_id,
        "session",
        false,
        false,
        false,
        false,
    );
}

async fn assert_ok(server: &TestServer, request: http::Request<axum::body::Body>) {
    let response = server.router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn kick_evicts_the_member_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/spaces/{space}/members/{}", member.user.id),
            &owner.auth_header(),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn ban_evicts_the_member_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_json_request(
            Method::PUT,
            &format!("/api/v1/spaces/{space}/bans/{}", member.user.id),
            &owner.auth_header(),
            &json!({ "reason": "spam" }),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn leaving_a_space_evicts_the_member_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/spaces/{space}/members/@me"),
            &member.auth_header(),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn deleting_a_space_evicts_everyone_in_its_voice_channels() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &owner.user.id, Some(&space), &channel);
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/spaces/{space}"),
            &owner.auth_header(),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &owner.user.id));
    assert!(mock.evicted(&channel, &member.user.id));
    assert!(server.state.voice_states.is_empty());
}

#[tokio::test]
async fn deleting_a_voice_channel_evicts_its_participants() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/channels/{channel}"),
            &owner.auth_header(),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn removing_a_group_dm_recipient_evicts_them_from_the_call() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let owner = server.create_user_with_token("owner").await;
    let guest = server.create_user_with_token("guest").await;
    let third = server.create_user_with_token("third").await;

    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            "/api/v1/users/@me/channels",
            &owner.auth_header(),
            &json!({ "recipients": [guest.user.id, third.user.id] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let channel = parse_body(response).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // DM calls have no parent space.
    join(&server, &guest.user.id, None, &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/channels/{channel}/recipients/{}", guest.user.id),
            &owner.auth_header(),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &guest.user.id));
    assert!(!server.state.voice_states.contains_key(&guest.user.id));
}

#[tokio::test]
async fn disabling_an_account_evicts_it_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let admin = server.create_admin_with_token("admin").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&member.user.id, "space").await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_json_request(
            Method::PATCH,
            &format!("/api/v1/admin/users/{}", member.user.id),
            &admin.auth_header(),
            &json!({ "disabled": true }),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn deleting_an_account_evicts_it_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let server = server_with_livekit(&url).await;
    let admin = server.create_admin_with_token("admin").await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/admin/users/{}", member.user.id),
            &admin.auth_header(),
        ),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

/// The MCP admin surface is mounted at `/mcp`, outside `/api/v1`, so nothing
/// there passes through the rate-limit layer that opportunistically runs the
/// reconcile sweep. Moderation performed over MCP has to revoke voice itself.
async fn mcp_call(server: &TestServer, tool: &str, args: serde_json::Value) {
    let request = authenticated_json_request(
        Method::POST,
        "/mcp",
        "Bearer mcp-test-key",
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }),
    );
    let response = server.router().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert!(body["error"].is_null(), "MCP call failed: {body}");
    assert!(
        body["result"]["isError"].as_bool() != Some(true),
        "MCP tool reported an error: {body}"
    );
}

#[tokio::test]
async fn mcp_kick_evicts_the_member_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let mut server = server_with_livekit(&url).await;
    server.state.mcp_api_key = Some("mcp-test-key".to_string());
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    mcp_call(
        &server,
        "kick_member",
        json!({ "space_id": space, "user_id": member.user.id }),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn mcp_ban_evicts_the_member_from_livekit() {
    let (mock, url) = LiveKitMock::start().await;
    let mut server = server_with_livekit(&url).await;
    server.state.mcp_api_key = Some("mcp-test-key".to_string());
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    mcp_call(
        &server,
        "ban_user",
        json!({ "space_id": space, "user_id": member.user.id, "reason": "spam" }),
    )
    .await;

    assert!(mock.evicted(&channel, &member.user.id));
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

/// A failed eviction is not lost: it stays queued so the periodic reconcile
/// worker can retry it, and local voice state is cleared either way.
#[tokio::test]
async fn a_failed_eviction_stays_queued_for_retry() {
    let mut server = TestServer::new().await;
    // No LiveKit client and not in test mode: evictions can never be acknowledged.
    server.state.test_mode = false;
    server.state.livekit_client = None;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_voice_channel(&space, "voice").await;
    join(&server, &member.user.id, Some(&space), &channel);

    assert_ok(
        &server,
        authenticated_request(
            Method::DELETE,
            &format!("/api/v1/spaces/{space}/members/{}", member.user.id),
            &owner.auth_header(),
        ),
    )
    .await;

    assert!(!server.state.voice_states.contains_key(&member.user.id));
    // `db::q` rewrites the `?` placeholders for Postgres.
    let queued: i64 = sqlx::query_scalar(&accordserver::db::q(
        "SELECT COUNT(*) FROM voice_evictions WHERE channel_id = ? AND user_id = ?",
    ))
    .bind(&channel)
    .bind(&member.user.id)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(queued, 1);
}
