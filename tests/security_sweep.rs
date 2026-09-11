mod common;

use common::{authenticated_json_request, authenticated_request, parse_body, TestServer};
use http::{Method, StatusCode};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn role_reordering_cannot_bypass_hierarchy_or_replace_everyone() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let manager = server.create_user_with_token("manager").await;
    let space = server.create_space(&owner.user.id, "roles").await;
    server.add_member(&space, &manager.user.id).await;
    let low = server.create_role(&space, "low", &[]).await;
    let own = server
        .create_role(&space, "manager", &["manage_roles"])
        .await;
    let high = server
        .create_role(&space, "admin", &["administrator"])
        .await;
    server.assign_role(&space, &manager.user.id, &own).await;
    let roles = accordserver::db::roles::list_roles(server.pool(), &space)
        .await
        .unwrap();
    let own_pos = roles.iter().find(|r| r.id == own).unwrap().position;
    let everyone = roles.iter().find(|r| r.position == 0).unwrap();
    for (body, expected) in [
        (
            json!([{ "id": high, "position": 1 }]),
            StatusCode::FORBIDDEN,
        ),
        (
            json!([{ "id": low, "position": own_pos }]),
            StatusCode::FORBIDDEN,
        ),
        (
            json!([{ "id": own, "position": 99 }]),
            StatusCode::FORBIDDEN,
        ),
        (
            json!([{ "id": everyone.id, "position": 1 }]),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!([{ "id": low, "position": -1 }]),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let req = authenticated_json_request(
            Method::PATCH,
            &format!("/api/v1/spaces/{space}/roles"),
            &manager.auth_header(),
            &body,
        );
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            expected
        );
    }
    let after = accordserver::db::roles::list_roles(server.pool(), &space)
        .await
        .unwrap();
    assert_eq!(
        roles
            .iter()
            .map(|r| (&r.id, r.position))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|r| (&r.id, r.position))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn channel_overwrites_cannot_grant_missing_permissions_or_administrator() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let manager = server.create_user_with_token("manager").await;
    let space = server.create_space(&owner.user.id, "overwrites").await;
    server.add_member(&space, &manager.user.id).await;
    let role = server
        .create_role(&space, "manager", &["manage_roles"])
        .await;
    server.assign_role(&space, &manager.user.id, &role).await;
    let channel = server.create_channel(&space, "general").await;
    for (perm, expected) in [
        ("administrator", StatusCode::BAD_REQUEST),
        ("manage_channels", StatusCode::FORBIDDEN),
    ] {
        let req = authenticated_json_request(
            Method::PUT,
            &format!("/api/v1/channels/{channel}/permissions/{}", manager.user.id),
            &manager.auth_header(),
            &json!({"type":"member", "allow":[perm], "deny":[]}),
        );
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            expected
        );
    }
    let perms = accordserver::middleware::permissions::resolve_channel_permissions(
        server.pool(),
        &channel,
        &space,
        &manager.user.id,
    )
    .await
    .unwrap();
    assert!(!perms
        .iter()
        .any(|p| p == "administrator" || p == "manage_channels"));
}

#[tokio::test]
async fn public_space_does_not_publish_private_channel_history() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let space = server.create_public_space(&owner.user.id, "public").await;
    let channel = server.create_channel(&space, "private").await;
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel}/messages"),
        &owner.auth_header(),
        &json!({"content":"private sentinel"}),
    );
    let body = parse_body(server.router().oneshot(req).await.unwrap()).await;
    let message = body["data"]["id"].as_str().unwrap();
    for uri in [
        format!("/api/v1/channels/{channel}/messages"),
        format!("/api/v1/channels/{channel}/messages/{message}"),
        format!("/api/v1/spaces/{space}/messages/search?query=sentinel"),
    ] {
        let req = http::Request::builder()
            .uri(&uri)
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED,
            "{uri}"
        );
    }
    // Explicit publication restores anonymous access.
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/channels/{channel}"),
        &owner.auth_header(),
        &json!({"allow_anonymous_read":true}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let req = http::Request::builder()
        .uri(format!("/api/v1/channels/{channel}/messages"))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(parse_body(response)
        .await
        .to_string()
        .contains("private sentinel"));
}

