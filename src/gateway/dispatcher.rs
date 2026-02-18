use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

use super::events::GatewayBroadcast;
use super::session::GatewaySession;

/// Manages all active gateway sessions and broadcasts events.
pub struct Dispatcher {
    sessions: Arc<DashMap<String, GatewaySession>>,
    tx: broadcast::Sender<GatewayBroadcast>,
}

impl Dispatcher {
    pub fn new() -> (Self, broadcast::Sender<GatewayBroadcast>) {
        let (tx, _) = broadcast::channel(1024);
        let sender = tx.clone();
        (
            Self {
                sessions: Arc::new(DashMap::new()),
                tx,
            },
            sender,
        )
    }

    pub fn sessions(&self) -> &Arc<DashMap<String, GatewaySession>> {
        &self.sessions
    }

    pub fn register_session(&self, session: GatewaySession) {
        self.sessions.insert(session.session_id.clone(), session);
    }

    pub fn remove_session(&self, session_id: &str) {
        self.sessions.remove(session_id);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GatewayBroadcast> {
        self.tx.subscribe()
    }

    pub fn broadcast(&self, msg: GatewayBroadcast) {
        let _ = self.tx.send(msg);
    }
}
