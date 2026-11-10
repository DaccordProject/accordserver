//! Private chat encryption v2: per-device keys, approval, rotation and
//! device-signed envelopes. Keys and seeds here are public test values.
mod common;
use accordserver::{
    db,
    e2ee::{self, account, Identity},
    middleware::auth::{create_token_hash, generate_token},
};
use common::{authenticated_json_request, parse_body, TestServer, TestUser};
use data_encoding::{BASE64, BASE64URL_NOPAD};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use http::{Method, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

/// An Ed25519 + X25519-shaped key pair. The server never performs X25519, so
/// any non-zero 32 bytes stand in for the exchange key.
#[derive(Clone)]
struct Keys {
    signing: SigningKey,
    exchange: String,
}
impl Keys {
    fn new(seed: u8) -> Self {
        Self {
            signing: SigningKey::from_bytes(&[seed; 32]),
            exchange: BASE64.encode(&[seed.wrapping_add(100); 32]),
        }
    }
    fn signing_key(&self) -> String {
        BASE64.encode(self.signing.verifying_key().as_bytes())
    }
    fn identity(&self) -> Identity {
        Identity {
            exchange_key: self.exchange.clone(),
            signing_key: self.signing_key(),
        }
    }
    fn sign(&self, message: &[u8]) -> String {
        BASE64.encode(&self.signing.sign(message).to_bytes())
    }
}

#[derive(Clone)]
struct Dev {
    id: String,
    keys: Keys,
}
impl Dev {
    fn new(seed: u8) -> Self {
        Self {
            id: BASE64URL_NOPAD.encode(&[seed; 16]),
            keys: Keys::new(seed),
        }
    }
}

fn certificate(account: &Keys, user_context: &str, version: i64, dev: &Dev) -> String {
    account.sign(&account::certificate_message(
        user_context,
        version,
        &dev.id,
        &dev.keys.exchange,
        &dev.keys.signing_key(),
    ))
}

/// A second bearer session for the same user.
async fn new_session(server: &TestServer, user: &TestUser) -> String {
    let token = generate_token();
    sqlx::query(&db::q(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, '2099-12-31T23:59:59')",
    ))
    .bind(create_token_hash(&token))
    .bind(&user.user.id)
    .execute(server.pool())
    .await
    .unwrap();
    format!("Bearer {token}")
}

async fn call(
    server: &TestServer,
    auth: &str,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(method, path, auth, &body))
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn register_account(server: &TestServer, user: &TestUser, keys: &Keys) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PUT,
            "/api/v1/users/@me/encryption",
            &user.auth_header(),
            &serde_json::to_value(keys.identity()).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    parse_body(response).await;
}

fn device_path(dev: &Dev) -> String {
    format!("/api/v1/users/@me/encryption/devices/{}", dev.id)
}

async fn put_device(
    server: &TestServer,
    auth: &str,
    dev: &Dev,
    name: &str,
    certified: Option<(String, i64)>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "name": name,
        "exchange_key": dev.keys.exchange,
        "signing_key": dev.keys.signing_key(),
    });
    if let Some((certificate, version)) = certified {
        body["certificate"] = json!(certificate);
        body["account_version"] = json!(version);
    }
    call(server, auth, Method::PUT, &device_path(dev), body).await
}

/// Register an active device with a certificate from the current account key.
async fn active_device(
    server: &TestServer,
    auth: &str,
    user: &TestUser,
    account: &Keys,
    version: i64,
    dev: &Dev,
) {
    let cert = certificate(account, &user.user.id, version, dev);
    let (status, body) = put_device(server, auth, dev, "active", Some((cert, version))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "active");
}

fn key_package() -> String {
    json!({"ephemeral": BASE64.encode(&[9; 32]), "box": BASE64.encode(&[1; 64])}).to_string()
}

fn subscribe(
    server: &TestServer,
) -> tokio::sync::broadcast::Receiver<accordserver::gateway::events::GatewayBroadcast> {
    server
        .state
        .gateway_tx
        .try_read()
        .unwrap()
        .as_ref()
        .unwrap()
        .subscribe()
}
fn events(
    rx: &mut tokio::sync::broadcast::Receiver<accordserver::gateway::events::GatewayBroadcast>,
) -> Vec<accordserver::gateway::events::GatewayBroadcast> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if event.event["type"]
            .as_str()
            .is_some_and(|t| t.starts_with("encryption."))
        {
            out.push(event);
        }
    }
    out
}

