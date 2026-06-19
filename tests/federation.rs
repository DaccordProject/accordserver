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
use serial_test::serial;
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
#[serial]
async fn cross_server_dm_round_trip() {
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

    let alice = a.create_user_with_token("alice").await;
    let bob = b.create_user_with_token("bob").await;
    let bob_q = format!("{}@b.test", bob.user.id);

    // alice (on A) opens a DM with bob (on B). The DM is anchored on whichever
    // server holds the lexicographically smaller qualified user id.
    let chan_a = accordserver::federation::dm::open_dm(&a.state, &alice.user, &bob_q)
        .await
        .unwrap();

    let origin_a = accordserver::db::federation::channel_origin(a.pool(), &chan_a.id)
        .await
        .unwrap();
    let home_domain = origin_a.clone().unwrap_or_else(|| "a.test".to_string());
    let dm_bare = chan_a.id.split('@').next().unwrap().to_string();
    let id_on_a = chan_a.id.clone();
    let id_on_b = if home_domain == "b.test" {
        dm_bare.clone()
    } else {
        format!("{dm_bare}@a.test")
    };

    // Both servers mirror the DM with both participants.
    assert_eq!(
        accordserver::db::dm_participants::count_participants(a.pool(), &id_on_a)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        accordserver::db::dm_participants::count_participants(b.pool(), &id_on_b)
            .await
            .unwrap(),
        2
    );

    // Send a message from the replica side; the home persists and fans it back.
    let (replica_state, home_state, replica_pool, replica_channel, replica_user) =
        if home_domain == "a.test" {
            (&b.state, &a.state, b.pool(), id_on_b.clone(), &bob.user)
        } else {
            (&a.state, &b.state, a.pool(), id_on_a.clone(), &alice.user)
        };

    let payload = accordserver::federation::dm::forward_dm_message(
        replica_state,
        &home_domain,
        &replica_channel,
        replica_user,
        "hello over a federated dm",
        None,
    )
    .await
    .unwrap();
    assert_eq!(payload["content"], "hello over a federated dm");

    // Home fans the message back to the replica's server.
    assert_eq!(
        accordserver::federation::sender::deliver_due_once(home_state).await,
        1
    );
    let msg_id = payload["id"].as_str().unwrap();
    let on_replica = accordserver::db::messages::get_message_row(replica_pool, msg_id)
        .await
        .unwrap();
    assert_eq!(on_replica.content, "hello over a federated dm");
    assert_eq!(on_replica.origin.as_deref(), Some(home_domain.as_str()));

    std::env::remove_var("ACCORD_FEDERATION_ALLOW_INSECURE");
}

#[tokio::test]
#[serial]
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

    // --- Edit round-trip ---
    accordserver::federation::forward::forward_edit(
        &a.state,
        "b.test",
        &on_a.id,
        &alice.user,
        "edited by alice",
    )
    .await
    .unwrap();
    assert_eq!(
        accordserver::db::messages::get_message_row(b.pool(), msg_bare)
            .await
            .unwrap()
            .content,
        "edited by alice"
    );
    assert_eq!(
        accordserver::federation::sender::deliver_due_once(&b.state).await,
        1
    );
    assert_eq!(
        accordserver::db::messages::get_message_row(a.pool(), &msg_qualified)
            .await
            .unwrap()
            .content,
        "edited by alice"
    );

    // --- Typing round-trip (ephemeral; assert the delivery path runs) ---
    accordserver::federation::forward::forward_typing(
        &a.state,
        "b.test",
        &mirrored_chan,
        &alice.user,
    )
    .await
    .unwrap();
    assert_eq!(
        accordserver::federation::sender::deliver_due_once(&b.state).await,
        1
    );

    // --- Delete round-trip ---
    accordserver::federation::forward::forward_delete(&a.state, "b.test", &on_a.id, &alice.user)
        .await
        .unwrap();
    assert!(
        accordserver::db::messages::get_message_row(b.pool(), msg_bare)
            .await
            .is_err()
    );
    assert_eq!(
        accordserver::federation::sender::deliver_due_once(&b.state).await,
        1
    );
    assert!(
        accordserver::db::messages::get_message_row(a.pool(), &msg_qualified)
            .await
            .is_err()
    );

    // --- Leave round-trip ---
    // alice leaves the remote-homed space -> forwarded to B, which drops her.
    accordserver::federation::forward::forward_leave(&a.state, "b.test", &mirrored, &alice.user)
        .await
        .unwrap();
    let interested = accordserver::db::federation::interested_servers(b.pool(), &space_b)
        .await
        .unwrap();
    assert!(
        !interested.contains(&"a.test".to_string()),
        "a.test should no longer be interested after alice leaves"
    );

    std::env::remove_var("ACCORD_FEDERATION_ALLOW_INSECURE");
}

