use serde::{Deserialize, Serialize};

use super::attachment::Attachment;
use super::embed::Embed;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub channel_id: String,
    pub space_id: Option<String>,
    pub author_id: String,
    pub content: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub timestamp: String,
    pub edited_at: Option<String>,
    pub tts: bool,
    pub pinned: bool,
    pub mention_everyone: bool,
    pub mentions: Vec<String>,
    pub mention_roles: Vec<String>,
    pub attachments: Vec<Attachment>,
    pub embeds: Vec<Embed>,
    pub reactions: Option<Vec<ReactionInfo>>,
    pub reply_to: Option<String>,
    pub flags: i64,
    pub webhook_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionInfo {
    pub emoji: ReactionEmoji,
    pub count: i64,
    pub includes_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionEmoji {
    pub id: Option<String>,
    pub name: String,
}

/// Row from the DB before loading relations.
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: String,
    pub channel_id: String,
    pub space_id: Option<String>,
    pub author_id: String,
    pub content: String,
    pub message_type: String,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub tts: bool,
    pub pinned: bool,
    pub mention_everyone: bool,
    pub mentions: String,
    pub mention_roles: String,
    pub embeds: String,
    pub reply_to: Option<String>,
    pub flags: i64,
    pub webhook_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMessage {
    pub content: String,
    pub tts: Option<bool>,
    pub embeds: Option<Vec<Embed>>,
    pub reply_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMessage {
    pub content: Option<String>,
    pub embeds: Option<Vec<Embed>>,
}

#[derive(Debug, Deserialize)]
pub struct BulkDeleteMessages {
    pub messages: Vec<String>,
}
