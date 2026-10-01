mod common;
use accordserver::{
    db,
    e2ee::{self, Identity},
    models::message::CreateMessage,
};
use common::{authenticated_json_request, parse_body, TestServer, TestUser};
use data_encoding::BASE64;
use ed25519_dalek::{Signer, SigningKey};
use http::{Method, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn register(server: &TestServer, user: &TestUser, seed: u8) -> (Identity, SigningKey) {
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let key = Identity {
        exchange_key: BASE64.encode(&[seed; 32]),
        signing_key: BASE64.encode(signing.verifying_key().as_bytes()),
    };
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::PUT,
            "/api/v1/users/@me/encryption",
            &user.auth_header(),
            &serde_json::to_value(&key).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    (key, signing)
}
fn envelope(
    channel: &str,
    author: &str,
    recipients: &[(String, Identity)],
    signing: &SigningKey,
    reply: Option<&str>,
    edit: Option<&str>,
) -> String {
    let mut recipients = recipients.to_vec();
    recipients.sort_by(|a, b| a.0.cmp(&b.0));
    let p = json!([
        1,
        channel,
        author,
        BASE64.encode(uuid::Uuid::new_v4().as_bytes()),
        reply,
        edit,
        BASE64.encode(&[3; 32]),
        BASE64.encode(&[5; 80]),
        recipients
            .iter()
            .map(|(id, key)| json!([id, key.exchange_key, BASE64.encode(&[4; 60])]))
            .collect::<Vec<_>>()
    ]);
    let signature = signing.sign(&serde_json::to_vec(&p).unwrap());
    format!(
        "{}{}",
        e2ee::PREFIX,
        json!({"payload":p,"signature":BASE64.encode(&signature.to_bytes())})
    )
}
async fn request(
    server: &TestServer,
    user: &TestUser,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            method,
            path,
            &user.auth_header(),
            &body,
        ))
        .await
        .unwrap();
    (response.status(), parse_body(response).await)
}

