use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub description: String,
    pub bot_public: bool,
    pub owner_id: String,
    pub flags: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateApplication {
    pub name: String,
    pub description: Option<String>,
}
