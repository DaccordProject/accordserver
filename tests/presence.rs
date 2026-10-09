mod common;

use accordserver::{
    federation::{
        identity::ServerIdentity,
        presence::{self, Exchange, PATH, TTL_MS},
        signatures::sign_request,
    },
    presence::{current_presence, offline_presence, set_presence},
};
use axum::body::Body;
use common::{authenticated_request, parse_body, TestServer};
use http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

fn identity() -> ServerIdentity {
    ServerIdentity::load_or_create(&accordserver::storage::temp_storage_path().join("presence-key"))
        .unwrap()
}
async fn peer(server: &TestServer, id: &ServerIdentity, trust: &str) {
    accordserver::db::federation::upsert_peer(
        server.pool(),
        "a.test",
        &id.public_key_b64(),
        "https://a.test/federation/v1/inbox",
        trust,
    )
    .await
    .unwrap();
}
fn signed(id: &ServerIdentity, body: Value) -> Request<Body> {
    let bytes = serde_json::to_vec(&body).unwrap();
    let s = sign_request(id, "a.test", "POST", PATH, "b.test", &bytes);
    Request::builder()
        .method(Method::POST)
        .uri(PATH)
        .header("Date", s.date)
        .header("Digest", s.digest)
        .header("Signature", s.signature)
        .header("Content-Type", "application/json")
        .body(Body::from(bytes))
        .unwrap()
}
async fn setup() -> (TestServer, String, String) {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let bob = server.create_user_with_token("bob").await;
    let space = server.create_space(&bob.user.id, "Shared").await;
    accordserver::db::users::upsert_remote_user(
        server.pool(),
        "alice@a.test",
        "a.test",
        "alice@a.test",
        None,
        None,
    )
    .await
    .unwrap();
    server.add_member(&space, "alice@a.test").await;
    (server, bob.user.id, space)
}
fn update(at: i64, status: &str) -> Exchange {
    let mut p = offline_presence("alice@a.test");
    p.status = status.into();
    Exchange {
        observed_at: at,
        presences: vec![p],
        after: None,
    }
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tokio::test]
async fn signed_exchange_publishes_remote_presence_and_returns_initial_local_snapshot() {
    let (server, bob, space) = setup().await;
    let key = identity();
    peer(&server, &key, "trusted").await;
    set_presence(&server.state, &bob, "dnd", vec![json!({"name":"Busy"})]);
    let mut rx = server
        .state
        .gateway_tx
        .read()
        .await
        .as_ref()
        .unwrap()
        .subscribe();
    let response = server
        .router()
        .oneshot(signed(
            &key,
            serde_json::to_value(update(now(), "online")).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let rows = body["presences"].as_array().unwrap();
    assert!(rows
        .iter()
        .any(|p| p["user_id"] == format!("{bob}@b.test") && p["status"] == "dnd"));
    assert_eq!(
        current_presence(&server.state, "alice@a.test")
            .unwrap()
            .status,
        "online"
    );
    let event = rx.try_recv().unwrap();
    assert_eq!(event.space_id.as_deref(), Some(space.as_str()));
    assert_eq!(event.event["type"], "presence.update");
    assert_eq!(event.event["data"]["user_id"], "alice@a.test");
    let queued = accordserver::db::federation::outbox_claim_due(server.pool(), 10)
        .await
        .unwrap();
    assert!(
        queued.is_empty(),
        "ephemeral presence must not enter durable outbox"
    );
}

#[tokio::test]
async fn exchange_masks_invisible_and_does_not_return_unrelated_local_users() {
    let (server, bob, _) = setup().await;
    let outsider = server.create_user_with_token("outsider").await;
    set_presence(
        &server.state,
        &bob,
        "invisible",
        vec![json!({"name":"Secret"})],
    );
    set_presence(&server.state, &outsider.user.id, "online", vec![]);
    let key = identity();
    peer(&server, &key, "trusted").await;
    let response = server
        .router()
        .oneshot(signed(&key, json!({"observed_at":now(),"presences":[]})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let rows = body["presences"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "offline");
    assert_eq!(rows[0]["activities"], json!([]));
    assert!(rows[0]["client_status"]["desktop"].is_null());
}

#[tokio::test]
async fn snapshot_endpoint_checks_membership_and_preserves_only_self_invisible() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let outsider = server.create_user_with_token("outsider").await;
    let space = server.create_space(&alice.user.id, "Local").await;
    server.add_member(&space, &bob.user.id).await;
    set_presence(
        &server.state,
        &alice.user.id,
        "invisible",
        vec![json!({"name":"Private"})],
    );
    set_presence(&server.state, &bob.user.id, "online", vec![]);
    let path = format!("/api/v1/spaces/{space}/presences");
    let denied = server
        .router()
        .oneshot(authenticated_request(
            Method::GET,
            &path,
            &outsider.auth_header(),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let response = server
        .router()
        .oneshot(authenticated_request(
            Method::GET,
            &path,
            &bob.auth_header(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    let p = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["user_id"] == alice.user.id)
        .unwrap();
    assert_eq!(p["status"], "offline");
    assert_eq!(p["activities"], json!([]));
    let response = server
        .router()
        .oneshot(authenticated_request(
            Method::GET,
            &path,
            &alice.auth_header(),
        ))
        .await
        .unwrap();
    let body = parse_body(response).await;
    let p = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["user_id"] == alice.user.id)
        .unwrap();
    assert_eq!(p["status"], "invisible");
}

#[tokio::test]
async fn untrusted_and_invalid_signatures_cannot_publish_presence() {
    let (server, _, _) = setup().await;
    let key = identity();
    peer(&server, &key, "pending").await;
    let req = signed(&key, serde_json::to_value(update(now(), "online")).unwrap());
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    accordserver::db::federation::set_peer_trust(server.pool(), "a.test", "trusted")
        .await
        .unwrap();
    let impostor = identity();
    let req = signed(
        &impostor,
        serde_json::to_value(update(now(), "online")).unwrap(),
    );
    assert_eq!(
        server.router().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(current_presence(&server.state, "alice@a.test").is_none());
}

#[tokio::test]
async fn rejects_spoofed_users_nonmembers_and_malformed_batches_atomically() {
    let (server, _, _) = setup().await;
    let mut batch = update(now(), "online");
    let mut spoof = offline_presence("mallory@c.test");
    spoof.status = "online".into();
    batch.presences.push(spoof);
    assert!(presence::apply(&server.state, "a.test", &batch)
        .await
        .is_err());
    assert!(current_presence(&server.state, "alice@a.test").is_none());
    accordserver::db::users::upsert_remote_user(
        server.pool(),
        "nonmember@a.test",
        "a.test",
        "nonmember@a.test",
        None,
        None,
    )
    .await
    .unwrap();
    let mut req = update(now(), "online");
    req.presences[0].user_id = "nonmember@a.test".into();
    assert!(presence::apply(&server.state, "a.test", &req)
        .await
        .is_err());
    assert!(
        presence::apply(&server.state, "a.test", &update(now() + 60_000, "online"))
            .await
            .is_err()
    );
    assert!(
        presence::apply(&server.state, "a.test", &update(now(), "invalid"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn out_of_order_and_expired_updates_cannot_restore_online_presence() {
    let (server, _, _) = setup().await;
    let at = now();
    presence::apply(&server.state, "a.test", &update(at, "offline"))
        .await
        .unwrap();
    presence::apply(&server.state, "a.test", &update(at - 1, "online"))
        .await
        .unwrap();
    assert_eq!(
        current_presence(&server.state, "alice@a.test")
            .unwrap()
            .status,
        "offline"
    );
    presence::apply(&server.state, "a.test", &update(at - TTL_MS - 1, "online"))
        .await
        .unwrap();
    assert_eq!(
        current_presence(&server.state, "alice@a.test")
            .unwrap()
            .status,
        "offline"
    );
}

#[tokio::test]
async fn remote_presence_expires_and_broadcasts_offline_without_a_disconnect_event() {
    let (server, _, _) = setup().await;
    presence::apply(&server.state, "a.test", &update(now(), "online"))
        .await
        .unwrap();
    server
        .state
        .federated_presence
        .remote
        .get_mut("alice@a.test")
        .unwrap()
        .expires_at = now() - 1;
    assert_eq!(
        current_presence(&server.state, "alice@a.test")
            .unwrap()
            .status,
        "offline"
    );
    let mut rx = server
        .state
        .gateway_tx
        .read()
        .await
        .as_ref()
        .unwrap()
        .subscribe();
    presence::expire(&server.state).await;
    let event = rx.try_recv().unwrap();
    assert_eq!(event.event["data"]["status"], "offline");
    presence::expire(&server.state).await;
    assert!(rx.try_recv().is_err(), "expiry must emit only once");
}

#[tokio::test]
async fn remote_invisible_is_masked_even_if_a_peer_sends_the_raw_status() {
    let (server, _, _) = setup().await;
    let mut req = update(now(), "invisible");
    req.presences[0].activities.push(json!({"name":"Private"}));
    presence::apply(&server.state, "a.test", &req)
        .await
        .unwrap();
    let p = current_presence(&server.state, "alice@a.test").unwrap();
    assert_eq!(p.status, "offline");
    assert!(p.activities.is_empty());
}

#[tokio::test]
async fn signed_snapshots_page_all_members_without_disclosing_unrelated_users() {
    let (server, _, space) = setup().await;
    let key = identity();
    peer(&server, &key, "trusted").await;
    // 501 additional accounts plus Bob ensure the second page is required.
    for i in 0..501 {
        let id = format!("extra-{i:04}");
        sqlx::query(&accordserver::db::q(
            "INSERT INTO users (id, username, display_name) VALUES (?, ?, ?)",
        ))
        .bind(&id)
        .bind(&id)
        .bind(&id)
        .execute(server.pool())
        .await
        .unwrap();
        server.add_member(&space, &id).await;
    }
    let response = server
        .router()
        .oneshot(signed(&key, json!({"observed_at":now(),"presences":[]})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first = parse_body(response).await;
    assert_eq!(first["presences"].as_array().unwrap().len(), 500);
    let response = server
        .router()
        .oneshot(signed(
            &key,
            json!({"observed_at":now(),"presences":[],"after":first["after"]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let second = parse_body(response).await;
    assert_eq!(second["presences"].as_array().unwrap().len(), 2);
    assert!(second["after"].is_null());
    let first_ids: std::collections::HashSet<_> = first["presences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["user_id"].as_str().unwrap())
        .collect();
    assert!(second["presences"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| !first_ids.contains(p["user_id"].as_str().unwrap())));
}

#[tokio::test]
async fn snapshots_include_shared_local_users_in_remote_homed_spaces() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let key = identity();
    peer(&server, &key, "trusted").await;
    let bob = server.create_user_with_token("bob").await;
    let (space, _) = server.mirror_remote_space("a.test").await;
    server.add_member(&space, &bob.user.id).await;
    set_presence(&server.state, &bob.user.id, "idle", vec![]);
    let response = server
        .router()
        .oneshot(signed(&key, json!({"observed_at":now(),"presences":[]})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = parse_body(response).await;
    assert!(body["presences"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["user_id"] == format!("{}@b.test", bob.user.id) && p["status"] == "idle"));
}
