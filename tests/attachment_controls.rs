mod common;

use accordserver::{automod, db};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    response::Response,
};
use common::{
    authenticated_json_request, authenticated_request, build_multipart_upload_body, parse_body,
    TestServer, TestUser,
};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn setup() -> (TestServer, TestUser, TestUser, String, String) {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "controls").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_channel(&space, "general").await;
    (server, owner, member, space, channel)
}
async fn send(server: &TestServer, user: &TestUser, channel: &str, content: &str) -> Response {
    server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel}/messages"),
            &user.auth_header(),
            &json!({"content":content}),
        ))
        .await
        .unwrap()
}
async fn upload(server: &TestServer, user: &TestUser, channel: &str, bytes: &[u8]) -> Response {
    let body = build_multipart_upload_body(
        "boundary",
        &json!({"content":"file"}),
        "file.bin",
        "application/octet-stream",
        bytes,
    );
    server
        .router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/api/v1/channels/{channel}/messages/upload"))
                .header("Authorization", user.auth_header())
                .header("Content-Type", "multipart/form-data; boundary=boundary")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn block(server: &TestServer, user: &TestUser, scope: &str, attachment: &str) -> Response {
    server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/automod/{scope}/attachments/{attachment}/block"),
            &user.auth_header(),
            &json!({"reason":"repeat spam"}),
        ))
        .await
        .unwrap()
}
async fn slowmode(server: &TestServer, owner: &TestUser, channel: &str, seconds: i64) -> Response {
    server
        .router()
        .oneshot(authenticated_json_request(
            Method::PATCH,
            &format!("/api/v1/channels/{channel}"),
            &owner.auth_header(),
            &json!({"rate_limit":seconds}),
        ))
        .await
        .unwrap()
}
async fn assert_limited(response: Response) {
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let seconds: u64 = response.headers()["Retry-After"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(seconds > 0);
    assert_eq!(parse_body(response).await["error"]["retry_after"], seconds);
}
async fn configure_uploads(server: &TestServer, input: Value) {
    let admin = server.create_admin_with_token("operator").await;
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PATCH,
            "/api/v1/admin/settings",
            &admin.auth_header(),
            &input,
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        parse_body(response).await
    );
}

