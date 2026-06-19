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

/// Build a signed POST to `path`. When `signer` differs from the peer whose key
/// is registered, the signature will fail to verify.
fn signed_request(
    signer: &ServerIdentity,
    key_id: &str,
    target_domain: &str,
    path: &str,
    body_value: &Value,
) -> Request<Body> {
    let body = serde_json::to_vec(body_value).unwrap();
    let signed = sign_request(signer, key_id, "POST", path, target_domain, &body);
    Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("Date", signed.date)
        .header("Digest", signed.digest)
        .header("Signature", signed.signature)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

/// Build a signed inbox request.
fn signed_inbox_request(
    signer: &ServerIdentity,
    key_id: &str,
    target_domain: &str,
    envelope: &Value,
) -> Request<Body> {
    signed_request(signer, key_id, target_domain, INBOX, envelope)
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
async fn inbound_message_for_foreign_space_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (_space_id, _channel_id) = server.mirror_remote_space("b.test").await;

    // b.test signs but the message id/channel are homed on c.test — b.test is
    // not authoritative for that space (S1). (envelope.space_id stays on b.test
    // so authority::check passes; the per-field bind in the applier catches it.)
    let env = message_event(
        "evt-msg-1",
        "b.test",
        "space1@b.test",
        "chan1@c.test",
        "msg1@c.test",
        "bob@b.test",
        "bob",
        "foreign",
    );
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
    assert!(
        accordserver::db::messages::get_message_row(server.pool(), "msg1@c.test")
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

const JOIN: &str = "/federation/v1/join";

/// Full two-server round-trip over real HTTP: A's user joins a space homed on
/// B, posts into it (forwarded to B, the authority), and the message fans back
/// to A. Exercises signing, SSRF-allow for loopback, join handshake, the
/// remote-homed forward path, and outbound delivery end-to-end.
#[tokio::test]
async fn two_server_message_round_trip() {
    // Permit loopback/http peers for this in-process two-node test only.
    std::env::set_var("ACCORD_FEDERATION_ALLOW_INSECURE", "1");

    let mut a = TestServer::new().await;
    a.enable_federation("a.test");
    let mut b = TestServer::new().await;
    b.enable_federation("b.test");

    let a_pub = a
        .state
        .federation
        .as_ref()
        .unwrap()
        .identity
        .public_key_b64();
    let b_pub = b
        .state
        .federation
        .as_ref()
        .unwrap()
        .identity
        .public_key_b64();

    let base_a = a.spawn().await;
    let base_b = b.spawn().await;

    // Cross-register peers: logical domain + real spawned inbox URL.
    accordserver::db::federation::upsert_peer(
        a.pool(),
        "b.test",
        &b_pub,
        &format!("{base_b}{INBOX}"),
        "trusted",
    )
    .await
    .unwrap();
    accordserver::db::federation::upsert_peer(
        b.pool(),
        "a.test",
        &a_pub,
        &format!("{base_a}{INBOX}"),
        "trusted",
    )
    .await
    .unwrap();

    // B homes a federated space.
    let owner = b.create_user_with_token("owner").await;
    let space_b = b.create_space(&owner.user.id, "Federated").await;
    let chan_b = b.create_channel(&space_b, "general").await;
    accordserver::db::federation::set_space_federation_enabled(b.pool(), &space_b, true)
        .await
        .unwrap();

    // A's local user joins B's space (live POST /federation/v1/join + snapshot).
    let alice = a.create_user_with_token("alice").await;
    let mirrored =
        accordserver::federation::forward::initiate_join(&a.state, "b.test", &space_b, &alice.user)
            .await
            .unwrap();
    assert_eq!(mirrored, format!("{space_b}@b.test"));
    let joined = accordserver::db::spaces::list_space_ids_for_user(a.pool(), &alice.user.id)
        .await
        .unwrap();
    assert!(joined.contains(&mirrored));

    // alice (on A) posts into the remote-homed space -> forwarded to B.
    let mirrored_chan = format!("{chan_b}@b.test");
    let payload = accordserver::federation::forward::forward_message(
        &a.state,
        "b.test",
        &mirrored_chan,
        &alice.user,
        "hello from alice",
        None,
    )
    .await
    .unwrap();
    assert_eq!(payload["content"], "hello from alice");
    assert_eq!(payload["author"]["id"], format!("{}@a.test", alice.user.id));

    // B persisted it authoritatively (bare ID on B).
    let msg_qualified = payload["id"].as_str().unwrap().to_string();
    let msg_bare = msg_qualified.split('@').next().unwrap();
    let on_b = accordserver::db::messages::get_message_row(b.pool(), msg_bare)
        .await
        .unwrap();
    assert_eq!(on_b.content, "hello from alice");
    assert_eq!(on_b.author_id, format!("{}@a.test", alice.user.id));

    // B fans the message back out to A; flush the outbox and assert A mirrored it.
    let delivered = accordserver::federation::sender::deliver_due_once(&b.state).await;
    assert_eq!(delivered, 1);
    let on_a = accordserver::db::messages::get_message_row(a.pool(), &msg_qualified)
        .await
        .unwrap();
    assert_eq!(on_a.content, "hello from alice");
    assert_eq!(on_a.origin.as_deref(), Some("b.test"));

    // --- Reaction round-trip ---
    // alice reacts to her message in the remote-homed space -> forwarded to B.
    accordserver::federation::forward::forward_reaction(
        &a.state,
        "b.test",
        &mirrored_chan,
        &on_a.id,
        &alice.user,
        "👍",
        false,
    )
    .await
    .unwrap();
    let reactor = format!("{}@a.test", alice.user.id);
    let on_b_react: i64 = sqlx::query_scalar(&accordserver::db::q(
        "SELECT COUNT(*) FROM reactions WHERE message_id = ? AND user_id = ? AND emoji_name = ?",
    ))
    .bind(msg_bare)
    .bind(&reactor)
    .bind("👍")
    .fetch_one(b.pool())
    .await
    .unwrap();
    assert_eq!(on_b_react, 1);

    // B fans the reaction back to A.
    assert_eq!(
        accordserver::federation::sender::deliver_due_once(&b.state).await,
        1
    );
    let on_a_react: i64 = sqlx::query_scalar(&accordserver::db::q(
        "SELECT COUNT(*) FROM reactions WHERE message_id = ? AND user_id = ? AND emoji_name = ?",
    ))
    .bind(&msg_qualified)
    .bind(&reactor)
    .bind("👍")
    .fetch_one(a.pool())
    .await
    .unwrap();
    assert_eq!(on_a_react, 1);

    std::env::remove_var("ACCORD_FEDERATION_ALLOW_INSECURE");
}

#[tokio::test]
async fn inbound_message_update_and_delete_applied() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, channel_id) = server.mirror_remote_space("b.test").await;

    // Seed a replica message to edit/delete.
    let env = message_event(
        "evt-msg-1",
        "b.test",
        &space_id,
        &channel_id,
        "msg1@b.test",
        "bob@b.test",
        "bob",
        "original",
    );
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);

    // Edit.
    let edit = json!({
        "event_id": "evt-edit-1", "origin": "b.test", "space_id": space_id,
        "type": "m.message.update",
        "payload": { "id": "msg1@b.test", "content": "edited", "edited_at": "2026-01-02 00:00:00" }
    });
    let req = signed_request(&bob, "b.test", "a.test", INBOX, &edit);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);
    let row = accordserver::db::messages::get_message_row(server.pool(), "msg1@b.test")
        .await
        .unwrap();
    assert_eq!(row.content, "edited");

    // Delete.
    let del = json!({
        "event_id": "evt-del-1", "origin": "b.test", "space_id": space_id,
        "type": "m.message.delete",
        "payload": { "id": "msg1@b.test", "channel_id": channel_id }
    });
    let req = signed_request(&bob, "b.test", "a.test", INBOX, &del);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);
    assert!(
        accordserver::db::messages::get_message_row(server.pool(), "msg1@b.test")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn home_serves_join_and_records_remote_member() {
    let mut home = TestServer::new().await;
    home.enable_federation("b.test");
    let alice_srv = peer_identity("a"); // a.test is the requesting server
    register_peer(&home, "a.test", &alice_srv, "trusted").await;

    // A local, federation-enabled space on the home server.
    let owner = home.create_user_with_token("owner").await;
    let space_id = home.create_space(&owner.user.id, "Federated Space").await;
    home.create_channel(&space_id, "general").await;
    accordserver::db::federation::set_space_federation_enabled(home.pool(), &space_id, true)
        .await
        .unwrap();

    let join = json!({
        "user": { "id": "alice@a.test", "username": "alice", "display_name": "Alice" },
        "space_id": space_id,
    });
    let req = signed_request(&alice_srv, "a.test", "b.test", JOIN, &join);
    let resp = home.router().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Snapshot is qualified to the home domain.
    let snap = common::parse_body(resp).await;
    assert_eq!(snap["space"]["id"], format!("{space_id}@b.test"));
    assert!(!snap["channels"].as_array().unwrap().is_empty());

    // The remote user is now a member -> a.test is an interested server.
    let interested = accordserver::db::federation::interested_servers(home.pool(), &space_id)
        .await
        .unwrap();
    assert_eq!(interested, vec!["a.test".to_string()]);
}

#[tokio::test]
async fn home_refuses_join_for_non_federated_space() {
    let mut home = TestServer::new().await;
    home.enable_federation("b.test");
    let alice_srv = peer_identity("a");
    register_peer(&home, "a.test", &alice_srv, "trusted").await;

    let owner = home.create_user_with_token("owner").await;
    let space_id = home.create_space(&owner.user.id, "Private Space").await;
    // federation_enabled left at default (off).

    let join = json!({
        "user": { "id": "alice@a.test", "username": "alice" },
        "space_id": space_id,
    });
    let req = signed_request(&alice_srv, "a.test", "b.test", JOIN, &join);
    assert_eq!(status_of(&home, req).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn joiner_applies_snapshot_as_replica() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let joiner = server.create_user_with_token("alice").await;

    let snapshot = json!({
        "space": { "id": "sp1@b.test", "name": "Remote", "slug": "remote-b", "owner_id": "owner@b.test" },
        "channels": [ { "id": "ch1@b.test", "space_id": "sp1@b.test", "name": "general", "type": "text", "position": 0 } ],
        "roles": [ { "id": "r1@b.test", "space_id": "sp1@b.test", "name": "@everyone", "position": 0, "permissions": "[]" } ],
        "members": [ { "id": "owner@b.test", "username": "owner", "display_name": "Owner" } ],
        "messages": [ {
            "id": "m1@b.test", "channel_id": "ch1@b.test", "space_id": "sp1@b.test",
            "author": { "id": "owner@b.test", "username": "owner" },
            "content": "welcome", "created_at": "2026-01-01 00:00:00"
        } ],
    });

    let space_id = accordserver::federation::handshake::apply_snapshot(
        &server.state,
        "b.test",
        &joiner.user.id,
        snapshot,
    )
    .await
    .unwrap();
    assert_eq!(space_id, "sp1@b.test");

    // Replica rows exist.
    accordserver::db::spaces::get_space_row(server.pool(), "sp1@b.test")
        .await
        .unwrap();
    accordserver::db::channels::get_channel_row(server.pool(), "ch1@b.test")
        .await
        .unwrap();
    let msg = accordserver::db::messages::get_message_row(server.pool(), "m1@b.test")
        .await
        .unwrap();
    assert_eq!(msg.origin.as_deref(), Some("b.test"));

    // The local joiner is a member of the mirrored space.
    let spaces = accordserver::db::spaces::list_space_ids_for_user(server.pool(), &joiner.user.id)
        .await
        .unwrap();
    assert!(spaces.contains(&"sp1@b.test".to_string()));
}

#[tokio::test]
async fn joiner_rejects_snapshot_targeting_local_space() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let joiner = server.create_user_with_token("alice").await;

    // space.id is a bare/local id — must be rejected (S2).
    let snapshot = json!({
        "space": { "id": "999", "name": "Evil", "slug": "evil", "owner_id": "owner@b.test" },
        "channels": [], "roles": [], "members": [], "messages": [],
    });
    let res = accordserver::federation::handshake::apply_snapshot(
        &server.state,
        "b.test",
        &joiner.user.id,
        snapshot,
    )
    .await;
    assert!(res.is_err());
}

#[tokio::test]
async fn dedup_cleanup_prunes_old_rows() {
    let server = TestServer::new().await;

    // An old dedup row (well beyond retention) and a fresh one.
    sqlx::query(&accordserver::db::q(
        "INSERT INTO federation_inbox_dedup (event_id, origin, received_at) VALUES (?, ?, ?)",
    ))
    .bind("old-evt")
    .bind("b.test")
    .bind("2000-01-01 00:00:00")
    .execute(server.pool())
    .await
    .unwrap();
    accordserver::db::federation::dedup_first_seen(server.pool(), "fresh-evt", "b.test")
        .await
        .unwrap();

    let pruned = accordserver::db::federation::cleanup_dedup(server.pool(), 24 * 3600)
        .await
        .unwrap();
    assert_eq!(pruned, 1);

    // The fresh row survives (still deduped).
    let still_dup =
        !accordserver::db::federation::dedup_first_seen(server.pool(), "fresh-evt", "b.test")
            .await
            .unwrap();
    assert!(still_dup);
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
