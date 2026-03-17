use serde::{Deserialize, Serialize};

use crate::storage;

#[derive(Debug, Clone, Serialize)]
pub struct ServerSettings {
    pub max_emoji_size: i64,
    pub max_avatar_size: i64,
    pub max_sound_size: i64,
    pub max_attachment_size: i64,
    pub max_attachments_per_message: i64,
    pub server_name: String,
    pub registration_policy: String,
    pub max_spaces: i64,
    pub max_members_per_space: i64,
    pub motd: Option<String>,
    pub public_listing: bool,
    pub tos_enabled: bool,
    pub tos_text: Option<String>,
    pub tos_version: i64,
    pub tos_url: Option<String>,
    pub updated_at: Option<String>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            max_emoji_size: storage::MAX_EMOJI_SIZE as i64,
            max_avatar_size: storage::MAX_AVATAR_SIZE as i64,
            max_sound_size: storage::MAX_SOUND_SIZE as i64,
            max_attachment_size: storage::MAX_ATTACHMENT_SIZE as i64,
            max_attachments_per_message: 10,
            server_name: "Accord Server".to_string(),
            registration_policy: "open".to_string(),
            max_spaces: 0,
            max_members_per_space: 0,
            motd: None,
            public_listing: false,
            tos_enabled: true,
            tos_text: None,
            tos_version: 1,
            tos_url: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateServerSettings {
    pub max_emoji_size: Option<i64>,
    pub max_avatar_size: Option<i64>,
    pub max_sound_size: Option<i64>,
    pub max_attachment_size: Option<i64>,
    pub max_attachments_per_message: Option<i64>,
    pub server_name: Option<String>,
    pub registration_policy: Option<String>,
    pub max_spaces: Option<i64>,
    pub max_members_per_space: Option<i64>,
    pub motd: Option<String>,
    pub public_listing: Option<bool>,
    pub tos_enabled: Option<bool>,
    pub tos_text: Option<String>,
    pub tos_version: Option<i64>,
    pub tos_url: Option<String>,
}
