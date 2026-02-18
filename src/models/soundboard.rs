use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundboardSound {
    pub id: String,
    pub name: String,
    pub audio_url: Option<String>,
    pub volume: f64,
    pub creator_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSound {
    pub name: String,
    pub audio: String, // base64 data URI
    pub volume: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSound {
    pub name: Option<String>,
    pub volume: Option<f64>,
}
