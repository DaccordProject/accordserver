mod common;
use accordserver::{
    automod::{
        self,
        policy::{Action, Policy, Rule, Scope, Trigger},
        scanner::{ScanResult, Scanner},
    },
    db,
};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::{
    authenticated_json_request, authenticated_request, build_multipart_upload_body, parse_body,
    TestServer, TestUser,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tower::ServiceExt;

struct Stub {
    calls: Arc<AtomicUsize>,
    score: f32,
    fail: bool,
}
impl Scanner for Stub {
    fn version(&self) -> &str {
        "test-v1"
    }
    fn scan(&self, _: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err("scanner unavailable".into());
            }
            Ok(ScanResult {
                sampled_timestamps_ms: vec![],
                model_version: "test-v1".into(),
                scores: BTreeMap::from([("explicit".into(), self.score)]),
            })
        })
    }
}
fn policy(action: Action) -> Policy {
    Policy {
        enabled: true,
        exempt_permissions: vec![],
        rules: vec![Rule {
            id: "explicit".into(),
            trigger: Trigger::Media {
                categories: vec!["explicit".into()],
                threshold: 0.8,
            },
            scope: Scope::NonNsfw,
            action,
        }],
        ..Policy::default()
    }
}
async fn setup(
    score: f32,
    fail: bool,
) -> (
    TestServer,
    TestUser,
    TestUser,
    String,
    String,
    Arc<AtomicUsize>,
) {
    let mut server = TestServer::new().await;
    let calls = Arc::new(AtomicUsize::new(0));
    server.state.automod = Arc::new(automod::AutoMod::new(Arc::new(Stub {
        calls: calls.clone(),
        score,
        fail,
    })));
    let owner = server.create_user_with_token("owner").await;
    let member = server.create_user_with_token("member").await;
    let space = server.create_space(&owner.user.id, "automod").await;
    server.add_member(&space, &member.user.id).await;
    let channel = server.create_channel(&space, "general").await;
    set_policy(&server, &owner, &space, &policy(Action::Quarantine)).await;
    (server, owner, member, space, channel, calls)
}
async fn set_policy(server: &TestServer, owner: &TestUser, scope: &str, policy: &Policy) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PUT,
            &format!("/api/v1/automod/{scope}/policy"),
            &owner.auth_header(),
            &serde_json::to_value(policy).unwrap(),
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
async fn upload(
    server: &TestServer,
    user: &TestUser,
    channel: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let body = build_multipart_upload_body(
        "testboundary",
        &json!({"content":"hello"}),
        "image.png",
        "image/png",
        bytes,
    );
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/channels/{channel}/messages/upload"))
        .header("Authorization", user.auth_header())
        .header("Content-Type", "multipart/form-data; boundary=testboundary")
        .body(Body::from(body))
        .unwrap();
    let response = server.router().oneshot(req).await.unwrap();
    let status = response.status();
    (status, parse_body(response).await)
}
fn id(body: &Value) -> &str {
    body["pending_attachments"][0]
        .as_str()
        .expect("pending attachment ID")
}
async fn review(server: &TestServer, user: &TestUser, id: &str, action: &str) -> StatusCode {
    server
        .router()
        .oneshot(authenticated_json_request(
            Method::PATCH,
            &format!("/api/v1/automod/uploads/{id}"),
            &user.auth_header(),
            &json!({"action":action,"reason":"reviewed"}),
        ))
        .await
        .unwrap()
        .status()
}
async fn cdn(server: &TestServer, url: &str) -> StatusCode {
    server
        .router()
        .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn pending_quarantine_review_and_withdrawal_never_expose_held_bytes() {
    let (server, owner, member, space, channel, _) = setup(0.95, false).await;
    let mut events = server
        .state
        .gateway_tx
        .read()
        .await
        .as_ref()
        .unwrap()
        .subscribe();
    let (status, body) = upload(&server, &member, &channel, b"explicit image").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["data"]["attachments"], json!([]));
    let id = id(&body);
    let msg = body["data"]["id"].as_str().unwrap();
    let url = format!("/cdn/attachments/{channel}/{id}/image.png");
    assert_eq!(cdn(&server, &url).await, StatusCode::NOT_FOUND);
    assert!(
        db::attachments::get_attachments_for_message(server.pool(), msg)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(automod::process_one(&server.state).await.unwrap());
    assert_eq!(
        automod::get(&server.state, id).await.unwrap().status,
        "quarantined"
    );
    assert_eq!(cdn(&server, &url).await, StatusCode::NOT_FOUND);
    assert_eq!(
        review(&server, &member, id, "release").await,
        StatusCode::FORBIDDEN
    );
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/automod/uploads/{id}/content"),
        &member.auth_header(),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let req = authenticated_request(
        Method::GET,
        &format!("/api/v1/automod/uploads/{id}/content"),
        &owner.auth_header(),
    );
    let response = server.router().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(review(&server, &owner, id, "release").await, StatusCode::OK);
    assert_eq!(cdn(&server, &url).await, StatusCode::OK);
    assert_eq!(
        db::attachments::get_attachments_for_message(server.pool(), msg)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        review(&server, &owner, id, "quarantine").await,
        StatusCode::OK
    );
    assert_eq!(cdn(&server, &url).await, StatusCode::NOT_FOUND);
    assert!(
        db::attachments::get_attachments_for_message(server.pool(), msg)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(review(&server, &owner, id, "release").await, StatusCode::OK);
    accordserver::storage::drain_attachment_deletions(&server.state)
        .await
        .unwrap();
    assert_eq!(cdn(&server, &url).await, StatusCode::OK);
    let entries = db::audit_log::list_entries(server.pool(), &space, None, None, None, 100)
        .await
        .unwrap();
    assert_eq!(entries.len(), 4);
    let mut updates = vec![];
    while let Ok(event) = events.try_recv() {
        if event.event["type"] == "message.update" {
            updates.push(event.event);
        }
    }
    assert_eq!(updates.len(), 3);
    assert_eq!(updates[1]["data"]["attachments"], json!([]));
}
#[tokio::test]
async fn safe_upload_publishes_and_cache_reapplies_changed_threshold() {
    let (server, owner, member, space, channel, calls) = setup(0.5, false).await;
    let (_, body) = upload(&server, &member, &channel, b"same bytes").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "published"
    );
    let mut stricter = policy(Action::Quarantine);
    if let Trigger::Media { threshold, .. } = &mut stricter.rules[0].trigger {
        *threshold = 0.4;
    }
    set_policy(&server, &owner, &space, &stricter).await;
    let (_, second) = upload(&server, &member, &channel, b"same bytes").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&second))
            .await
            .unwrap()
            .status,
        "quarantined"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn scanner_outage_is_persisted_and_recovered_by_a_new_worker() {
    let (mut server, _, member, _, channel, _) = setup(0.5, true).await;
    let (_, body) = upload(&server, &member, &channel, b"bytes").await;
    let id = id(&body);
    automod::process_one(&server.state).await.unwrap();
    let held = automod::get(&server.state, id).await.unwrap();
    assert_eq!(held.status, "pending");
    assert_eq!(held.reason, "scanner unavailable");
    assert_eq!(held.attempts, 1);
    assert!(!automod::process_one(&server.state).await.unwrap());
    server.state.automod = Arc::new(automod::AutoMod::new(Arc::new(Stub {
        calls: Arc::new(AtomicUsize::new(0)),
        score: 0.1,
        fail: false,
    })));
    sqlx::query(&db::q(
        "UPDATE automod_uploads SET next_attempt=0 WHERE id=?",
    ))
    .bind(id)
    .execute(server.pool())
    .await
    .unwrap();
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id).await.unwrap().status,
        "published"
    );
}
#[tokio::test]
async fn disabled_and_role_exempt_uploads_preserve_immediate_delivery() {
    let (server, owner, member, space, channel, calls) = setup(0.99, false).await;
    set_policy(&server, &owner, &space, &Policy::default()).await;
    let (status, body) = upload(&server, &member, &channel, b"one").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["attachments"].as_array().unwrap().len(), 1);
    let mut p = policy(Action::Quarantine);
    p.exempt_permissions = vec!["manage_messages".into()];
    set_policy(&server, &owner, &space, &p).await;
    let (status, _) = upload(&server, &owner, &channel, b"two").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn hash_rule_rejects_before_message_creation_and_without_scanning() {
    let (server, owner, member, space, channel, calls) = setup(0.0, false).await;
    let mut p = policy(Action::Reject);
    p.rules[0].trigger = Trigger::HashDenylist;
    p.rules[0].scope = Scope::All;
    set_policy(&server, &owner, &space, &p).await;
    let digest = automod::hash(b"blocked");
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PUT,
            &format!("/api/v1/automod/{space}/hashes/{digest}"),
            &owner.auth_header(),
            &json!({"reason":"blocked file"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (status, _) = upload(&server, &member, &channel, b"blocked").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn flag_publishes_and_creates_exactly_one_report() {
    let (server, owner, member, space, channel, _) = setup(0.99, false).await;
    set_policy(&server, &owner, &space, &policy(Action::Flag)).await;
    let (_, body) = upload(&server, &member, &channel, b"flag").await;
    automod::process_one(&server.state).await.unwrap();
    assert!(!automod::process_one(&server.state).await.unwrap());
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "published"
    );
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM reports")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
}
#[tokio::test]
async fn retention_and_deleted_messages_cannot_release_or_lose_audit() {
    let (server, owner, member, _, channel, _) = setup(0.99, false).await;
    let (_, body) = upload(&server, &member, &channel, b"evidence").await;
    let id = id(&body);
    automod::process_one(&server.state).await.unwrap();
    sqlx::query(&db::q("DELETE FROM messages WHERE id=?"))
        .bind(body["data"]["id"].as_str().unwrap())
        .execute(server.pool())
        .await
        .unwrap();
    assert_eq!(review(&server, &owner, id, "release").await, StatusCode::OK);
    assert_eq!(
        automod::get(&server.state, id).await.unwrap().status,
        "quarantined"
    );
    assert!(automod::private_path(&server.state, id).is_file());
    sqlx::query(&db::q("UPDATE automod_uploads SET expires_at=0 WHERE id=?"))
        .bind(id)
        .execute(server.pool())
        .await
        .unwrap();
    automod::cleanup(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id).await.unwrap().status,
        "removed"
    );
    assert!(!automod::private_path(&server.state, id).exists());
    assert_eq!(
        review(&server, &owner, id, "release").await,
        StatusCode::CONFLICT
    );
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM automod_events")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert!(n >= 3);
}
#[tokio::test]
async fn capacity_rejection_is_atomic_and_does_not_create_extra_message() {
    let (mut server, _, member, _, channel, _) = setup(0.99, false).await;
    Arc::get_mut(&mut server.state.automod).unwrap().max_held = 1;
    assert_eq!(
        upload(&server, &member, &channel, b"one").await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        upload(&server, &member, &channel, b"two").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
}
#[tokio::test]
async fn space_override_inherits_server_default_and_requires_authorization() {
    let (server, owner, member, space, _, _) = setup(0.0, false).await;
    let admin = server.create_user_with_token("admin").await;
    sqlx::query(&db::q("UPDATE users SET is_admin=TRUE WHERE id=?"))
        .bind(&admin.user.id)
        .execute(server.pool())
        .await
        .unwrap();
    set_policy(&server, &admin, "*", &policy(Action::Quarantine)).await;
    let response = server
        .router()
        .oneshot(authenticated_request(
            Method::DELETE,
            &format!("/api/v1/automod/{space}/policy"),
            &owner.auth_header(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        automod::policy::load(&server.state, Some(&space))
            .await
            .unwrap()
            .enabled
    );
    let response = server
        .router()
        .oneshot(authenticated_request(
            Method::GET,
            &format!("/api/v1/automod/{space}/uploads"),
            &member.auth_header(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    for path in [
        "/api/v1/automod/health".to_string(),
        format!("/api/v1/automod/{space}/events"),
        format!("/api/v1/automod/{space}/hashes"),
        format!("/api/v1/automod/{space}/uploads"),
    ] {
        let response = server
            .router()
            .oneshot(authenticated_request(
                Method::GET,
                &path,
                &admin.auth_header(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path}: {}",
            parse_body(response).await
        );
    }
}
#[tokio::test]
async fn nsfw_channel_skips_non_nsfw_rule_and_timeout_requires_separate_rule() {
    let (server, owner, member, space, channel, calls) = setup(0.99, false).await;
    sqlx::query(&db::q("UPDATE channels SET nsfw=TRUE WHERE id=?"))
        .bind(&channel)
        .execute(server.pool())
        .await
        .unwrap();
    let (_, body) = upload(&server, &member, &channel, b"allowed").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "published"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(policy(Action::Timeout { seconds: 60 }).validate().is_err());
    let mut p = policy(Action::Timeout { seconds: 60 });
    p.rules[0].trigger = Trigger::LowTrust {
        min_account_age_hours: 24,
        min_space_age_hours: 24,
        require_role: true,
    };
    p.rules[0].scope = Scope::All;
    set_policy(&server, &owner, &space, &p).await;
    let (_, body) = upload(&server, &member, &channel, b"new member").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "quarantined"
    );
    assert!(
        db::members::get_member_row(server.pool(), &space, &member.user.id)
            .await
            .unwrap()
            .timed_out_until
            .is_some()
    );
}
#[test]
fn decode_rejects_disguised_animated_malformed_and_oversized_media() {
    use automod::scanner::decode;
    assert!(decode(b"GIF89a").is_err());
    assert!(decode(b"not an image").is_err());
    let mut buffer = std::io::Cursor::new(vec![]);
    image::RgbImage::new(8, 8)
        .write_to(&mut buffer, image::ImageFormat::Png)
        .unwrap();
    assert!(decode(buffer.get_ref()).is_ok());
    let mut apng = buffer.into_inner();
    apng.splice(8..8, b"\0\0\0\0acTL\0\0\0\0".iter().copied());
    assert!(decode(&apng).is_err());
    let mut buffer = std::io::Cursor::new(vec![]);
    image::RgbImage::new(5000, 1)
        .write_to(&mut buffer, image::ImageFormat::Png)
        .unwrap();
    assert!(decode(buffer.get_ref()).is_err());
}

/// Run explicitly with the official weights and CPU runtime installed. No model
/// download is performed by ordinary CI tests.
#[tokio::test]
#[ignore = "requires ACCORD_AUTOMOD_MODEL_PATH and ACCORD_AUTOMOD_RUNTIME_PATH"]
async fn real_local_model_cpu_smoke() {
    let model = std::env::var("ACCORD_AUTOMOD_MODEL_PATH").unwrap();
    let runtime = std::env::var("ACCORD_AUTOMOD_RUNTIME_PATH").unwrap();
    let start = std::time::Instant::now();
    let scanner = automod::scanner::Local::load(
        std::path::Path::new(&model),
        std::path::Path::new(&runtime),
        2,
    )
    .unwrap();
    eprintln!("model load: {:?}", start.elapsed());
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(640, 480, image::Rgb([30, 100, 200]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let start = std::time::Instant::now();
    let result = scanner.scan(bytes.into_inner()).await.unwrap();
    eprintln!(
        "CPU scan: {:?}; scores: {:?}",
        start.elapsed(),
        result.scores
    );
    result.validate().unwrap();
    assert_eq!(result.scores.len(), 18);
}

#[tokio::test]
async fn policy_disabled_during_pending_and_missing_model_both_hold_uploads() {
    let (mut server, owner, member, space, channel, _) = setup(0.1, false).await;
    let (_, body) = upload(&server, &member, &channel, b"pending").await;
    set_policy(&server, &owner, &space, &Policy::default()).await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "quarantined"
    );
    set_policy(&server, &owner, &space, &policy(Action::Quarantine)).await;
    server.state.automod = Arc::new(automod::AutoMod::default());
    let (_, body) = upload(&server, &member, &channel, b"no model").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "pending"
    );
}
#[tokio::test]
async fn revoked_membership_blocks_queued_publication() {
    let (server, _, member, space, channel, _) = setup(0.1, false).await;
    let (_, body) = upload(&server, &member, &channel, b"pending").await;
    sqlx::query(&db::q("DELETE FROM members WHERE space_id=? AND user_id=?"))
        .bind(space)
        .bind(&member.user.id)
        .execute(server.pool())
        .await
        .unwrap();
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "quarantined"
    );
}
struct Slow;
impl Scanner for Slow {
    fn version(&self) -> &str {
        "slow"
    }
    fn scan(&self, _: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>> {
        Box::pin(async { std::future::pending().await })
    }
}
#[tokio::test]
async fn timeout_does_not_publish_and_model_rejection_is_reported_asynchronously() {
    let (mut server, owner, member, space, channel, _) = setup(0.95, false).await;
    set_policy(&server, &owner, &space, &policy(Action::Reject)).await;
    let (_, body) = upload(&server, &member, &channel, b"rejected").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "rejected"
    );
    let mut runtime = automod::AutoMod::new(Arc::new(Slow));
    runtime.scan_timeout = std::time::Duration::from_millis(10);
    server.state.automod = Arc::new(runtime);
    let (_, body) = upload(&server, &member, &channel, b"slow").await;
    automod::process_one(&server.state).await.unwrap();
    let held = automod::get(&server.state, id(&body)).await.unwrap();
    assert_eq!(held.status, "pending");
    assert_eq!(held.reason, "scanner timed out");
}
#[tokio::test]
async fn moderator_gateway_events_are_not_disclosed_to_ordinary_members() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let (server, owner, member, space, channel, _) = setup(0.95, false).await;
    let base = server.spawn().await.replace("http://", "ws://");
    let mut sockets = vec![];
    for user in [&owner, &member] {
        let (mut socket, _) = connect_async(format!("{base}/ws")).await.unwrap();
        socket.next().await.unwrap().unwrap();
        socket.send(Message::Text(json!({"op":2,"data":{"token":user.gateway_token(),"intents":["messages","moderation","message_typing"]}}).to_string().into())).await.unwrap();
        // READY is preceded by no other application event for these sessions.
        loop {
            let event = socket.next().await.unwrap().unwrap();
            if let Message::Text(text) = event {
                if serde_json::from_str::<Value>(&text).unwrap()["type"] == "ready" {
                    break;
                }
            }
        }
        sockets.push(socket);
    }
    upload(&server, &member, &channel, b"explicit").await;
    automod::process_one(&server.state).await.unwrap();
    server.state.gateway_tx.read().await.as_ref().unwrap().send(accordserver::gateway::events::GatewayBroadcast {space_id:Some(space),target_user_ids:None,intent:"message_typing".into(),event:json!({"op":0,"type":"typing.start","data":{"channel_id":channel,"user_id":owner.user.id}}),required_permission:None}).unwrap();
    for (index, mut socket) in sockets.into_iter().enumerate() {
        let saw = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut saw = false;
            while let Some(event) = socket.next().await {
                if let Message::Text(text) = event.unwrap() {
                    let event: Value = serde_json::from_str(&text).unwrap();
                    if event["type"] == "automod.upload_update" {
                        saw = true;
                    }
                    if event["type"] == "typing.start" {
                        break;
                    }
                }
            }
            saw
        })
        .await
        .unwrap();
        assert_eq!(saw, index == 0);
        socket.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn dm_uploads_use_instance_policy_and_only_instance_admin_can_review() {
    let (server, owner, member, _, _, _) = setup(0.95, false).await;
    let admin = server.create_admin_with_token("operator").await;
    set_policy(&server, &admin, "*", &policy(Action::Flag)).await;
    let dm = server.create_dm(&owner.user.id, &member.user.id).await;
    let (_, body) = upload(&server, &member, &dm, b"dm content").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "published"
    );
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM reports WHERE space_id IS NULL")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        review(&server, &owner, id(&body), "quarantine").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        review(&server, &admin, id(&body), "quarantine").await,
        StatusCode::OK
    );
}

struct FiveFrames {
    count: usize,
    fail: bool,
}
impl automod::video::VideoSampler for FiveFrames {
    fn sample(
        &self,
        _: &std::path::Path,
        _: automod::video::Container,
    ) -> BoxFuture<'_, Result<automod::video::Samples, String>> {
        Box::pin(async move {
            if self.fail {
                return Err("video decoding failed".into());
            }
            Ok(automod::video::Samples {
                frames: (0..self.count).map(|i| vec![i as u8]).collect(),
                timestamps_ms: (0..self.count).map(|i| 1000 + i as u64 * 2000).collect(),
            })
        })
    }
}
struct FrameScores {
    calls: Arc<AtomicUsize>,
}
impl Scanner for FrameScores {
    fn version(&self) -> &str {
        "video-model-v1"
    }
    fn scan(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ScanResult {
                model_version: self.version().into(),
                scores: BTreeMap::from([(
                    "explicit".into(),
                    if bytes == [4] { 0.99 } else { 0.01 },
                )]),
                sampled_timestamps_ms: vec![],
            })
        })
    }
}
#[tokio::test]
async fn video_scans_five_frames_takes_maximum_and_caches_sample_metadata() {
    let (mut server, _, member, _, channel, _) = setup(0.1, false).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut runtime = automod::AutoMod::new(Arc::new(FrameScores {
        calls: calls.clone(),
    }));
    runtime.video = Arc::new(FiveFrames {
        count: 5,
        fail: false,
    });
    server.state.automod = Arc::new(runtime);
    // Filename and MIME still say PNG: the bytes must identify the video.
    let bytes = b"\0\0\0\x18ftypmp42 video bytes";
    for _ in 0..2 {
        let (_, body) = upload(&server, &member, &channel, bytes).await;
        automod::process_one(&server.state).await.unwrap();
        let held = automod::get(&server.state, id(&body)).await.unwrap();
        assert_eq!(held.status, "quarantined");
        let result: ScanResult = serde_json::from_str(held.result.as_ref().unwrap()).unwrap();
        assert_eq!(result.scores["explicit"], 0.99);
        assert_eq!(
            result.sampled_timestamps_ms,
            vec![1000, 3000, 5000, 7000, 9000]
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}
#[tokio::test]
async fn video_decode_failure_or_incomplete_sampling_keeps_upload_pending() {
    let (mut server, _, member, _, channel, calls) = setup(0.1, false).await;
    for (count, fail, bytes) in [
        (5, true, b"\0\0\0\x18ftypmp42 failure".as_slice()),
        (4, false, b"\0\0\0\x18ftypmp42 incomplete".as_slice()),
    ] {
        Arc::get_mut(&mut server.state.automod).unwrap().video =
            Arc::new(FiveFrames { count, fail });
        let (_, body) = upload(&server, &member, &channel, bytes).await;
        automod::process_one(&server.state).await.unwrap();
        assert_eq!(
            automod::get(&server.state, id(&body)).await.unwrap().status,
            "pending"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
#[ignore = "requires ffmpeg and ffprobe (or ACCORD_AUTOMOD_FFMPEG_PATH / ACCORD_AUTOMOD_FFPROBE_PATH)"]
async fn real_video_sampler_extracts_five_spaced_frames() {
    use automod::video::VideoSampler;
    let server = TestServer::new().await;
    let path = server.state.storage_path.join("sample.mp4");
    let ffmpeg = std::env::var("ACCORD_AUTOMOD_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".into());
    let output = tokio::process::Command::new(ffmpeg)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=128x96:rate=10:duration=10",
            "-threads",
            "1",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&path)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let start = std::time::Instant::now();
    let samples = automod::video::Ffmpeg::from_env()
        .sample(&path, automod::video::Container::Mov)
        .await
        .unwrap();
    eprintln!("Five-frame extraction: {:?}", start.elapsed());
    assert_eq!(samples.timestamps_ms, vec![1000, 3000, 5000, 7000, 9000]);
    assert_eq!(samples.frames.len(), 5);
    let mut hashes = std::collections::HashSet::new();
    for frame in samples.frames {
        let image = automod::scanner::decode(&frame).unwrap();
        assert_eq!(image.dimensions(), (320, 320));
        hashes.insert(automod::hash(&frame));
    }
    assert_eq!(hashes.len(), 5);
}

#[tokio::test]
async fn existing_system_username_cannot_stall_or_impersonate_automod() {
    let (server, _, member, _, channel, _) = setup(0.1, false).await;
    let named_system = server.create_user_with_token("System").await;
    let (_, body) = upload(&server, &member, &channel, b"allowed").await;
    automod::process_one(&server.state).await.unwrap();
    assert_eq!(
        automod::get(&server.state, id(&body)).await.unwrap().status,
        "published"
    );
    assert!(
        !db::users::get_user(server.pool(), &named_system.user.id)
            .await
            .unwrap()
            .system
    );
    let actor = db::users::get_or_create_system_user(server.pool())
        .await
        .unwrap();
    assert_ne!(actor, named_system.user.id);
}

#[tokio::test]
async fn upload_response_does_not_wait_for_an_in_flight_scan() {
    let (server, _, member, _, channel, _) = setup(0.1, false).await;
    // Simulate a slow scan. Mutation middleware must not wait on its lock
    // when it drains the independent attachment-deletion queue.
    let _scan = server.state.automod.processing.lock().await;
    let (status, body) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        upload(&server, &member, &channel, b"queued while busy"),
    )
    .await
    .expect("upload waited for scan completion");
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

/// An automod decision writes a space audit log entry, and moderators watching
/// the gateway must see it live rather than only after a reload.
#[tokio::test]
async fn automod_decisions_broadcast_their_audit_log_entry() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let (server, owner, member, space, channel, _) = setup(0.95, false).await;
    let base = server.spawn().await.replace("http://", "ws://");
    let (mut socket, _) = connect_async(format!("{base}/ws")).await.unwrap();
    socket.next().await.unwrap().unwrap();
    socket
        .send(Message::Text(
            json!({"op":2,"data":{"token":owner.gateway_token(),"intents":["moderation"]}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    loop {
        if let Message::Text(text) = socket.next().await.unwrap().unwrap() {
            if serde_json::from_str::<Value>(&text).unwrap()["type"] == "ready" {
                break;
            }
        }
    }

    upload(&server, &member, &channel, b"explicit").await;
    automod::process_one(&server.state).await.unwrap();

    let entry = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = socket.next().await {
            if let Message::Text(text) = event.unwrap() {
                let event: Value = serde_json::from_str(&text).unwrap();
                if event["type"] == "audit_log.create" {
                    return event["data"].clone();
                }
            }
        }
        panic!("gateway closed before the audit entry arrived");
    })
    .await
    .expect("audit_log.create was never broadcast");

    assert_eq!(entry["action_type"], "automod.quarantined");
    assert_eq!(entry["space_id"], space);
    assert_eq!(entry["target_type"], "attachment");
    socket.close(None).await.unwrap();

    // The broadcast reflects a row that is really there.
    let (rows,): (i64,) = sqlx::query_as(&db::q(
        "SELECT COUNT(*) FROM audit_log WHERE space_id=? AND action_type='automod.quarantined'",
    ))
    .bind(&space)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(rows, 1);
}
