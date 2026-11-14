//! Attaching link previews (unfurl) updates a message's embeds; it is not an
//! edit, so `edited_at` must stay unset. Editing the content still marks it.

mod common;

use accordserver::db::messages::{create_message, update_message};
use accordserver::models::message::{CreateMessage, UpdateMessage};
use common::TestServer;
use serde_json::json;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn embeds_do_not_mark_a_message_edited() {
    let server = TestServer::new().await;
    let owner = server.create_user_with_token("unfurl_owner").await;
    let space_id = server.create_space(&owner.user.id, "Unfurl").await;
    let channel_id = server.create_channel(&space_id, "general").await;

    let msg = create_message(
        server.pool(),
        &channel_id,
        &owner.user.id,
        Some(&space_id),
        &CreateMessage {
            content: "look https://example.com".into(),
            tts: None,
            embeds: None,
            reply_to: None,
            thread_id: None,
            title: None,
        },
        0,
    )
    .await
    .unwrap();

    let embed = serde_json::from_value(json!({
        "type": "link",
        "title": "Example Domain",
        "url": "https://example.com"
    }))
    .unwrap();
    let unfurled = update_message(
        server.pool(),
        &msg.id,
        &UpdateMessage {
            content: None,
            embeds: Some(vec![embed]),
            title: None,
        },
        false,
    )
    .await
    .unwrap();
    assert!(
        unfurled.edited_at.is_none(),
        "attaching embeds must not set edited_at"
    );

    let edited = update_message(
        server.pool(),
        &msg.id,
        &UpdateMessage {
            content: Some("look again https://example.com".into()),
            embeds: None,
            title: None,
        },
        false,
    )
    .await
    .unwrap();
    assert!(edited.edited_at.is_some(), "a content edit sets edited_at");
}
