use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub user_id: String,
    pub space_id: String,
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub roles: Vec<String>,
    pub joined_at: String,
    pub premium_since: Option<String>,
    pub deaf: bool,
    pub mute: bool,
    pub pending: Option<bool>,
    pub timed_out_until: Option<String>,
    pub permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct MemberRow {
    pub user_id: String,
    pub space_id: String,
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub joined_at: String,
    pub premium_since: Option<String>,
    pub deaf: bool,
    pub mute: bool,
    pub pending: bool,
    pub timed_out_until: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMember {
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub roles: Option<Vec<String>>,
    pub mute: Option<bool>,
    pub deaf: Option<bool>,
}
