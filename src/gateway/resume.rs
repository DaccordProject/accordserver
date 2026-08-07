use std::collections::VecDeque;
use tokio::sync::{broadcast, mpsc, oneshot};

use super::events::GatewayBroadcast;

/// How long a dropped session stays resumable. This is also how long the user
/// keeps their presence after the socket goes away, so a transient drop no
/// longer flaps everyone's member list to offline and back.
pub const RESUME_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Events retained per session for replay. Bounded because a parked session
/// holds its buffer for the whole resume window; overflowing it means the
/// client is told to re-IDENTIFY rather than handed an incomplete replay.
pub const REPLAY_BUFFER_LIMIT: usize = 256;

/// Everything a resuming socket needs to carry on exactly where the dropped one
/// stopped. The broadcast receiver is handed over rather than re-subscribed so
/// no event falls between the two connections or arrives on both.
pub struct Handover {
    /// Sequence number of the last event assigned to this session.
    pub seq: u64,
    /// Recently dispatched events as `(seq, payload)`, oldest first.
    pub buffer: VecDeque<(u64, String)>,
    pub broadcast_rx: Option<broadcast::Receiver<GatewayBroadcast>>,
}

/// Registry entry for a session whose socket dropped. The task that owned the
/// socket stays alive behind it, still collecting events, until a RESUME claims
/// it through `claim_tx` or `RESUME_WINDOW` elapses.
pub struct ParkedSession {
    pub user_id: String,
    pub intents: Vec<String>,
    pub claim_tx: mpsc::Sender<oneshot::Sender<Handover>>,
}

/// Append a dispatched event, evicting the oldest once the buffer is full.
pub fn push(buffer: &mut VecDeque<(u64, String)>, seq: u64, payload: String) {
    buffer.push_back((seq, payload));
    while buffer.len() > REPLAY_BUFFER_LIMIT {
        buffer.pop_front();
    }
}

/// The events dispatched after `after_seq`, or `None` when the buffer no longer
/// covers the gap and the client has to start over with IDENTIFY. Returning
/// `None` rather than a partial replay is the point: a client told it resumed
/// stops backfilling.
pub fn missed_since(
    buffer: &VecDeque<(u64, String)>,
    current_seq: u64,
    after_seq: u64,
) -> Option<Vec<String>> {
    if after_seq > current_seq {
        // The client claims to have seen events we never sent.
        return None;
    }
    if after_seq == current_seq {
        return Some(Vec::new());
    }
    match buffer.front() {
        Some((oldest, _)) if *oldest <= after_seq + 1 => Some(
            buffer
                .iter()
                .filter(|(seq, _)| *seq > after_seq)
                .map(|(_, payload)| payload.clone())
                .collect(),
        ),
        // Either the buffer is empty or it has already evicted what the client
        // is asking for.
        _ => None,
    }
}