async fn member_count(server: &TestServer, space_id: &str, user_id: &str) -> i64 {
    sqlx::query_scalar(&accordserver::db::q(
        "SELECT COUNT(*) FROM members WHERE space_id = ? AND user_id = ?",
    ))
    .bind(space_id)
    .bind(user_id)
    .fetch_one(server.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn inbound_message_with_bare_local_author_is_rejected() {
    // A trusted peer must not overwrite a LOCAL user row by claiming a bare
    // (unqualified) author ID that collides with a local snowflake (S2).
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, channel_id) = server.mirror_remote_space("b.test").await;

    // A local victim user with a bare snowflake ID.
    let victim = server.create_user_with_token("victim").await;

    let env = message_event(
        "evt-attack-1",
        "b.test",
        &space_id,
        &channel_id,
        "msg-attack@b.test",
        &victim.user.id, // bare local ID — the attack
        "pwned",
        "takeover",
    );
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);

    // The local user's row is untouched: username unchanged, still local.
    let row = accordserver::db::users::get_user(server.pool(), &victim.user.id)
        .await
        .unwrap();
    assert_eq!(row.username, "victim");
    assert!(row.origin.is_none());
}

#[tokio::test]
async fn inbound_reaction_with_bare_local_reactor_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, channel_id) = server.mirror_remote_space("b.test").await;
    let victim = server.create_user_with_token("victim").await;

    let env = json!({
        "event_id": "evt-attack-2", "origin": "b.test", "space_id": space_id,
        "type": "m.reaction.add",
        "payload": {
            "channel_id": channel_id,
            "message_id": "msg1@b.test",
            "user_id": victim.user.id, // bare local ID — the attack
            "emoji": "👍"
        }
    });
    let req = signed_request(&bob, "b.test", "a.test", INBOX, &env);
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
    let row = accordserver::db::users::get_user(server.pool(), &victim.user.id)
        .await
        .unwrap();
    assert_eq!(row.username, "victim");
    assert!(row.origin.is_none());
}

#[tokio::test]
async fn inbound_member_join_and_leave_applied() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, _channel_id) = server.mirror_remote_space("b.test").await;

    // A new member joins B's space.
    let join = json!({
        "event_id": "evt-join-1", "origin": "b.test", "space_id": space_id,
        "type": "m.member.join",
        "payload": { "user": { "id": "carol@b.test", "username": "carol", "display_name": "Carol" } }
    });
    let req = signed_request(&bob, "b.test", "a.test", INBOX, &join);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);
    assert_eq!(member_count(&server, &space_id, "carol@b.test").await, 1);

    // Then they leave.
    let leave = json!({
        "event_id": "evt-leave-1", "origin": "b.test", "space_id": space_id,
        "type": "m.member.leave",
        "payload": { "user_id": "carol@b.test" }
    });
    let req = signed_request(&bob, "b.test", "a.test", INBOX, &leave);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);
    assert_eq!(member_count(&server, &space_id, "carol@b.test").await, 0);
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

