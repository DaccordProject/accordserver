use serde::{Deserialize, Serialize};

/// Plugin manifest as stored in `manifest_json` and returned to clients.
/// This matches the `plugin.json` schema from the plugin bundle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default = "default_plugin_type")]
    pub plugin_type: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, alias = "entry")]
    pub entry_point: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub max_participants: i64,
    #[serde(default)]
    pub max_spectators: i64,
    #[serde(default)]
    pub max_file_size: i64,
    #[serde(default)]
    pub lobby: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub data_topics: Vec<String>,
    #[serde(default)]
    pub bundle_hash: String,
    #[serde(default)]
    pub canvas_size: Option<[i64; 2]>,
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub signature: String,
}

fn default_plugin_type() -> String {
    "activity".to_string()
}

/// Plugin as returned by the API (no BLOBs).
/// Manifest fields are flattened to the top level for client compatibility.
#[derive(Debug, Clone, Serialize)]
pub struct Plugin {
    pub id: String,
    pub space_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub plugin_type: String,
    pub runtime: String,
    pub description: String,
    pub version: String,
    pub bundle_hash: String,
    pub signed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    pub has_bundle: bool,
    pub creator_id: String,
    pub created_at: String,
    pub updated_at: String,
    // Manifest fields flattened for client compatibility
    pub entry_point: String,
    pub max_participants: i64,
    pub max_spectators: i64,
    pub max_file_size: i64,
    pub lobby: bool,
    pub permissions: Vec<String>,
    pub data_topics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canvas_size: Option<[i64; 2]>,
    pub signature: String,
    /// Internal manifest — not serialized to clients.
    #[serde(skip)]
    pub manifest: PluginManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSession {
    pub id: String,
    pub plugin_id: String,
    pub channel_id: String,
    pub host_user_id: String,
    pub state: String,
    pub participants: Vec<PluginSessionParticipant>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSessionParticipant {
    pub user_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_index: Option<i64>,
    pub joined_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSession {
    pub channel_id: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSessionState {
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct AssignRole {
    pub user_id: String,
    pub role: String,
    #[serde(default)]
    pub slot_index: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PluginAction {
    #[serde(flatten)]
    pub data: serde_json::Value,
}
