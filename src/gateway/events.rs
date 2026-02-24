use serde::{Deserialize, Serialize};

/// Broadcast message sent through the gateway channel.
#[derive(Debug, Clone)]
pub struct GatewayBroadcast {
    pub space_id: Option<String>,
    /// When set, only sessions belonging to these user IDs receive the event.
    /// Used for DM/group_dm channel events that have no space_id.
    pub target_user_ids: Option<Vec<String>>,
    pub event: serde_json::Value,
    pub intent: String,
}

/// Opcodes for gateway messages.
pub mod opcode {
    pub const EVENT: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const RESUME: u8 = 3;
    pub const HEARTBEAT_ACK: u8 = 4;
    pub const HELLO: u8 = 5;
    pub const RECONNECT: u8 = 6;
    pub const INVALID_SESSION: u8 = 7;
    pub const PRESENCE_UPDATE: u8 = 8;
    pub const VOICE_STATE_UPDATE: u8 = 9;
    pub const REQUEST_MEMBERS: u8 = 10;
}

/// Close codes.
pub mod close_code {
    pub const UNKNOWN_ERROR: u16 = 4000;
    pub const UNKNOWN_OPCODE: u16 = 4001;
    pub const DECODE_ERROR: u16 = 4002;
    pub const NOT_AUTHENTICATED: u16 = 4003;
    pub const AUTH_FAILED: u16 = 4004;
    pub const ALREADY_AUTHENTICATED: u16 = 4005;
    pub const INVALID_SEQ: u16 = 4007;
    pub const RATE_LIMITED: u16 = 4008;
    pub const SESSION_TIMED_OUT: u16 = 4009;
    pub const INVALID_VERSION: u16 = 4012;
    pub const INVALID_INTENT: u16 = 4013;
    pub const DISALLOWED_INTENT: u16 = 4014;
}

/// Gateway message envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct GatewayMessage {
    pub op: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// IDENTIFY payload data.
#[derive(Debug, Deserialize)]
pub struct IdentifyData {
    pub token: String,
    pub intents: Vec<String>,
    pub properties: Option<serde_json::Value>,
    pub presence: Option<serde_json::Value>,
}

/// VOICE_STATE_UPDATE (opcode 9) payload data.
#[derive(Debug, Deserialize)]
pub struct VoiceStateUpdateData {
    pub space_id: String,
    pub channel_id: Option<String>,
    pub self_mute: Option<bool>,
    pub self_deaf: Option<bool>,
}
