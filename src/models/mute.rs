use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ChannelMute {
    pub user_id: String,
    pub channel_id: String,
    pub created_at: String,
}
