use serde::{Deserialize, Serialize};

use crate::storage;

#[derive(Debug, Clone, Serialize)]
pub struct ServerSettings {
    pub max_emoji_size: i64,
    pub max_avatar_size: i64,
    pub max_sound_size: i64,
    pub max_attachment_size: i64,
    pub max_attachments_per_message: i64,
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
}
