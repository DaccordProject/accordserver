use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub code: String,
    pub space_id: String,
    pub channel_id: Option<String>,
    pub inviter_id: Option<String>,
    pub max_uses: Option<i64>,
    pub uses: i64,
    pub max_age: Option<i64>,
    pub temporary: bool,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInvite {
    pub max_uses: Option<i64>,
    pub max_age: Option<i64>,
    pub temporary: Option<bool>,
}