#[tokio::test]
async fn hashes_are_persisted_and_moderators_can_block_legacy_files_without_a_model() {
    let (server, owner, member, space, channel) = setup().await;
    let response = upload(&server, &member, &channel, b"identical bytes").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let attachment = body["data"]["attachments"][0]["id"].as_str().unwrap();
    let digest = automod::hash(b"identical bytes");
    assert_eq!(body["data"]["attachments"][0]["content_hash"], digest);
    assert_eq!(
        block(&server, &member, &space, attachment).await.status(),
        StatusCode::FORBIDDEN
    );
    let moderator = server.create_user_with_token("mod").await;
    server.add_member(&space, &moderator.user.id).await;
    let role = server
        .create_role(&space, "Moderator", &["manage_messages"])
        .await;
    server.assign_role(&space, &moderator.user.id, &role).await;
    // Simulate a pre-migration attachment: the action computes its digest locally.
    sqlx::query(&db::q(
        "UPDATE attachments SET content_hash=NULL WHERE id=?",
    ))
    .bind(attachment)
    .execute(server.pool())
    .await
    .unwrap();
    assert_eq!(
        block(&server, &moderator, &space, attachment)
            .await
            .status(),
        StatusCode::OK
    );
    let saved: (String,) =
        sqlx::query_as(&db::q("SELECT content_hash FROM attachments WHERE id=?"))
            .bind(attachment)
            .fetch_one(server.pool())
            .await
            .unwrap();
    assert_eq!(saved.0, digest);
    let who: (String, i64) = sqlx::query_as(&db::q(
        "SELECT added_by,created_at FROM automod_hashes WHERE scope_id=? AND hash=?",
    ))
    .bind(&space)
    .bind(&digest)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(who.0, moderator.user.id);
    assert!(who.1 > 0);
    let message = body["data"]["id"].as_str().unwrap();
    let deleted = server
        .router()
        .oneshot(authenticated_request(
            Method::DELETE,
            &format!("/api/v1/channels/{channel}/messages/{message}"),
            &moderator.auth_header(),
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(
        upload(&server, &member, &channel, b"identical bytes")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let other_space = server.create_space(&owner.user.id, "other").await;
    server.add_member(&other_space, &member.user.id).await;
    let other = server.create_channel(&other_space, "general").await;
    assert_eq!(
        upload(&server, &member, &other, b"identical bytes")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        upload(&server, &member, &channel, b"different bytes")
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn instance_hash_block_applies_across_spaces_and_requires_admin() {
    let (server, owner, member, _, channel) = setup().await;
    let body = parse_body(upload(&server, &member, &channel, b"spam").await).await;
    let attachment = body["data"]["attachments"][0]["id"].as_str().unwrap();
    assert_eq!(
        block(&server, &owner, "*", attachment).await.status(),
        StatusCode::FORBIDDEN
    );
    let other = server.create_space(&owner.user.id, "other").await;
    assert_eq!(
        block(&server, &owner, &other, attachment).await.status(),
        StatusCode::NOT_FOUND
    );
    let admin = server.create_admin_with_token("operator").await;
    assert_eq!(
        block(&server, &admin, "*", attachment).await.status(),
        StatusCode::OK
    );
    let other_channel = server.create_channel(&other, "general").await;
    server.add_member(&other, &member.user.id).await;
    assert_eq!(
        upload(&server, &member, &other_channel, b"spam")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        upload(&server, &owner, &channel, b"spam").await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn slowmode_is_atomic_shared_by_text_and_uploads_and_survives_deletion() {
    let (server, owner, member, space, channel) = setup().await;
    assert_eq!(
        slowmode(&server, &owner, &channel, 60).await.status(),
        StatusCode::OK
    );
    let (a, b) = tokio::join!(
        send(&server, &member, &channel, "one"),
        send(&server, &member, &channel, "two")
    );
    let (accepted, rejected) = if a.status() == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_limited(rejected).await;
    let body = parse_body(accepted).await;
    let id = body["data"]["id"].as_str().unwrap();
    sqlx::query(&db::q("DELETE FROM messages WHERE id=?"))
        .bind(id)
        .execute(server.pool())
        .await
        .unwrap();
    assert_limited(upload(&server, &member, &channel, b"denied").await).await;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM attachments")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let other = server.create_channel(&space, "other").await;
    assert_eq!(
        send(&server, &member, &other, "allowed").await.status(),
        StatusCode::OK
    );
    sqlx::query(&db::q(
        "UPDATE message_cooldowns SET last_sent_ms=? WHERE channel_id=? AND user_id=?",
    ))
    .bind(chrono::Utc::now().timestamp_millis() - 61000)
    .bind(&channel)
    .bind(&member.user.id)
    .execute(server.pool())
    .await
    .unwrap();
    assert_eq!(
        upload(&server, &member, &channel, b"allowed")
            .await
            .status(),
        StatusCode::OK
    );
    assert_limited(send(&server, &member, &channel, "too soon").await).await;
    assert_eq!(
        slowmode(&server, &owner, &channel, 0).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&server, &member, &channel, "disabled").await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn slowmode_exempts_moderators_and_failed_inserts_do_not_consume_cooldown() {
    let (server, owner, member, space, channel) = setup().await;
    assert_eq!(
        slowmode(&server, &owner, &channel, -1).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        slowmode(&server, &owner, &channel, 21601).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        slowmode(&server, &owner, &channel, 60).await.status(),
        StatusCode::OK
    );
    let failed = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel}/messages"),
            &member.auth_header(),
            &json!({"content":"invalid","reply_to":"missing"}),
        ))
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        send(&server, &member, &channel, "valid").await.status(),
        StatusCode::OK
    );
    for permission in ["manage_messages", "manage_channels"] {
        let moderator = server.create_user_with_token(permission).await;
        server.add_member(&space, &moderator.user.id).await;
        let role = server.create_role(&space, permission, &[permission]).await;
        server.assign_role(&space, &moderator.user.id, &role).await;
        for _ in 0..2 {
            assert_eq!(
                send(&server, &moderator, &channel, "moderation")
                    .await
                    .status(),
                StatusCode::OK
            );
        }
    }
    for _ in 0..2 {
        assert_eq!(
            send(&server, &owner, &channel, "owner").await.status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn upload_request_budget_is_per_user_across_channels_and_does_not_limit_text() {
    let (server, owner, member, space, channel) = setup().await;
    configure_uploads(&server, json!({"upload_requests_per_minute":2})).await;
    let settings = db::settings::get_settings(server.pool()).await.unwrap();
    assert_eq!(settings.upload_requests_per_minute, 2);
    let other = server.create_channel(&space, "other").await;
    assert_eq!(
        upload(&server, &member, &channel, b"one").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        upload(&server, &member, &other, b"two").await.status(),
        StatusCode::OK
    );
    assert_limited(upload(&server, &member, &other, b"three").await).await;
    assert_eq!(
        send(&server, &member, &channel, "text").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        upload(&server, &owner, &channel, b"separate budget")
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn upload_byte_budget_counts_actual_payload_without_content_length() {
    let (server, _, member, _, channel) = setup().await;
    configure_uploads(&server, json!({"upload_bytes_per_minute":6})).await;
    assert_eq!(
        upload(&server, &member, &channel, b"1234").await.status(),
        StatusCode::OK
    );
    assert_limited(upload(&server, &member, &channel, b"5678").await).await;
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        upload(&server, &member, &channel, b"56").await.status(),
        StatusCode::OK
    );
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A `data:` URI carrying `bytes`. The server validates the declared MIME type,
/// not the payload, so any bytes exercise the admission path.
fn data_uri(bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", base64_encode(bytes))
}
async fn block_hash(server: &TestServer, user: &TestUser, scope: &str, hash: &str) -> Response {
    server
        .router()
        .oneshot(authenticated_json_request(
            Method::PUT,
            &format!("/api/v1/automod/{scope}/hashes/{hash}"),
            &user.auth_header(),
            &json!({"reason":"blocked everywhere"}),
        ))
        .await
        .unwrap()
}
async fn set_avatar(server: &TestServer, user: &TestUser, image: &str) -> Response {
    server
        .router()
        .oneshot(authenticated_json_request(
            Method::PATCH,
            "/api/v1/users/@me",
            &user.auth_header(),
            &json!({"avatar":image}),
        ))
        .await
        .unwrap()
}

/// A blocked digest must stay blocked on every ingest route, not just message
/// attachments: otherwise the same bytes come straight back as an avatar,
/// emoji, space icon or sound.
#[tokio::test]
async fn hash_blocks_apply_to_avatars_emoji_icons_and_sounds() {
    let (server, owner, member, space, _) = setup().await;
    let admin = server.create_admin_with_token("operator").await;
    let bytes = b"blocked everywhere".to_vec();
    let digest = automod::hash(&bytes);
    assert_eq!(
        block_hash(&server, &admin, "*", &digest).await.status(),
        StatusCode::OK
    );
    let image = data_uri(&bytes);

    assert_eq!(
        set_avatar(&server, &member, &image).await.status(),
        StatusCode::BAD_REQUEST
    );
    let emoji = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/spaces/{space}/emojis"),
            &owner.auth_header(),
            &json!({"name":"blocked","image":image}),
        ))
        .await
        .unwrap();
    assert_eq!(emoji.status(), StatusCode::BAD_REQUEST);
    let icon = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PATCH,
            &format!("/api/v1/spaces/{space}"),
            &owner.auth_header(),
            &json!({"icon":image}),
        ))
        .await
        .unwrap();
    assert_eq!(icon.status(), StatusCode::BAD_REQUEST);
    let sound = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/spaces/{space}/soundboard"),
            &owner.auth_header(),
            &json!({
                "name":"blocked",
                "audio":format!("data:audio/ogg;base64,{}", base64_encode(&bytes))
            }),
        ))
        .await
        .unwrap();
    assert_eq!(sound.status(), StatusCode::BAD_REQUEST);

    // An unblocked image still goes through, so the block is about the digest
    // and not about the route being broken.
    assert_eq!(
        set_avatar(&server, &member, &data_uri(b"allowed"))
            .await
            .status(),
        StatusCode::OK
    );
}

/// The per-user upload budget is supposed to bound upload bandwidth, so base64
/// ingest routes have to be charged against the same bucket as attachments.
#[tokio::test]
async fn upload_budget_covers_base64_ingest_routes() {
    let (server, _, member, _, channel) = setup().await;
    configure_uploads(&server, json!({"upload_requests_per_minute":2})).await;
    assert_eq!(
        set_avatar(&server, &member, &data_uri(b"first"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        set_avatar(&server, &member, &data_uri(b"second"))
            .await
            .status(),
        StatusCode::OK
    );
    // Budget exhausted by the two avatars: the attachment route shares it.
    assert_limited(upload(&server, &member, &channel, b"third").await).await;
}