#[tokio::test]
async fn devices_register_approve_decline_and_packages() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let account_keys = Keys::new(7);
    let a = alice.auth_header();
    let b = new_session(&server, &alice).await;
    let c = new_session(&server, &alice).await;
    let d1 = Dev::new(31);
    let d2 = Dev::new(32);
    let d3 = Dev::new(33);
    let mut rx = subscribe(&server);

    // A certificate requires the account identity.
    let cert = certificate(&account_keys, &alice.user.id, 1, &d1);
    let (status, _) = put_device(&server, &a, &d1, "laptop", Some((cert.clone(), 1))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    register_account(&server, &alice, &account_keys).await;

    // Certificate failures.
    let wrong = certificate(&Keys::new(8), &alice.user.id, 1, &d1);
    assert_eq!(
        put_device(&server, &a, &d1, "laptop", Some((wrong, 1)))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let other_device = certificate(&account_keys, &alice.user.id, 1, &d2);
    assert_eq!(
        put_device(&server, &a, &d1, "laptop", Some((other_device, 1)))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let wrong_context = certificate(&account_keys, "someone-else", 1, &d1);
    assert_eq!(
        put_device(&server, &a, &d1, "laptop", Some((wrong_context, 1)))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let future = certificate(&account_keys, &alice.user.id, 2, &d1);
    assert_eq!(
        put_device(&server, &a, &d1, "laptop", Some((future, 2)))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (status, _) = call(
        &server,
        &a,
        Method::PUT,
        &device_path(&d1),
        json!({"name":"laptop","exchange_key":d1.keys.exchange,"signing_key":d1.keys.signing_key(),"certificate":cert}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &server,
        &a,
        Method::PUT,
        "/api/v1/users/@me/encryption/devices/short",
        json!({"name":"x","exchange_key":d1.keys.exchange,"signing_key":d1.keys.signing_key()}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        put_device(&server, &a, &d1, "", None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        put_device(&server, &a, &d1, &"n".repeat(65), None).await.0,
        StatusCode::BAD_REQUEST
    );

    // Self-certified device becomes active and is bound to session A.
    let (status, body) = put_device(&server, &a, &d1, "laptop", Some((cert, 1))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "active");
    assert_eq!(body["data"]["current"], true);
    assert_eq!(body["data"]["account_version"], 1);
    events(&mut rx);

    // A device without the keyring registers pending and notifies the user.
    let (status, body) = put_device(&server, &b, &d2, "phone", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["status"], "pending");
    assert_eq!(body["data"]["certificate"], Value::Null);
    let sent = events(&mut rx);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].event["type"], "encryption.device_pending");
    assert_eq!(sent[0].event["op"], 0);
    assert_eq!(sent[0].intent, "direct_messages");
    assert_eq!(sent[0].target_user_ids, Some(vec![alice.user.id.clone()]));
    assert_eq!(sent[0].event["data"]["device_id"], d2.id);
    assert_eq!(sent[0].event["data"]["name"], "phone");

    // Idempotent re-registration may rename; keys are immutable.
    let (status, body) = put_device(&server, &b, &d2, "my phone", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["name"], "my phone");
    assert!(events(&mut rx).is_empty());
    let mut changed = d2.clone();
    changed.keys = Keys::new(99);
    assert_eq!(
        put_device(&server, &b, &changed, "phone", None).await.0,
        StatusCode::CONFLICT
    );

    let (status, body) = call(
        &server,
        &a,
        Method::GET,
        "/api/v1/users/@me/encryption",
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["user_context"], alice.user.id);
    assert_eq!(data["account"]["version"], 1);
    assert_eq!(data["account"]["signing_key"], account_keys.signing_key());
    // Only version 1, published unsigned so later verifiers can check it.
    assert_eq!(
        data["successions"],
        json!([{"version": 1, "exchange_key": account_keys.exchange, "signing_key": account_keys.signing_key(), "signature": null}])
    );
    let devices = data["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    for row in devices {
        assert_eq!(row["current"], row["device_id"] == d1.id.as_str());
        for key in ["created_at", "last_seen_at"] {
            chrono::DateTime::parse_from_rfc3339(row[key].as_str().unwrap()).unwrap();
        }
    }

    // Approval: only from a session bound to an active device.
    let approve = format!("{}/approve", device_path(&d2));
    let good = json!({
        "certificate": certificate(&account_keys, &alice.user.id, 1, &d2),
        "account_version": 1,
        "key_package": key_package(),
    });
    assert_eq!(
        call(&server, &b, Method::POST, &approve, good.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&server, &c, Method::POST, &approve, good.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut bad = good.clone();
    bad["certificate"] = json!(certificate(&account_keys, &alice.user.id, 1, &d1));
    assert_eq!(
        call(&server, &a, Method::POST, &approve, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = good.clone();
    bad["key_package"] = json!("{\"ephemeral\":\"x\"}");
    assert_eq!(
        call(&server, &a, Method::POST, &approve, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = good.clone();
    bad["key_package"] = json!("x".repeat(16 * 1024 + 1));
    assert_eq!(
        call(&server, &a, Method::POST, &approve, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    let package_path = format!("{}/package", device_path(&d2));
    assert_eq!(
        call(&server, &b, Method::GET, &package_path, json!(null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = call(&server, &a, Method::POST, &approve, good.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"], "active");
    assert_eq!(body["data"]["current"], false);
    let sent = events(&mut rx);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].event["type"], "encryption.devices_update");
    assert_eq!(sent[0].event["data"], json!({}));
    assert_eq!(
        call(&server, &a, Method::POST, &approve, good).await.0,
        StatusCode::CONFLICT
    );

    // The package is readable and acknowledgeable only by the bound session.
    assert_eq!(
        call(&server, &a, Method::GET, &package_path, json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&server, &c, Method::DELETE, &package_path, json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = call(&server, &b, Method::GET, &package_path, json!(null)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["key_package"], key_package());
    assert_eq!(
        call(&server, &b, Method::DELETE, &package_path, json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&server, &b, Method::GET, &package_path, json!(null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Active devices are removed only by rotation.
    assert_eq!(
        call(&server, &a, Method::DELETE, &device_path(&d2), json!(null))
            .await
            .0,
        StatusCode::CONFLICT
    );

    // Declining a pending device deletes it (no tombstone).
    assert_eq!(
        put_device(&server, &c, &d3, "tablet", None).await.0,
        StatusCode::OK
    );
    events(&mut rx);
    assert_eq!(
        call(&server, &a, Method::DELETE, &device_path(&d3), json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        events(&mut rx)[0].event["type"],
        "encryption.devices_update"
    );
    assert_eq!(
        call(&server, &a, Method::DELETE, &device_path(&d3), json!(null))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = put_device(&server, &c, &d3, "tablet", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["status"], "pending");
    // A pending device can withdraw itself; other pending sessions cannot.
    let d4 = Dev::new(34);
    let e = new_session(&server, &alice).await;
    put_device(&server, &e, &d4, "other", None).await;
    assert_eq!(
        call(&server, &e, Method::DELETE, &device_path(&d3), json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&server, &c, Method::DELETE, &device_path(&d3), json!(null))
            .await
            .0,
        StatusCode::OK
    );

    // Bots cannot bind devices.
    let (_, bot) = server.create_bot_with_token("owner", "bot").await;
    assert_eq!(
        put_device(&server, &bot.auth_header(), &d4, "bot", None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn pending_devices_expire_and_device_count_is_capped() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let account_keys = Keys::new(7);
    register_account(&server, &alice, &account_keys).await;
    let a = alice.auth_header();
    let mut rx = subscribe(&server);
    let pending = Dev::new(40);
    assert_eq!(
        put_device(&server, &a, &pending, "old", None).await.0,
        StatusCode::OK
    );
    assert_eq!(events(&mut rx).len(), 1);
    sqlx::query(&db::q(
        "UPDATE e2ee_devices SET created_at=? WHERE device_id=?",
    ))
    .bind("2000-01-01T00:00:00.000Z")
    .bind(&pending.id)
    .execute(server.pool())
    .await
    .unwrap();
    let (_, body) = call(
        &server,
        &a,
        Method::GET,
        "/api/v1/users/@me/encryption",
        json!(null),
    )
    .await;
    assert_eq!(body["data"]["devices"], json!([]));
    let approve = format!("{}/approve", device_path(&pending));
    assert_eq!(
        call(
            &server,
            &a,
            Method::POST,
            &approve,
            json!({"certificate":certificate(&account_keys,&alice.user.id,1,&pending),"account_version":1,"key_package":key_package()})
        )
        .await
        .0,
        StatusCode::FORBIDDEN // no active device bound to this session
    );
    // Re-registration after expiry starts a fresh pending request.
    assert_eq!(
        put_device(&server, &a, &pending, "again", None).await.0,
        StatusCode::OK
    );
    assert_eq!(events(&mut rx).len(), 1);

    for seed in 41..60u8 {
        let session = new_session(&server, &alice).await;
        assert_eq!(
            put_device(&server, &session, &Dev::new(seed), "n", None)
                .await
                .0,
            StatusCode::OK
        );
    }
    let session = new_session(&server, &alice).await;
    let extra = Dev::new(60);
    assert_eq!(
        put_device(&server, &session, &extra, "n", None).await.0,
        StatusCode::BAD_REQUEST
    );
    // Re-registering an existing device is not a new device.
    assert_eq!(
        put_device(&server, &a, &pending, "again", None).await.0,
        StatusCode::OK
    );
    sqlx::query(&db::q(
        "UPDATE e2ee_devices SET created_at=? WHERE device_id=?",
    ))
    .bind("2000-01-01T00:00:00.000Z")
    .bind(&pending.id)
    .execute(server.pool())
    .await
    .unwrap();
    assert_eq!(
        put_device(&server, &session, &extra, "n", None).await.0,
        StatusCode::OK
    );
}

struct Rotation {
    body: Value,
}
fn rotation(
    user_context: &str,
    old: &Keys,
    new: &Keys,
    version: i64,
    revoke: &[&Dev],
    keep: &[&Dev],
    caller: &Dev,
) -> Rotation {
    let signature = old.sign(&account::succession_message(
        user_context,
        version,
        &new.identity(),
    ));
    Rotation {
        body: json!({
            "revoke": revoke.iter().map(|d| d.id.clone()).collect::<Vec<_>>(),
            "account": new.identity(),
            "version": version,
            "signature": signature,
            "devices": keep.iter().map(|d| json!({"device_id": d.id, "certificate": certificate(new, user_context, version, d)})).collect::<Vec<_>>(),
            "packages": keep.iter().filter(|d| d.id != caller.id).map(|d| json!({"device_id": d.id, "key_package": key_package()})).collect::<Vec<_>>(),
        }),
    }
}

#[tokio::test]
async fn rotation_revokes_devices_sessions_and_pending_requests() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let ctx = alice.user.id.clone();
    let v1 = Keys::new(7);
    let v2 = Keys::new(17);
    register_account(&server, &alice, &v1).await;
    register_account(&server, &bob, &Keys::new(8)).await;
    let (sa, sb, sc, sd) = (
        alice.auth_header(),
        new_session(&server, &alice).await,
        new_session(&server, &alice).await,
        new_session(&server, &alice).await,
    );
    let (d1, d2, d3, d4) = (Dev::new(31), Dev::new(32), Dev::new(33), Dev::new(34));
    active_device(&server, &sa, &alice, &v1, 1, &d1).await;
    active_device(&server, &sb, &alice, &v1, 1, &d2).await;
    active_device(&server, &sc, &alice, &v1, 1, &d3).await;
    put_device(&server, &sd, &d4, "pending", None).await;
    let path = "/api/v1/users/@me/encryption/rotate";
    let good = rotation(&ctx, &v1, &v2, 2, &[&d3], &[&d1, &d2], &d1).body;

    let mut bad = good.clone();
    bad["version"] = json!(3);
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::CONFLICT
    );
    let mut bad = good.clone();
    bad["devices"] = json!([good["devices"][0]]);
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::CONFLICT
    );
    let mut bad = good.clone();
    bad["packages"] = json!([{"device_id": d1.id, "key_package": key_package()}, {"device_id": d2.id, "key_package": key_package()}]);
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::CONFLICT
    );
    let mut bad = good.clone();
    bad["signature"] = json!(v2.sign(&account::succession_message(&ctx, 2, &v2.identity())));
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    let stale_certs = rotation(&ctx, &v1, &v1, 2, &[&d3], &[&d1, &d2], &d1).body;
    let mut bad = good.clone();
    bad["devices"] = stale_certs["devices"].clone();
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = good.clone();
    bad["revoke"] = json!([d1.id]);
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = good.clone();
    bad["revoke"] = json!([d4.id]);
    assert_eq!(
        call(&server, &sa, Method::POST, path, bad).await.0,
        StatusCode::CONFLICT
    );
    let reused = rotation(&ctx, &v1, &v1, 2, &[&d3], &[&d1, &d2], &d1).body;
    assert_eq!(
        call(&server, &sa, Method::POST, path, reused).await.0,
        StatusCode::BAD_REQUEST
    );
    // Only an active device's session may rotate.
    assert_eq!(
        call(&server, &sd, Method::POST, path, good.clone()).await.0,
        StatusCode::FORBIDDEN
    );

    let mut rx = subscribe(&server);
    let (status, body) = call(&server, &sa, Method::POST, path, good.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        events(&mut rx)[0].event["type"],
        "encryption.devices_update"
    );
    let data = &body["data"];
    assert_eq!(data["account"]["version"], 2);
    assert_eq!(data["account"]["signing_key"], v2.signing_key());
    let successions = data["successions"].as_array().unwrap();
    assert_eq!(successions.len(), 2);
    assert_eq!(successions[0]["version"], 1);
    assert!(successions[0]["signature"].is_null());
    assert_eq!(successions[1]["version"], 2);
    let signature = ed25519_dalek::Signature::from_slice(
        &BASE64
            .decode(successions[1]["signature"].as_str().unwrap().as_bytes())
            .unwrap(),
    )
    .unwrap();
    v1.signing
        .verifying_key()
        .verify(
            &account::succession_message(&ctx, 2, &v2.identity()),
            &signature,
        )
        .unwrap();
    let mut ids: Vec<&str> = data["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["device_id"].as_str().unwrap())
        .collect();
    ids.sort();
    let mut expected = vec![d1.id.as_str(), d2.id.as_str()];
    expected.sort();
    assert_eq!(ids, expected, "revoked and pending devices are gone");
    for row in data["devices"].as_array().unwrap() {
        assert_eq!(row["account_version"], 2);
        assert_eq!(row["status"], "active");
    }
    // Replaying the same rotation is stale.
    assert_eq!(
        call(&server, &sa, Method::POST, path, good).await.0,
        StatusCode::CONFLICT
    );

    // The revoked device's session is gone; its ID is tombstoned.
    assert_eq!(
        call(
            &server,
            &sc,
            Method::GET,
            "/api/v1/users/@me/encryption",
            json!(null)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let fresh = new_session(&server, &alice).await;
    let (status, body) = put_device(&server, &fresh, &d3, "again", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["message"], account::REVOKED_MESSAGE);
    // The pending device was wiped; the others keep their sessions.
    assert_eq!(
        call(
            &server,
            &sd,
            Method::GET,
            &format!("{}/package", device_path(&d4)),
            json!(null)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = call(
        &server,
        &sb,
        Method::GET,
        &format!("{}/package", device_path(&d2)),
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["key_package"], key_package());
    assert_eq!(
        call(
            &server,
            &sa,
            Method::GET,
            &format!("{}/package", device_path(&d1)),
            json!(null)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // v1 registration is idempotent for the current key, conflicting otherwise.
    assert_eq!(
        call(
            &server,
            &sa,
            Method::PUT,
            "/api/v1/users/@me/encryption",
            serde_json::to_value(v1.identity()).unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &server,
            &sa,
            Method::PUT,
            "/api/v1/users/@me/encryption",
            serde_json::to_value(v2.identity()).unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );

    // Discovery: current account key, successions, and certified devices
    // (active + revoked, never pending).
    put_device(&server, &sd, &d4, "pending again", None).await;
    let channel = db::dm_participants::create_dm_channel(
        server.pool(),
        &alice.user.id,
        std::slice::from_ref(&bob.user.id),
        server.state.db_is_postgres,
    )
    .await
    .unwrap();
    let (status, body) = call(
        &server,
        &bob.auth_header(),
        Method::GET,
        &format!("/api/v1/channels/{}/encryption", channel.id),
        json!(null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["data"]["participants"].as_array().unwrap();
    let alice_row = rows
        .iter()
        .find(|r| r["user_id"] == alice.user.id.as_str())
        .unwrap();
    assert_eq!(
        alice_row["identity"],
        serde_json::to_value(v2.identity()).unwrap()
    );
    assert_eq!(alice_row["account_version"], 2);
    assert_eq!(alice_row["successions"], data["successions"]);
    let devices = alice_row["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 3);
    for d in devices {
        let keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys.len(),
            6,
            "device_id, exchange_key, signing_key, certificate, account_version, revoked"
        );
        let revoked = d["device_id"] == d3.id.as_str();
        assert_eq!(d["revoked"], revoked);
        assert_eq!(d["account_version"], if revoked { 1 } else { 2 });
    }
    let bob_row = rows
        .iter()
        .find(|r| r["user_id"] == bob.user.id.as_str())
        .unwrap();
    assert_eq!(bob_row["account_version"], 1);
    let bob_successions = bob_row["successions"].as_array().unwrap();
    assert_eq!(bob_successions.len(), 1);
    assert_eq!(bob_successions[0]["version"], 1);
    assert!(bob_successions[0]["signature"].is_null());
    assert_eq!(bob_row["devices"], json!([]));
}

/// A v2 envelope addressed to `recipients` (`(user_context, key_id, exchange)`).
#[allow(clippy::too_many_arguments)]
fn envelope_v2(
    channel: &str,
    author: &str,
    device_id: &str,
    signer: &Keys,
    recipients: &[(String, String, String)],
    token: &str,
    reply: Option<&str>,
    edit: Option<&str>,
) -> String {
    let p = json!([
        2,
        channel,
        author,
        device_id,
        token,
        reply,
        edit,
        BASE64.encode(&[3; 32]),
        BASE64.encode(&[5; 80]),
        recipients
            .iter()
            .map(|(user, key_id, key)| json!([user, key_id, key, BASE64.encode(&[4; 60])]))
            .collect::<Vec<_>>()
    ]);
    format!(
        "{}{}",
        e2ee::PREFIX_V2,
        json!({"payload": p, "signature": signer.sign(&serde_json::to_vec(&p).unwrap())})
    )
}
fn token() -> String {
    BASE64.encode(uuid::Uuid::new_v4().as_bytes())
}
fn account_entry(user: &TestUser, keys: &Keys) -> (String, String, String) {
    (
        user.user.id.clone(),
        "account".into(),
        keys.exchange.clone(),
    )
}
fn device_entry(user: &TestUser, dev: &Dev) -> (String, String, String) {
    (
        user.user.id.clone(),
        dev.id.clone(),
        dev.keys.exchange.clone(),
    )
}
fn sorted(mut r: Vec<(String, String, String)>) -> Vec<(String, String, String)> {
    r.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
    r
}

#[tokio::test]
async fn v2_envelopes_are_admitted_only_from_active_devices_to_the_exact_set() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let (av1, av2, bv1) = (Keys::new(7), Keys::new(17), Keys::new(8));
    register_account(&server, &alice, &av1).await;
    register_account(&server, &bob, &bv1).await;
    let (sa, sa2, sa3) = (
        alice.auth_header(),
        new_session(&server, &alice).await,
        new_session(&server, &alice).await,
    );
    let (sb, sb2) = (bob.auth_header(), new_session(&server, &bob).await);
    let (da, da2, dap) = (Dev::new(31), Dev::new(32), Dev::new(33));
    let (db1, dbp) = (Dev::new(41), Dev::new(42));
    active_device(&server, &sa, &alice, &av1, 1, &da).await;
    active_device(&server, &sa2, &alice, &av1, 1, &da2).await;
    put_device(&server, &sa3, &dap, "pending", None).await;
    active_device(&server, &sb, &bob, &bv1, 1, &db1).await;
    put_device(&server, &sb2, &dbp, "pending", None).await;
    let channel = db::dm_participants::create_dm_channel(
        server.pool(),
        &alice.user.id,
        std::slice::from_ref(&bob.user.id),
        server.state.db_is_postgres,
    )
    .await
    .unwrap();
    let path = format!("/api/v1/channels/{}/messages", channel.id);
    let (cid, aid) = (channel.id.as_str(), alice.user.id.as_str());
    let set = sorted(vec![
        account_entry(&alice, &av1),
        device_entry(&alice, &da),
        device_entry(&alice, &da2),
        account_entry(&bob, &bv1),
        device_entry(&bob, &db1),
    ]);
    let send = |content: String| {
        let server = &server;
        let path = path.clone();
        let sa = sa.clone();
        async move {
            call(
                server,
                &sa,
                Method::POST,
                &path,
                json!({"content": content}),
            )
            .await
        }
    };

    let shared_token = token();
    let content = envelope_v2(cid, aid, &da.id, &da.keys, &set, &shared_token, None, None);
    let (status, body) = send(content.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["content"], content);
    let message_id = body["data"]["id"].as_str().unwrap().to_owned();
    // Replay of the same token is rejected (v2 token is payload item 4).
    assert_eq!(send(content).await.0, StatusCode::CONFLICT);
    let same_token = envelope_v2(
        cid,
        aid,
        &da2.id,
        &da2.keys,
        &set,
        &shared_token,
        None,
        None,
    );
    assert_eq!(send(same_token).await.0, StatusCode::CONFLICT);
    // A second message from the same device with a new token is fine, which a
    // reservation keyed on the device ID (item 3) would have refused.
    let (status, body) = send(envelope_v2(
        cid,
        aid,
        &da.id,
        &da.keys,
        &set,
        &token(),
        None,
        None,
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Edits carry the edit context.
    let edited = envelope_v2(
        cid,
        aid,
        &da.id,
        &da.keys,
        &set,
        &token(),
        None,
        Some(&message_id),
    );
    let (status, body) = call(
        &server,
        &sa,
        Method::PATCH,
        &format!("{path}/{message_id}"),
        json!({"content": edited}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Wrong recipient sets: missing a device, including a pending device,
    // stale key, unsorted, duplicate.
    let missing: Vec<_> = set.iter().filter(|r| r.1 != db1.id).cloned().collect();
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da.keys,
            &missing,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut with_pending = set.clone();
    with_pending.push(device_entry(&bob, &dbp));
    let with_pending = sorted(with_pending);
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da.keys,
            &with_pending,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut stale = set.clone();
    for r in &mut stale {
        if r.0 == bob.user.id && r.1 == "account" {
            r.2 = Keys::new(99).exchange;
        }
    }
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da.keys,
            &stale,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut unsorted = set.clone();
    unsorted.reverse();
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da.keys,
            &unsorted,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut duplicate = set.clone();
    duplicate.insert(1, set[1].clone());
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da.keys,
            &duplicate,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // Signature checks: the account key is not a device key; another device's
    // key does not match the claimed device; pending/unknown devices are refused.
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &av1,
            &set,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da2.keys,
            &set,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &dap.id,
            &dap.keys,
            &set,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &db1.id,
            &db1.keys,
            &set,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // Bob's device cannot author as Alice; contexts must match.
    assert_eq!(
        call(&server, &sb, Method::POST, &path, json!({"content": envelope_v2(cid, aid, &db1.id, &db1.keys, &set, &token(), None, None)})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(envelope_v2(
            "other",
            aid,
            &da.id,
            &da.keys,
            &set,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // Malformed: v1-shaped payload under the v2 prefix.
    let v1_shaped = format!(
        "{}{}",
        e2ee::PREFIX_V2,
        json!({"payload":[2,cid,aid,token(),null,null,"x","y",[]],"signature":"AA=="})
    );
    assert_eq!(send(v1_shaped).await.0, StatusCode::BAD_REQUEST);

    // v1 envelopes (account-signed) remain admissible.
    let v1_payload = json!([
        1,
        cid,
        aid,
        token(),
        null,
        null,
        BASE64.encode(&[3; 32]),
        BASE64.encode(&[5; 80]),
        [
            [aid, av1.exchange, BASE64.encode(&[4; 60])],
            [bob.user.id, bv1.exchange, BASE64.encode(&[4; 60])]
        ]
    ]);
    let mut v1_payload = v1_payload;
    let mut recipients = v1_payload[8].as_array().unwrap().clone();
    recipients.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    v1_payload[8] = json!(recipients);
    let v1_content = format!(
        "{}{}",
        e2ee::PREFIX,
        json!({"payload": v1_payload, "signature": av1.sign(&serde_json::to_vec(&v1_payload).unwrap())})
    );
    let (status, body) = send(v1_content).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Rotation revoking da2: its messages are refused, and the old set is stale.
    let rotate = rotation(aid, &av1, &av2, 2, &[&da2], &[&da], &da).body;
    let (status, body) = call(
        &server,
        &sa,
        Method::POST,
        "/api/v1/users/@me/encryption/rotate",
        rotate,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let new_set = sorted(vec![
        account_entry(&alice, &av2),
        device_entry(&alice, &da),
        account_entry(&bob, &bv1),
        device_entry(&bob, &db1),
    ]);
    let (status, body) = send(envelope_v2(
        cid,
        aid,
        &da2.id,
        &da2.keys,
        &new_set,
        &token(),
        None,
        None,
    ))
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["message"], account::REVOKED_MESSAGE);
    assert_eq!(
        send(envelope_v2(
            cid,
            aid,
            &da.id,
            &da.keys,
            &set,
            &token(),
            None,
            None
        ))
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, body) = send(envelope_v2(
        cid,
        aid,
        &da.id,
        &da.keys,
        &new_set,
        &token(),
        None,
        None,
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The database writer enforces the same rules (MCP / internal callers).
    let input = accordserver::models::message::CreateMessage {
        content: envelope_v2(cid, aid, &da2.id, &da2.keys, &new_set, &token(), None, None),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
        title: None,
    };
    assert!(
        db::messages::create_message(server.pool(), cid, aid, None, &input, 0)
            .await
            .is_err()
    );

    // v2 envelopes stay out of space channels.
    let space = server.create_space(&alice.user.id, "space").await;
    let public = server.create_channel(&space, "public").await;
    let (status, _) = call(
        &server,
        &sa,
        Method::POST,
        &format!("/api/v1/channels/{public}/messages"),
        json!({"content": envelope_v2(&public, aid, &da.id, &da.keys, &new_set, &token(), None, None)}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Discovery lists Bob's active device but not his pending one.
    let (_, body) = call(
        &server,
        &sa,
        Method::GET,
        &format!("/api/v1/channels/{cid}/encryption"),
        json!(null),
    )
    .await;
    let bob_row = body["data"]["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["user_id"] == bob.user.id.as_str())
        .unwrap()
        .clone();
    assert_eq!(bob_row["devices"].as_array().unwrap().len(), 1);
    assert_eq!(bob_row["devices"][0]["device_id"], db1.id.as_str());
    assert_eq!(bob_row["devices"][0]["revoked"], false);
}

// ---------------------------------------------------------------------------
// Dart interop fixture (added by the integrator). Skipped when absent.
// ---------------------------------------------------------------------------

fn verify_b64(signing_key: &str, message: &[u8], signature: &str) {
    let key: [u8; 32] = BASE64
        .decode(signing_key.as_bytes())
        .unwrap()
        .try_into()
        .unwrap();
    let sig = ed25519_dalek::Signature::from_slice(&BASE64.decode(signature.as_bytes()).unwrap())
        .unwrap();
    VerifyingKey::from_bytes(&key)
        .unwrap()
        .verify_strict(message, &sig)
        .expect("signature verifies");
}

#[tokio::test]
async fn dart_v2_fixture_verifies_when_present() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2ee_v2.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipping: {} not present", path.display());
        return;
    };
    let fixture: Value = serde_json::from_str(&text).unwrap();
    let author = fixture["user_context"].as_str().unwrap();
    let channel = fixture["channel_context"].as_str().unwrap();
    let mut accounts = vec![(
        author.to_owned(),
        fixture["account"].clone(),
        fixture["devices"].clone(),
    )];
    for p in fixture["participants"].as_array().unwrap() {
        let context = p["user_context"].as_str().unwrap();
        if context != author {
            accounts.push((
                context.to_owned(),
                p["account"].clone(),
                p["devices"].clone(),
            ));
        }
    }
    // Every certificate verifies against its account signing key.
    let mut expected = Vec::new();
    for (context, account_keys, devices) in &accounts {
        let signing = account_keys["signing_key"].as_str().unwrap();
        expected.push((
            context.clone(),
            "account".to_owned(),
            account_keys["exchange_key"].as_str().unwrap().to_owned(),
        ));
        for d in devices.as_array().unwrap() {
            let id = d["device_id"].as_str().unwrap();
            assert!(account::valid_device_id(id));
            verify_b64(
                signing,
                &account::certificate_message(
                    context,
                    d["account_version"].as_i64().unwrap(),
                    id,
                    d["exchange_key"].as_str().unwrap(),
                    d["signing_key"].as_str().unwrap(),
                ),
                d["certificate"].as_str().unwrap(),
            );
            expected.push((
                context.clone(),
                id.to_owned(),
                d["exchange_key"].as_str().unwrap().to_owned(),
            ));
        }
    }
    expected.sort_by(|a, b| {
        account::code_unit_cmp(&a.0, &b.0).then_with(|| account::code_unit_cmp(&a.1, &b.1))
    });

    let content = fixture["content"].as_str().unwrap();
    let envelope: Value =
        serde_json::from_str(content.strip_prefix(e2ee::PREFIX_V2).unwrap()).unwrap();
    assert_eq!(envelope.as_object().unwrap().len(), 2);
    let p = envelope["payload"].as_array().unwrap();
    assert_eq!(p.len(), 10);
    assert_eq!(p[0], 2);
    assert_eq!(p[1], channel);
    assert_eq!(p[2], author);
    let sender = fixture["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["device_id"] == p[3])
        .expect("sender is one of the author's devices");
    // Rust re-serialization of the parsed payload must reproduce the signed bytes.
    verify_b64(
        sender["signing_key"].as_str().unwrap(),
        &serde_json::to_vec(&envelope["payload"]).unwrap(),
        envelope["signature"].as_str().unwrap(),
    );
    let recipients: Vec<(String, String, String)> = p[9]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let r = r.as_array().unwrap();
            assert_eq!(r.len(), 4);
            assert_eq!(
                BASE64
                    .decode(r[3].as_str().unwrap().as_bytes())
                    .unwrap()
                    .len(),
                60
            );
            (
                r[0].as_str().unwrap().into(),
                r[1].as_str().unwrap().into(),
                r[2].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(recipients, expected);

    // Full server admission when every account is at version 1 (no
    // successions are carried by the fixture).
    let all_v1 = accounts.iter().all(|(_, _, devices)| {
        devices
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["account_version"] == 1)
    });
    if !all_v1 {
        return;
    }
    let server = TestServer::new().await;
    for (context, account_keys, devices) in &accounts {
        sqlx::query(&db::q(
            "INSERT INTO users(id,username,display_name) VALUES (?,?,?)",
        ))
        .bind(context)
        .bind(context)
        .bind(context)
        .execute(server.pool())
        .await
        .unwrap();
        let row = json!({"identity": account_keys, "account_version": 1, "successions": [], "devices": devices});
        accordserver::federation::e2ee::cache_account(&server.state, context, context, &row)
            .await
            .unwrap();
    }
    sqlx::query(&db::q(
        "INSERT INTO channels(id,name,type) VALUES (?,'','group_dm')",
    ))
    .bind(channel)
    .execute(server.pool())
    .await
    .unwrap();
    for (context, _, _) in &accounts {
        db::dm_participants::add_participant(
            server.pool(),
            channel,
            context,
            server.state.db_is_postgres,
        )
        .await
        .unwrap();
    }
    e2ee::validate_message(server.pool(), channel, author, content, None, None)
        .await
        .unwrap();
}