#[tokio::test]
async fn crawlers_and_oembed_cannot_read_unpublished_channels() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let space = server.create_public_space(&owner.user.id, "public").await;
    server.create_channel(&space, "private-sentinel").await;
    let space = accordserver::db::spaces::get_space_row(server.pool(), &space)
        .await
        .unwrap();
    for uri in [
        format!("/s/{}/private-sentinel", space.slug),
        format!(
            "/oembed?url=https://example.com/s/{}/private-sentinel",
            space.slug
        ),
    ] {
        let req = http::Request::builder()
            .uri(&uri)
            .header("User-Agent", "Googlebot")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let req = http::Request::builder()
        .uri("/sitemap.xml")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("private-sentinel"));
}

#[tokio::test]
async fn spoofed_headers_and_random_tokens_do_not_reset_rate_limits() {
    let server = TestServer::new().await;
    for i in 0..71 {
        let req = http::Request::builder()
            .uri("/api/v1/gateway")
            .header("Authorization", format!("Bearer fake-{i}"))
            .header("X-Forwarded-For", format!("192.0.2.{i}"))
            .header("X-Real-IP", format!("198.51.100.{i}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let expected = if i < 70 {
            StatusCode::OK
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            expected
        );
    }
    assert_eq!(server.state.rate_limits.len(), 1);
}

#[tokio::test]
async fn hidden_channels_cannot_be_used_when_send_permission_remains() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "hidden").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_channel(&space, "hidden").await;
    let req = authenticated_json_request(
        Method::PUT,
        &format!("/api/v1/channels/{channel}/permissions/{}", member.user.id),
        &owner.auth_header(),
        &json!({"type":"member", "allow":[], "deny":["view_channel"]}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/channels/{channel}/messages"),
        &member.auth_header(),
        &json!({"content":"must not send"}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space}/channels"),
        &member.auth_header(),
    );
    let body = parse_body(server.router().oneshot(req).await.unwrap()).await;
    assert!(!body.to_string().contains(&channel));
}

#[tokio::test]
async fn uploaded_html_is_served_as_a_sandboxed_download() {
    let server = TestServer::new().await;
    let user = server.create_user_with_token("uploader").await;
    let space = server.create_space(&user.user.id, "space").await;
    let channel = server.create_channel(&space, "general").await;
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel}/messages"),
            &user.auth_header(),
            &json!({"content":"uploaded html"}),
        ))
        .await
        .unwrap();
    let message = parse_body(response).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(&accordserver::db::q(
        "INSERT INTO attachments (id, message_id, filename, size, url) VALUES (?, ?, ?, ?, ?)",
    ))
    .bind("test")
    .bind(&message)
    .bind("payload.html")
    .bind(25_i64)
    .bind("/cdn/attachments/test/payload.html")
    .execute(server.pool())
    .await
    .unwrap();
    let dir = server.state.storage_path.join("attachments/test");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("payload.html"), "<script>alert(1)</script>")
        .await
        .unwrap();
    let req = http::Request::builder()
        .uri("/cdn/attachments/test/payload.html")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-disposition"], "attachment");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("sandbox"));
}

