use serde::{Deserialize, Serialize};

use super::permission::PermissionOverwrite;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub space_id: Option<String>,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub position: Option<i64>,
    pub parent_id: Option<String>,
    pub nsfw: bool,
    pub rate_limit: Option<i64>,
    pub bitrate: Option<i64>,
    pub user_limit: Option<i64>,
    pub owner_id: Option<String>,
    pub last_message_id: Option<String>,
    pub permission_overwrites: Vec<PermissionOverwrite>,
    pub archived: Option<bool>,
    pub auto_archive_after: Option<i64>,
    pub created_at: String,
}

/// Row from the DB before loading permission overwrites.
#[derive(Debug, Clone)]
pub struct ChannelRow {
    pub id: String,
    pub channel_type: String,
    pub space_id: Option<String>,
    pub name: Option<String>,
    pub description: String,
    pub topic: Option<String>,
    pub position: i64,
    pub parent_id: Option<String>,
    pub nsfw: bool,
    pub rate_limit: i64,
    pub bitrate: Option<i64>,
    pub user_limit: Option<i64>,
    pub owner_id: Option<String>,
    pub last_message_id: Option<String>,
    pub archived: bool,
    pub auto_archive_after: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateChannel {
    pub name: String,
    #[serde(rename = "type", default = "default_channel_type")]
    pub channel_type: String,
    pub topic: Option<String>,
    pub parent_id: Option<String>,
    pub nsfw: Option<bool>,
    pub bitrate: Option<i64>,
    pub user_limit: Option<i64>,
    pub rate_limit: Option<i64>,
    pub position: Option<i64>,
}

fn default_channel_type() -> String {
    "text".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateChannel {
    pub name: Option<String>,
    pub topic: Option<String>,
    pub position: Option<i64>,
    pub parent_id: Option<String>,
    pub nsfw: Option<bool>,
    pub rate_limit: Option<i64>,
    pub bitrate: Option<i64>,
    pub user_limit: Option<i64>,
    pub archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelPositionUpdate {
    pub id: String,
    pub position: i64,
}
