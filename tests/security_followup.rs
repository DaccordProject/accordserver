mod common;

use accordserver::{
    db, models::invite::CreateInvite, models::plugin::PluginManifest, security, storage,
};
use common::{authenticated_json_request, authenticated_request, parse_body, TestServer};
use http::{Method, StatusCode};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn admin_provisioning_is_local_and_does_not_promote_existing_users() {
    let server = TestServer::new().await;
    let ordinary = server.create_user_with_token("ordinary").await;
    assert!(
        security::bootstrap_admin(server.pool(), "ordinary", "local-admin-password-123")
            .await
            .is_err()
    );
    assert!(
        !db::users::get_user(server.pool(), &ordinary.user.id)
            .await
            .unwrap()
            .is_admin
    );
    security::bootstrap_admin(server.pool(), "operator", "local-admin-password-123")
        .await
        .unwrap();
    let response = server
        .router()
        .oneshot(common::json_request(
            Method::POST,
            "/api/v1/auth/login",
            &json!({"username":"operator", "password":"local-admin-password-123"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(parse_body(response).await["data"]["user"]["is_admin"], true);
}

#[tokio::test]
async fn finite_invites_are_atomic_idempotent_and_ban_aware() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let banned = server.create_user_with_token("banned").await;
    let a = server.create_user_with_token("a").await;
    let b = server.create_user_with_token("b").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server
        .ban_user(&space, &banned.user.id, &owner.user.id)
        .await;
    let invite = db::invites::create_invite(
        server.pool(),
        &space,
        None,
        &owner.user.id,
        &CreateInvite {
            max_uses: Some(1),
            max_age: None,
            temporary: None,
        },
    )
    .await
    .unwrap();
    assert!(
        !db::invites::accept_invite(server.pool(), &invite.code, &owner.user.id)
            .await
            .unwrap()
            .1
    );
    assert!(
        db::invites::accept_invite(server.pool(), &invite.code, &banned.user.id)
            .await
            .is_err()
    );
    assert_eq!(
        db::invites::get_invite(server.pool(), &invite.code)
            .await
            .unwrap()
            .uses,
        0
    );
    let (a, b) = tokio::join!(
        db::invites::accept_invite(server.pool(), &invite.code, &a.user.id),
        db::invites::accept_invite(server.pool(), &invite.code, &b.user.id)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        db::invites::get_invite(server.pool(), &invite.code)
            .await
            .unwrap()
            .uses,
        1
    );
}

#[tokio::test]
async fn invite_page_escapes_stored_icons_for_browsers_and_crawlers() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let space = server.create_space(&owner.user.id, "space").await;
    let payload = "x\"><script>window.XSS_PROOF=1</script>&'";
    sqlx::query(&db::q("UPDATE spaces SET icon = ? WHERE id = ?"))
        .bind(payload)
        .bind(&space)
        .execute(server.pool())
        .await
        .unwrap();
    let invite = db::invites::create_invite(
        server.pool(),
        &space,
        None,
        &owner.user.id,
        &CreateInvite {
            max_uses: None,
            max_age: None,
            temporary: None,
        },
    )
    .await
    .unwrap();
    for agent in ["Mozilla/5.0", "Googlebot"] {
        let req = http::Request::builder()
            .uri(format!("/invite/{}", invite.code))
            .header("User-Agent", agent)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = server.router().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = axum::body::to_bytes(response.into_body(), 100000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&html);
        assert!(!html.contains("<script>window.XSS_PROOF"));
        assert!(html.contains("&lt;script&gt;window.XSS_PROOF"));
    }
}

#[tokio::test]
async fn plugin_sessions_reject_foreign_channels_and_nonmember_participants() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let outsider = server.create_user_with_token("outsider").await;
    let space = server.create_space(&owner.user.id, "space").await;
    let other = server.create_space(&outsider.user.id, "other").await;
    let local = server.create_channel(&space, "local").await;
    let foreign = server.create_channel(&other, "foreign").await;
    let plugin = db::plugins::create_plugin(
        server.pool(),
        &space,
        &owner.user.id,
        &PluginManifest {
            name: "demo".into(),
            runtime: "scripted".into(),
            plugin_type: "activity".into(),
            ..Default::default()
        },
        None,
        None,
    )
    .await
    .unwrap();
    let req = authenticated_json_request(
        Method::POST,
        &format!("/api/v1/plugins/{}/sessions", plugin.id),
        &owner.auth_header(),
        &json!({"channel_id":foreign}),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let session =
        db::plugins::create_session(server.pool(), &plugin.id, &local, &owner.user.id, false)
            .await
            .unwrap();
    assert!(db::plugins::add_participant(
        server.pool(),
        &session.id,
        &outsider.user.id,
        "player",
        Some(1)
    )
    .await
    .is_err());
    assert_eq!(
        db::plugins::get_session_user_ids(server.pool(), &session.id)
            .await
            .unwrap(),
        vec![owner.user.id]
    );
}

#[tokio::test]
async fn tracker_capacity_is_hard_bounded_and_expired_entries_are_reclaimed() {
    let server = TestServer::new().await;
    let map = dashmap::DashMap::new();
    for i in 0..security::MAX_TRACKERS {
        map.insert(i.to_string(), false);
    }
    assert!(security::reserve_tracker(&server.state, &map, "new", |v| *v, || false).is_err());
    assert_eq!(map.len(), security::MAX_TRACKERS);
    map.insert("0".into(), true);
    security::reserve_tracker(&server.state, &map, "new", |v| *v, || false).unwrap();
    assert_eq!(map.len(), security::MAX_TRACKERS);
    assert!(!map.contains_key("0"));
}

#[tokio::test]
async fn channel_space_and_account_deletion_remove_attachment_files() {
    for deletion in ["channel", "space", "user", "message"] {
        let server = TestServer::new().await;
        let owner = server.create_user_with_token("owner").await;
        let author = server.create_user_with_token("author").await;
        let space = server.create_space(&owner.user.id, "space").await;
        server.add_member(&space, &author.user.id).await;
        let channel = server.create_channel(&space, "general").await;
        let response = server
            .router()
            .oneshot(authenticated_json_request(
                Method::POST,
                &format!("/api/v1/channels/{channel}/messages"),
                &author.auth_header(),
                &json!({"content":"attachment"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let message = parse_body(response).await["data"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (url, _) = storage::save_attachment(
            &server.state.storage_path,
            &channel,
            "upload",
            "private.txt",
            b"secret",
            100,
        )
        .await
        .unwrap();
        sqlx::query(&db::q("INSERT INTO attachments (id, message_id, filename, content_type, size, url) VALUES (?, ?, ?, ?, ?, ?)"))
            .bind("upload").bind(&message).bind("private.txt").bind("text/plain").bind(6_i64).bind(&url).execute(server.pool()).await.unwrap();
        let get = || {
            http::Request::builder()
                .uri(&url)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert_eq!(
            server.router().oneshot(get()).await.unwrap().status(),
            StatusCode::OK
        );
        match deletion {
            "channel" => db::channels::delete_channel(server.pool(), &channel)
                .await
                .unwrap(),
            "space" => db::spaces::delete_space(server.pool(), &space)
                .await
                .unwrap(),
            "user" => db::admin::delete_user(server.pool(), &author.user.id)
                .await
                .unwrap(),
            _ => {
                sqlx::query(&db::q("DELETE FROM messages WHERE id = ?"))
                    .bind(&message)
                    .execute(server.pool())
                    .await
                    .unwrap();
            }
        }
        for encoded in [
            url.replacen("attachments", "%61ttachments", 1),
            url.replacen("/cdn/", "/cdn//", 1),
        ] {
            let request = http::Request::builder()
                .uri(encoded)
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(
                server.router().oneshot(request).await.unwrap().status(),
                StatusCode::NOT_FOUND
            );
        }
        // Metadata gate takes effect even before cleanup has run.
        assert_eq!(
            server.router().oneshot(get()).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{deletion}"
        );
        storage::drain_attachment_deletions(&server.state)
            .await
            .unwrap();
        assert!(
            !server
                .state
                .storage_path
                .join(url.strip_prefix("/cdn/").unwrap())
                .exists(),
            "{deletion}"
        );
    }
}

#[tokio::test]
async fn orphan_cleanup_preserves_new_uploads_and_live_files() {
    let server = TestServer::new().await;
    let (url, _) = storage::save_attachment(
        &server.state.storage_path,
        "channel",
        "upload",
        "orphan.txt",
        b"secret",
        100,
    )
    .await
    .unwrap();
    let path = server
        .state
        .storage_path
        .join(url.strip_prefix("/cdn/").unwrap());
    storage::reconcile_attachment_orphans(&server.state)
        .await
        .unwrap();
    assert!(path.exists());
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old))
        .unwrap();
    storage::reconcile_attachment_orphans(&server.state)
        .await
        .unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn kicking_member_clears_voice_state() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_channel(&space, "voice").await;
    accordserver::voice::state::join_voice_channel(
        &server.state,
        &member.user.id,
        Some(&space),
        &channel,
        "session",
        false,
        false,
        false,
        false,
    );
    let req = authenticated_request(
        Method::DELETE,
        &format!("/api/v1/spaces/{space}/members/{}", member.user.id),
        &owner.auth_header(),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    assert!(!server.state.voice_states.contains_key(&member.user.id));
}

#[tokio::test]
async fn livekit_evictions_are_sent_and_failed_requests_are_durably_retried() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mock = axum::Router::new().route(
        "/twirp/livekit.RoomService/RemoveParticipant",
        axum::routing::post(move |body: axum::body::Bytes| {
            let observed = observed.clone();
            async move {
                assert!(body.windows(8).any(|w| w == b"channel_"));
                if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "{\"code\":\"unavailable\",\"msg\":\"retry\"}",
                    )
                } else {
                    (StatusCode::OK, "")
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let mut server = TestServer::new().await;
    server.state.test_mode = false;
    server.state.livekit_client = Some(accordserver::voice::livekit::LiveKitClient::new(
        &url,
        &url,
        "test",
        "mock-secret",
    ));
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "space").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_channel(&space, "voice").await;
    accordserver::voice::state::join_voice_channel(
        &server.state,
        &member.user.id,
        Some(&space),
        &channel,
        "session",
        false,
        false,
        false,
        false,
    );
    sqlx::query(&db::q(
        "DELETE FROM members WHERE space_id = ? AND user_id = ?",
    ))
    .bind(&space)
    .bind(&member.user.id)
    .execute(server.pool())
    .await
    .unwrap();
    security::reconcile_voice_access(&server.state).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!server.state.voice_states.contains_key(&member.user.id));
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM voice_evictions")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(queued, 1);
    security::reconcile_voice_access(&server.state).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM voice_evictions")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
    task.abort();
}
