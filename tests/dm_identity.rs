//! Equivalent recipient spellings must reopen the same 1:1 conversation.
mod common;

use common::{authenticated_json_request, parse_body, TestServer};
use http::{Method, StatusCode};
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn local_dm_recipient_spellings_reuse_channel() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let alice = server.create_user_with_token("alice").await;
    let bob = server.create_user_with_token("bob").await;
    let mut channel_id = None;
    for recipient in [
        bob.user.id.clone(),
        format!("{}@a.test", bob.user.id),
        format!("  {}@A.TEST  ", bob.user.id),
    ] {
        // Both request forms must use the same normalized identity.
        for body in [
            json!({"recipients": [recipient]}),
            json!({"recipient_id": recipient}),
        ] {
            let response = server
                .router()
                .oneshot(authenticated_json_request(
                    Method::POST,
                    "/api/v1/users/@me/channels",
                    &alice.auth_header(),
                    &body,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = parse_body(response).await;
            let id = body["data"]["id"].as_str().unwrap().to_string();
            if let Some(expected) = &channel_id {
                assert_eq!(&id, expected);
            }
            channel_id = Some(id);
        }
    }
    let participants = accordserver::db::dm_participants::list_participant_ids(
        server.pool(),
        channel_id.as_ref().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(participants.len(), 2);
    assert!(participants.contains(&alice.user.id));
    assert!(participants.contains(&bob.user.id));
}

#[tokio::test]
async fn qualified_self_is_rejected_after_normalization() {
    let mut server = TestServer::new().await;
    server.enable_federation("a.test");
    let alice = server.create_user_with_token("alice").await;
    let response = server
        .router()
        .oneshot(authenticated_json_request(
            Method::POST,
            "/api/v1/users/@me/channels",
            &alice.auth_header(),
            &json!({"recipient_id": format!(" {}@A.TEST ", alice.user.id)}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn canonical_participants_keep_other_domains_and_local_part_case() {
    use accordserver::federation::mapping::participant_storage_id;
    assert_eq!(
        participant_storage_id(" User@B.TEST ", Some("a.test")),
        "User@b.test"
    );
    assert_eq!(
        participant_storage_id(" User@A.TEST ", Some("a.test")),
        "User"
    );
    assert_eq!(participant_storage_id(" User@A.TEST ", None), "User@a.test");
}