#[tokio::test]
async fn replayed_forward_request_is_rejected() {
    // The synchronous forward endpoints have no event-id dedup, so a verified
    // signature may be used only once within its skew window. Resending the
    // *identical* signed bytes must be rejected as a replay, even though the
    // first attempt's own outcome is unrelated.
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;

    let body_value = json!({
        "actor": { "id": "carol@b.test", "username": "carol" },
        "channel_id": "no-such-channel",
    });
    let body = serde_json::to_vec(&body_value).unwrap();
    // Sign once, then issue the same Date/Digest/Signature twice.
    let signed = sign_request(
        &bob,
        "b.test",
        "POST",
        "/federation/v1/typing",
        "a.test",
        &body,
    );
    let build = || {
        Request::builder()
            .method(Method::POST)
            .uri("/federation/v1/typing")
            .header("Date", &signed.date)
            .header("Digest", &signed.digest)
            .header("Signature", &signed.signature)
            .header("Content-Type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };

    // First use consumes the signature; its (failing) serve outcome is not 401.
    let first = status_of(&server, build()).await;
    assert_ne!(
        first,
        StatusCode::UNAUTHORIZED,
        "first use must pass signature verification"
    );
    // Identical resend is a replay.
    assert_eq!(status_of(&server, build()).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn home_react_rejects_message_from_a_different_channel() {
    // serve_react re-derives the target from our own DB: a forwarded reaction
    // whose message_id belongs to a different channel than the (permission-
    // scoped) channel_id must be rejected, never silently reattached.
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;

    // A locally-homed space with two channels; a message lives in channel one.
    let owner = server.create_user_with_token("owner").await;
    let space_id = server.create_space(&owner.user.id, "Home").await;
    let chan_one = server.create_channel(&space_id, "one").await;
    let chan_two = server.create_channel(&space_id, "two").await;
    let msg = accordserver::db::messages::create_message(
        server.pool(),
        &chan_one,
        &owner.user.id,
        Some(&space_id),
        &accordserver::models::message::CreateMessage {
            content: "hi".to_string(),
            tts: None,
            embeds: None,
            reply_to: None,
            thread_id: None,
            title: None,
        },
    )
    .await
    .unwrap();

    // A remote member (homed on the signing peer) of the space.
    accordserver::db::users::upsert_remote_user(
        server.pool(),
        "carol@b.test",
        "b.test",
        "carol@b.test",
        Some("Carol"),
        None,
    )
    .await
    .unwrap();
    server.add_member(&space_id, "carol@b.test").await;

    // Reaction scoped to channel two but targeting the channel-one message.
    // (`remove` needs only membership, isolating the target-mismatch check from
    // add_reactions permission defaults.)
    let body = json!({
        "actor": { "id": "carol@b.test", "username": "carol" },
        "channel_id": chan_two,
        "message_id": msg.id,
        "emoji": "👍",
        "remove": true,
    });
    let req = signed_request(&bob, "b.test", "a.test", "/federation/v1/react", &body);
    assert_eq!(status_of(&server, req).await, StatusCode::NOT_FOUND);

    // No reaction was recorded against the real message.
    let count: i64 = sqlx::query_scalar(&accordserver::db::q(
        "SELECT COUNT(*) FROM reactions WHERE message_id = ?",
    ))
    .bind(&msg.id)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// Emoji federation
// ---------------------------------------------------------------------------

/// A trusted peer's `m.emoji.create` is mirrored into a space we replicate, with
/// the right origin and image URL, and broadcast to local gateway sessions.
#[tokio::test]
async fn inbound_emoji_create_is_mirrored_and_broadcast() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, _channel_id) = server.mirror_remote_space("b.test").await;

    let mut rx = server
        .state
        .gateway_tx
        .read()
        .await
        .as_ref()
        .unwrap()
        .subscribe();

    let env = json!({
        "event_id": "evt-emoji-1",
        "origin": "b.test",
        "space_id": space_id,
        "type": "m.emoji.create",
        "payload": {
            "id": "emo1@b.test",
            "space_id": space_id,
            "name": "partyblob",
            "animated": true,
            "image_url": "https://b.test/cdn/emojis/space1/emo1.gif",
            "role_ids": [],
        }
    });
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);

    let emoji = accordserver::db::emojis::get_emoji(server.pool(), "emo1@b.test")
        .await
        .unwrap();
    assert_eq!(emoji.name, "partyblob");
    assert!(emoji.animated);
    assert_eq!(
        emoji.image_url.as_deref(),
        Some("https://b.test/cdn/emojis/space1/emo1.gif")
    );
    assert_eq!(
        accordserver::db::emojis::emoji_origin(server.pool(), "emo1@b.test")
            .await
            .unwrap()
            .as_deref(),
        Some("b.test")
    );

    let broadcast = rx.try_recv().expect("expected a gateway broadcast");
    assert_eq!(broadcast.event["type"], "emoji.create");
    assert_eq!(broadcast.event["data"]["emoji"]["id"], "emo1@b.test");
}

/// An `m.emoji.delete` from the home peer removes the mirrored emoji.
#[tokio::test]
async fn inbound_emoji_delete_removes_mirror() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, _channel_id) = server.mirror_remote_space("b.test").await;

    accordserver::db::emojis::upsert_remote_emoji(
        server.pool(),
        "emo1@b.test",
        "b.test",
        &space_id,
        "blob",
        false,
        Some("https://b.test/cdn/emojis/space1/emo1.png"),
        &[],
    )
    .await
    .unwrap();

    let env = json!({
        "event_id": "evt-emoji-del",
        "origin": "b.test",
        "space_id": space_id,
        "type": "m.emoji.delete",
        "payload": { "id": "emo1@b.test", "space_id": space_id }
    });
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::OK);

    assert!(
        accordserver::db::emojis::get_emoji(server.pool(), "emo1@b.test")
            .await
            .is_err(),
        "emoji should have been deleted"
    );
}