#[tokio::test]
async fn private_messages_require_encryption_and_keys_are_immutable() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let eve = server.create_user_with_token("eve").await;
    let channel = db::dm_participants::create_dm_channel(
        server.pool(),
        &alice.user.id,
        &[bob.user.id.clone()],
        server.state.db_is_postgres,
    )
    .await
    .unwrap();
    let path = format!("/api/v1/channels/{}/messages", channel.id);
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":"must not be stored"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 0);
    let (alice_key, _) = register(&server, &alice, 7).await;
    assert_eq!(
        request(
            &server,
            &alice,
            Method::PUT,
            "/api/v1/users/@me/encryption",
            serde_json::to_value(&alice_key).unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    let changed = Identity {
        exchange_key: BASE64.encode(&[9; 32]),
        ..alice_key
    };
    assert_eq!(
        request(
            &server,
            &alice,
            Method::PUT,
            "/api/v1/users/@me/encryption",
            serde_json::to_value(changed).unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let key_path = format!("/api/v1/channels/{}/encryption", channel.id);
    assert_eq!(
        request(&server, &eve, Method::GET, &key_path, json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let invalid = Identity {
        exchange_key: BASE64.encode(&[0; 32]),
        signing_key: BASE64.encode(&[0; 32]),
    };
    assert_eq!(
        request(
            &server,
            &bob,
            Method::PUT,
            "/api/v1/users/@me/encryption",
            serde_json::to_value(invalid).unwrap()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn signed_ciphertext_roundtrips_edits_and_replays_are_rejected() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let (a, signing) = register(&server, &alice, 7).await;
    let (b, _) = register(&server, &bob, 8).await;
    let channel = db::dm_participants::create_dm_channel(
        server.pool(),
        &alice.user.id,
        &[bob.user.id.clone()],
        server.state.db_is_postgres,
    )
    .await
    .unwrap();
    let recipients = vec![(alice.user.id.clone(), a), (bob.user.id.clone(), b)];
    let content = envelope(
        &channel.id,
        &alice.user.id,
        &recipients,
        &signing,
        None,
        None,
    );
    let path = format!("/api/v1/channels/{}/messages", channel.id);
    let (status, body) = request(
        &server,
        &alice,
        Method::POST,
        &path,
        json!({"content":content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["content"], content);
    assert_eq!(body["data"]["embeds"], json!([]));
    let id = body["data"]["id"].as_str().unwrap();
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":content})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let edit_path = format!("{path}/{id}");
    let edited = envelope(
        &channel.id,
        &alice.user.id,
        &recipients,
        &signing,
        None,
        Some(id),
    );
    let (status, body) = request(
        &server,
        &alice,
        Method::PATCH,
        &edit_path,
        json!({"content":edited}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["content"], edited);
    assert_eq!(
        request(
            &server,
            &alice,
            Method::PATCH,
            &edit_path,
            json!({"content":"plaintext edit"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &server,
            &alice,
            Method::PATCH,
            &edit_path,
            json!({"content":edited})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &server,
            &bob,
            Method::PATCH,
            &edit_path,
            json!({"content":edited})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = request(&server, &bob, Method::GET, &edit_path, json!(null)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["content"], edited);
}

#[tokio::test]
async fn group_membership_signature_context_and_metadata_are_enforced() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let charlie = server.create_user_with_token("charlie").await;
    let (a, signing) = register(&server, &alice, 7).await;
    let (b, _) = register(&server, &bob, 8).await;
    let (c, _) = register(&server, &charlie, 9).await;
    let channel = db::dm_participants::create_dm_channel(
        server.pool(),
        &alice.user.id,
        &[bob.user.id.clone(), charlie.user.id.clone()],
        server.state.db_is_postgres,
    )
    .await
    .unwrap();
    let participants = vec![
        (alice.user.id.clone(), a),
        (bob.user.id.clone(), b),
        (charlie.user.id.clone(), c),
    ];
    let path = format!("/api/v1/channels/{}/messages", channel.id);
    let content = envelope(
        &channel.id,
        &alice.user.id,
        &participants,
        &signing,
        None,
        None,
    );
    let mut bad: Value = serde_json::from_str(content.strip_prefix(e2ee::PREFIX).unwrap()).unwrap();
    bad["payload"][7] = json!(BASE64.encode(&[77; 80]));
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":format!("{}{}",e2ee::PREFIX,bad)})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let moved = envelope(
        "different-chat",
        &alice.user.id,
        &participants,
        &signing,
        None,
        None,
    );
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":moved})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &server,
            &bob,
            Method::POST,
            &path,
            json!({"content":content})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":content,"tts":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":content,"embeds":[{"title":"leak"}]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    db::dm_participants::remove_participant(server.pool(), &channel.id, &bob.user.id)
        .await
        .unwrap();
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":content})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let current: Vec<_> = participants
        .into_iter()
        .filter(|(id, _)| id != &bob.user.id)
        .collect();
    let fresh = envelope(&channel.id, &alice.user.id, &current, &signing, None, None);
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":fresh})
        )
        .await
        .0,
        StatusCode::OK
    );
    let input = CreateMessage {
        content: "direct writer plaintext".into(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
        title: None,
    };
    assert!(db::messages::create_message(
        server.pool(),
        &channel.id,
        &alice.user.id,
        None,
        &input,
        0
    )
    .await
    .is_err());
}

#[tokio::test]
async fn space_channels_stay_plaintext_and_reject_encryption_envelopes() {
    let server = TestServer::new().await;
    let alice = server.create_user_with_token("alice").await;
    let space = server.create_space(&alice.user.id, "space").await;
    let channel = server.create_channel(&space, "public").await;
    let path = format!("/api/v1/channels/{channel}/messages");
    let (status, body) = request(
        &server,
        &alice,
        Method::POST,
        &path,
        json!({"content":"normal channel message"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["content"], "normal channel message");
    assert_eq!(
        request(
            &server,
            &alice,
            Method::POST,
            &path,
            json!({"content":"daccord-e2ee:1:{}"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn dart_ciphertext_vector_verifies_and_is_stored_unchanged() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/e2ee_v1.json")).unwrap();
    let server = TestServer::new().await;
    for id in ["alice", "bob"] {
        sqlx::query("INSERT INTO users(id,username,display_name) VALUES ($1,$2,$3)")
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(server.pool())
            .await
            .unwrap();
        let key: Identity = serde_json::from_value(fixture["identities"][id].clone()).unwrap();
        accordserver::federation::e2ee::cache_identity(&server.state, id, id, &key)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO channels(id,name,type) VALUES ('chat','','dm')")
        .execute(server.pool())
        .await
        .unwrap();
    for id in ["alice", "bob"] {
        db::dm_participants::add_participant(
            server.pool(),
            "chat",
            id,
            server.state.db_is_postgres,
        )
        .await
        .unwrap();
    }
    let content = fixture["content"].as_str().unwrap();
    let envelope: Value =
        serde_json::from_str(content.strip_prefix(e2ee::PREFIX).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_string(&envelope["payload"]).unwrap(),
        fixture["canonical_payload"].as_str().unwrap()
    );
    e2ee::validate_message(server.pool(), "chat", "alice", content, None, None)
        .await
        .unwrap();
    let input = CreateMessage {
        content: content.into(),
        tts: None,
        embeds: None,
        reply_to: None,
        thread_id: None,
        title: None,
    };
    let message = db::messages::create_message(server.pool(), "chat", "alice", None, &input, 0)
        .await
        .unwrap();
    assert_eq!(message.content, content);
    assert!(!message
        .content
        .contains(fixture["expected_content"].as_str().unwrap()));
}
