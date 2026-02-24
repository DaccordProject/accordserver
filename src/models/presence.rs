use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    pub user_id: String,
    pub status: String,
    pub client_status: ClientStatus,
    pub activities: Vec<serde_json::Value>,
    pub space_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientStatus {
    pub desktop: Option<String>,
    pub mobile: Option<String>,
    pub web: Option<String>,
}