/// A bare-local emoji id in an inbound emoji event is rejected (S2): federation
/// may never create or overwrite a local emoji row.
#[tokio::test]
async fn inbound_emoji_with_bare_local_id_is_rejected() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let bob = peer_identity("b");
    register_peer(&server, "b.test", &bob, "trusted").await;
    let (space_id, _channel_id) = server.mirror_remote_space("b.test").await;

    let env = json!({
        "event_id": "evt-emoji-bad",
        "origin": "b.test",
        "space_id": space_id,
        "type": "m.emoji.create",
        "payload": {
            "id": "999",
            "space_id": space_id,
            "name": "evil",
            "image_url": "https://b.test/cdn/emojis/space1/x.png",
            "role_ids": [],
        }
    });
    let req = signed_inbox_request(&bob, "b.test", "a.test", &env);
    assert_eq!(status_of(&server, req).await, StatusCode::FORBIDDEN);
}

/// Creating an emoji in a locally-homed space fans it out to interested peers
/// with qualified IDs and an absolute image URL.
#[tokio::test]
async fn local_emoji_create_fans_out_to_interested_peers() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");

    let owner = server.create_user_with_token("alice").await;
    let space_id = server.create_space(&owner.user.id, "Local Space").await;

    accordserver::db::users::upsert_remote_user(
        server.pool(),
        "carol@b.test",
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
        "carol@b.test",
        Some("b.test"),
    )
    .await
    .unwrap();

    let emoji = accordserver::db::emojis::create_emoji(
        server.pool(),
        &space_id,
        &owner.user.id,
        &accordserver::models::emoji::CreateEmoji {
            name: "blobwave".to_string(),
            image: String::new(),
        },
        Some("/cdn/emojis/local/emo.png"),
        Some("image/png"),
        Some(10),
        false,
    )
    .await
    .unwrap();

    let payload = accordserver::federation::outbound::emoji_payload(
        "a.test",
        "https://a.test",
        &space_id,
        &emoji,
    );
    accordserver::federation::outbound::fanout_to_space(
        &server.state,
        &space_id,
        "m.emoji.create",
        payload,
    )
    .await
    .unwrap();

    let queued = accordserver::db::federation::outbox_claim_due(server.pool(), 10)
        .await
        .unwrap();
    assert_eq!(queued.len(), 1, "expected one queued delivery");
    assert_eq!(queued[0].target_domain, "b.test");
    let env: Value = serde_json::from_str(&queued[0].payload).unwrap();
    assert_eq!(env["type"], "m.emoji.create");
    let emoji_id = emoji.id.unwrap();
    assert_eq!(env["payload"]["id"], format!("{emoji_id}@a.test"));
    assert_eq!(
        env["payload"]["image_url"],
        "https://a.test/cdn/emojis/local/emo.png"
    );
}

/// A join snapshot carrying emoji is applied as replica rows.
#[tokio::test]
async fn joiner_applies_snapshot_emoji() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let joiner = server.create_user_with_token("alice").await;

    let snapshot = json!({
        "space": { "id": "sp1@b.test", "name": "Remote", "slug": "remote-b", "owner_id": "owner@b.test" },
        "channels": [],
        "roles": [],
        "members": [ { "id": "owner@b.test", "username": "owner" } ],
        "messages": [],
        "emojis": [ {
            "id": "emo1@b.test", "space_id": "sp1@b.test", "name": "blob",
            "animated": false, "image_url": "https://b.test/cdn/emojis/sp1/emo1.png", "role_ids": []
        } ],
    });

    accordserver::federation::handshake::apply_snapshot(
        &server.state,
        "b.test",
        &joiner.user.id,
        snapshot,
    )
    .await
    .unwrap();

    let emoji = accordserver::db::emojis::get_emoji(server.pool(), "emo1@b.test")
        .await
        .unwrap();
    assert_eq!(emoji.name, "blob");
    assert_eq!(
        accordserver::db::emojis::emoji_origin(server.pool(), "emo1@b.test")
            .await
            .unwrap()
            .as_deref(),
        Some("b.test")
    );
}
