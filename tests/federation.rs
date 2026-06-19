//! Phase 0 federation tests: the signed-inbox security envelope.
//!
//! These exercise the inbound `POST /federation/v1/inbox` pipeline end-to-end
//! through the real router (via `oneshot`), proving signature verification,
//! authority binding, the trust gate, and dedup — without needing real ports.

mod common;

use accordserver::federation::identity::ServerIdentity;
use accordserver::federation::signatures::sign_request;
use axum::body::Body;
use common::TestServer;
use http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

const INBOX: &str = "/federation/v1/inbox";

/// A throwaway signing identity for a peer "server" in a test.
fn peer_identity(tag: &str) -> ServerIdentity {
    let dir = accordserver::storage::temp_storage_path();
    ServerIdentity::load_or_create(&dir.join(format!("{tag}-key"))).unwrap()
}

/// Build a signed inbox request. When `signer` differs from the peer whose key
/// is registered, the signature will fail to verify.
fn signed_inbox_request(
    signer: &ServerIdentity,
    key_id: &str,
    target_domain: &str,
    envelope: &Value,
) -> Request<Body> {
    let body = serde_json::to_vec(envelope).unwrap();
    let signed = sign_request(signer, key_id, "POST", INBOX, target_domain, &body);
    Request::builder()
        .method(Method::POST)
        .uri(INBOX)
        .header("Date", signed.date)
        .header("Digest", signed.digest)
        .header("Signature", signed.signature)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn ping(event_id: &str, origin: &str) -> Value {
    json!({ "event_id": event_id, "origin": origin, "type": "m.ping", "payload": {} })
}

/// Build an `m.message.create` envelope homed on `origin`.
#[allow(clippy::too_many_arguments)]
fn message_event(
    event_id: &str,
    origin: &str,
    space_id: &str,
    channel_id: &str,
    msg_id: &str,
    author_id: &str,
    author_username: &str,
    content: &str,
) -> Value {
    json!({
        "event_id": event_id,
        "origin": origin,
        "space_id": space_id,
        "type": "m.message.create",
        "payload": {
            "id": msg_id,
            "channel_id": channel_id,
            "space_id": space_id,
            "author": { "id": author_id, "username": author_username },
            "content": content,
            "created_at": "2026-01-01 00:00:00",
        }
    })
}

/// Register peer `domain` on `server` with `identity`'s public key and trust.
async fn register_peer(server: &TestServer, domain: &str, identity: &ServerIdentity, trust: &str) {
    accordserver::db::federation::upsert_peer(
        server.pool(),
        domain,
        &identity.public_key_b64(),
        &format!("https://{domain}{INBOX}"),
        trust,
    )
    .await
    .unwrap();
}

async fn status_of(server: &TestServer, req: Request<Body>) -> StatusCode {
    server.router().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn well_known_is_served_when_federated() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");

    let req = Request::builder()
        .method(Method::GET)
        .uri("/.well-known/accord-federation")
        .body(Body::empty())
        .unwrap();
    let resp = server.router().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = common::parse_body(resp).await;
    assert_eq!(body["domain"], "b.test");
    assert!(body["public_key"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(body["inbox_url"], "https://b.test/federation/v1/inbox");
}

#[tokio::test]
async fn well_known_404_without_federation() {
    let server = TestServer::new().await;
    let req = Request::builder()
        .method(Method::GET)
        .uri("/.well-known/accord-federation")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(&server, req).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn signed_ping_from_trusted_peer_is_accepted() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let alice = peer_identity("a");
    register_peer(&server, "a.test", &alice, "trusted").await;

    let req = signed_inbox_request(&alice, "a.test", "b.test", &ping("evt-1", "a.test"));
    assert_eq!(status_of(&server, req).await, StatusCode::OK);
}

#[tokio::test]
async fn unknown_peer_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let alice = peer_identity("a");
    // Not registered.
    let req = signed_inbox_request(&alice, "a.test", "b.test", &ping("evt-1", "a.test"));
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn pending_peer_cannot_exchange_content() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let alice = peer_identity("a");
    register_peer(&server, "a.test", &alice, "pending").await;

    let req = signed_inbox_request(&alice, "a.test", "b.test", &ping("evt-1", "a.test"));
    // Signature is valid, but the trust gate rejects a pending peer (S4).
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let alice = peer_identity("a");
    register_peer(&server, "a.test", &alice, "trusted").await;

    // Sign with a *different* key while claiming to be a.test.
    let impostor = peer_identity("impostor");
    let req = signed_inbox_request(&impostor, "a.test", "b.test", &ping("evt-1", "a.test"));
    assert_eq!(status_of(&server, req).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn origin_spoofing_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let alice = peer_identity("a");
    register_peer(&server, "a.test", &alice, "trusted").await;

    // a.test signs correctly but claims the event originated on c.test (S1).
    let req = signed_inbox_request(&alice, "a.test", "b.test", &ping("evt-1", "c.test"));
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn duplicate_event_is_idempotent() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");
    let alice = peer_identity("a");
    register_peer(&server, "a.test", &alice, "trusted").await;

    let first = signed_inbox_request(&alice, "a.test", "b.test", &ping("dup-1", "a.test"));
    assert_eq!(status_of(&server, first).await, StatusCode::OK);

    // Same event_id+origin again: deduped, still acknowledged.
    let second = signed_inbox_request(&alice, "a.test", "b.test", &ping("dup-1", "a.test"));
    assert_eq!(status_of(&server, second).await, StatusCode::OK);
}

#[tokio::test]
async fn inbound_message_is_stored_and_broadcast() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, channel_id) = server.mirror_remote_space("b.test").await;

    // Subscribe to the gateway broadcast before delivering.
    let mut rx = server
        .state
        .gateway_tx
        .read()
        .await
        .as_ref()
        .unwrap()
        .subscribe();

    let env = message_event(
        "evt-msg-1",
        "b.test",
        &space_id,
        &channel_id,
        "msg1@b.test",
        "bob@b.test",
        "bob",
        "hello from b",
    );
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);

    // Stored as a replica with the correct origin.
    let row = accordserver::db::messages::get_message_row(server.pool(), "msg1@b.test")
        .await
        .unwrap();
    assert_eq!(row.content, "hello from b");
    assert_eq!(row.origin.as_deref(), Some("b.test"));
    assert_eq!(row.author_id, "bob@b.test");

    // Injected into the gateway.
    let broadcast = rx.try_recv().expect("expected a gateway broadcast");
    assert_eq!(broadcast.event["type"], "message.create");
    assert_eq!(broadcast.event["data"]["id"], "msg1@b.test");
    assert_eq!(broadcast.space_id.as_deref(), Some(space_id.as_str()));
}

