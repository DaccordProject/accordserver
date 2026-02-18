use std::collections::HashSet;
use tokio::sync::mpsc;

/// Represents an authenticated gateway session.
#[derive(Debug)]
pub struct GatewaySession {
    pub session_id: String,
    pub user_id: String,
    pub intents: Vec<String>,
    pub space_ids: HashSet<String>,
    pub sequence: u64,
    pub tx: mpsc::UnboundedSender<String>,
}
