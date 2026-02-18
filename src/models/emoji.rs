use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Emoji {
    pub id: Option<String>,
    pub name: String,
    pub animated: bool,
    pub managed: bool,
    pub available: bool,
    pub require_colons: bool,
    pub role_ids: Vec<String>,
    pub creator_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEmoji {
    pub name: String,
    pub image: String, // base64 data URI
}

#[derive(Debug, Deserialize)]
pub struct UpdateEmoji {
    pub name: Option<String>,
}
