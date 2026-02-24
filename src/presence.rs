use std::collections::HashSet;

use crate::models::presence::{ClientStatus, Presence};
use crate::state::AppState;

/// Set a user's presence. Returns the previous presence if any.
pub fn set_presence(
    state: &AppState,
    user_id: &str,
    status: &str,
    activities: Vec<serde_json::Value>,
) -> Option<Presence> {
    let prev = state.presences.get(user_id).map(|p| p.clone());
    let presence = Presence {
        user_id: user_id.to_string(),
        status: status.to_string(),
        client_status: ClientStatus {
            desktop: Some(status.to_string()),
            mobile: None,
            web: None,
        },
        activities,
        space_id: None,
    };
    state.presences.insert(user_id.to_string(), presence);
    prev
}

/// Remove a user's presence. Returns the old presence if any.
pub fn remove_presence(state: &AppState, user_id: &str) -> Option<Presence> {
    state.presences.remove(user_id).map(|(_, p)| p)
}

/// Get a single user's current presence.
pub fn get_user_presence(state: &AppState, user_id: &str) -> Option<Presence> {
    state.presences.get(user_id).map(|p| p.clone())
}

/// Get presences for all online members of a space.
/// Requires the set of member user IDs for that space.
pub fn get_space_presences(state: &AppState, member_ids: &HashSet<String>) -> Vec<Presence> {
    state
        .presences
        .iter()
        .filter(|entry| member_ids.contains(entry.key()))
        .map(|entry| entry.value().clone())
        .collect()
}

/// Check if a user has any other active gateway sessions.
pub async fn user_has_other_sessions(
    state: &AppState,
    user_id: &str,
    exclude_session_id: &str,
) -> bool {
    if let Some(ref dispatcher) = *state.dispatcher.read().await {
        for entry in dispatcher.sessions().iter() {
            let session = entry.value();
            if session.user_id == user_id && session.session_id != exclude_session_id {
                return true;
            }
        }
    }
    false
}