#[tokio::test]
async fn overwrites_cannot_be_cleared_to_restore_a_denied_privilege() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let manager = server.create_user_with_token("manager").await;
    let space = server.create_space(&owner.user.id, "overwrites").await;
    server.add_member(&space, &manager.user.id).await;
    let role = server
        .create_role(&space, "manager", &["manage_roles", "manage_channels"])
        .await;
    server.assign_role(&space, &manager.user.id, &role).await;
    let channel = server.create_channel(&space, "general").await;
    let uri = format!("/api/v1/channels/{channel}/permissions/{}", manager.user.id);
    let req = authenticated_json_request(
        Method::PUT,
        &uri,
        &owner.auth_header(),
        &json!({"type":"member", "allow":[], "deny":["manage_channels"]}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let req = authenticated_json_request(
        Method::PUT,
        &uri,
        &manager.auth_header(),
        &json!({"type":"member", "allow":[], "deny":[]}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let req = authenticated_request(Method::DELETE, &uri, &manager.auth_header());
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn guest_tokens_are_scoped_and_stop_working_when_guest_access_is_disabled() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let space = server.create_space(&owner.user.id, "guest").await;
    let other = server.create_space(&owner.user.id, "other").await;
    for id in [&space, &other] {
        let req = authenticated_json_request(
            Method::PATCH,
            &format!("/api/v1/spaces/{id}"),
            &owner.auth_header(),
            &json!({"allow_guest_access":true}),
        );
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            StatusCode::OK
        );
    }
    let token = accordserver::middleware::auth::generate_token();
    sqlx::query(&accordserver::db::q(
        "INSERT INTO guest_tokens (token_hash, space_id, expires_at) VALUES (?, ?, ?)",
    ))
    .bind(accordserver::middleware::auth::create_token_hash(&token))
    .bind(&space)
    .bind(
        (chrono::Utc::now() + chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string(),
    )
    .execute(server.pool())
    .await
    .unwrap();
    let auth = format!("Bearer {token}");
    for uri in [
        format!("/api/v1/spaces/{other}"),
        format!("/api/v1/spaces/{other}/channels"),
    ] {
        let req = authenticated_request(Method::GET, &uri, &auth);
        assert_eq!(
            server.router().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space}/members"),
        &auth,
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let req = authenticated_json_request(
        Method::PATCH,
        &format!("/api/v1/spaces/{space}"),
        &owner.auth_header(),
        &json!({"allow_guest_access":false}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/spaces/{space}/members"),
        &auth,
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn attachments_are_served_with_media_metadata_and_range_support() {
    let server = TestServer::new().await;
    let user = server.create_user_with_token("uploader").await;
    let space = server.create_space(&user.user.id, "space").await;
    let channel = server.create_channel(&space, "general").await;
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            &format!("/api/v1/channels/{channel}/messages"),
            &user.auth_header(),
            &json!({"content":"uploaded clip"}),
        ))
        .await
        .unwrap();
    let message = parse_body(response).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(&accordserver::db::q(
        "INSERT INTO attachments (id, message_id, filename, size, url) VALUES (?, ?, ?, ?, ?)",
    ))
    .bind("clip")
    .bind(&message)
    .bind("clip.mp3")
    .bind(8_i64)
    .bind("/cdn/attachments/clip/clip.mp3")
    .execute(server.pool())
    .await
    .unwrap();
    let dir = server.state.storage_path.join("attachments/clip");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("clip.mp3"), b"0123456789")
        .await
        .unwrap();

    let req = http::Request::builder()
        .uri("/cdn/attachments/clip/clip.mp3")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The real media type, not a blanket application/octet-stream.
    assert_eq!(response.headers()["content-type"], "audio/mpeg");
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    // Revalidate cached media so moderation withdrawal cannot be bypassed.
    assert!(response.headers()["cache-control"]
        .to_str()
        .unwrap()
        .contains("no-cache"));
    // Inert content guarantees still hold.
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["content-disposition"], "attachment");

    // Seeking works, which inline audio/video playback depends on.
    let req = http::Request::builder()
        .uri("/cdn/attachments/clip/clip.mp3")
        .header("Range", "bytes=2-5")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 2-5/10");
    let body = axum::body::to_bytes(response.into_body(), 64)
        .await
        .unwrap();
    assert_eq!(&body[..], b"2345");
}
