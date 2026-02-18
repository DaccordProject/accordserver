use serde::{Deserialize, Serialize};

use super::message::Message;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    pub application_id: String,
    #[serde(rename = "type")]
    pub interaction_type: String,
    pub data: Option<InteractionData>,
    pub space_id: Option<String>,
    pub channel_id: Option<String>,
    pub member_id: Option<String>,
    pub user_id: Option<String>,
    pub token: String,
    pub message: Option<Message>,
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionData {
    pub id: String,
    pub name: String,
    pub options: Option<Vec<CommandOptionValue>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOptionValue {
    pub name: String,
    pub value: Option<serde_json::Value>,
    pub options: Option<Vec<CommandOptionValue>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub application_id: String,
    pub space_id: Option<String>,
    pub name: String,
    pub description: String,
    pub options: Option<Vec<CommandOption>>,
    #[serde(rename = "type")]
    pub command_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOption {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub option_type: String,
    pub required: Option<bool>,
    pub choices: Option<Vec<CommandChoice>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandChoice {
    pub name: String,
    pub value: serde_json::Value,
}