#[tokio::test]
async fn inbound_message_with_spoofed_author_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, channel_id) = server.mirror_remote_space("b.test").await;

    // b.test signs correctly but the author is homed on c.test (S1).
    let env = message_event(
        "evt-msg-1",
        "b.test",
        &space_id,
        &channel_id,
        "msg1@b.test",
        "evil@c.test",
        "evil",
        "spoofed",
    );
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
    assert!(
        accordserver::db::messages::get_message_row(server.pool(), "msg1@b.test")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn inbound_message_for_unmirrored_channel_is_ignored() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    // No mirror_remote_space: we don't participate in this space.

    let env = message_event(
        "evt-msg-1",
        "b.test",
        "space1@b.test",
        "chan1@b.test",
        "msg1@b.test",
        "bob@b.test",
        "bob",
        "unsolicited",
    );
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    // Acknowledged but not stored.
    assert_eq!(status_of(&server, req).await, StatusCode::OK);
    assert!(
        accordserver::db::messages::get_message_row(server.pool(), "msg1@b.test")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn local_message_fans_out_to_interested_peers() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");

    // A local space with a local author and a remote member from b.test.
    let owner = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&owner.user.id, "Local Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let remote_member = "carol@b.test";
    accordserver::db::users::upsert_remote_user(
        server.pool(),
        remote_member,
        "b.test",
        "carol@b.test",
        Some("Carol"),
        None,
    )
    .await
    .unwrap();
    accordserver::db::federation::add_member_with_origin(
        server.pool(),
        &space_id,
        remote_member,
        Some("b.test"),
    )
    .await
    .unwrap();

    let msg = accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &owner.user.id,
        Some(&space_id),
        &accordserver::models::message::CreateMessage {
            content: "hi peers".into(),
            tts: None,
            embeds: None,
            reply_to: None,
            thread_id: None,
            title: None,
        },
    )
    .await
    .unwrap();

    accordserver::federation::outbound::fanout_message_create(&server.state, &msg)
        .await
        .unwrap();

    // An outbound delivery to b.test is queued, carrying a qualified message.
    let queued = accordserver::db::federation::outbox_claim_due(server.pool(), 10)
        .await
        .unwrap();
    assert_eq!(queued.len(), 1, "expected one queued delivery");
    assert_eq!(queued[0].target_domain, "b.test");
    let env: Value = serde_json::from_str(&queued[0].payload).unwrap();
    assert_eq!(env["type"], "m.message.create");
    assert_eq!(env["origin"], "a.test");
    assert_eq!(env["payload"]["id"], format!("{}@a.test", msg.id));
    assert_eq!(
        env["payload"]["author"]["id"],
        format!("{}@a.test", owner.user.id)
    );
}

#[tokio::test]
async fn local_message_without_remote_members_does_not_fan_out() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");

    let owner = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&owner.user.id, "Local Space").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let msg = accordserver::db::messages::create_message(
        server.pool(),
        &channel_id,
        &owner.user.id,
        Some(&space_id),
        &accordserver::models::message::CreateMessage {
            content: "nobody remote here".into(),
            tts: None,
            embeds: None,
            reply_to: None,
            thread_id: None,
            title: None,
        },
    )
    .await
    .unwrap();

    accordserver::federation::outbound::fanout_message_create(&server.state, &msg)
        .await
        .unwrap();

    let queued = accordserver::db::federation::outbox_claim_due(server.pool(), 10)
        .await
        .unwrap();
    assert!(queued.is_empty(), "no remote members -> no fanout");
}

#[tokio::test]
async fn missing_signature_is_unauthorized() {
    let mut server = TestServer::new().await;
    server.enable_federation("b.test");

    let body = serde_json::to_vec(&ping("evt-1", "a.test")).unwrap();
    let req = Request::builder()
        .method(Method::POST)
        .uri(INBOX)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap();
    assert_eq!(status_of(&server, req).await, StatusCode::UNAUTHORIZED);
}
