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
    state.federated_presence.changed.notify_one();
    prev
}

/// Remove a user's presence. Returns the old presence if any.
pub fn remove_presence(state: &AppState, user_id: &str) -> Option<Presence> {
    let old = state.presences.remove(user_id).map(|(_, p)| p);
    state.federated_presence.changed.notify_one();
    old
}

/// Get a single user's current presence.
pub fn get_user_presence(state: &AppState, user_id: &str) -> Option<Presence> {
    state.presences.get(user_id).map(|p| p.clone())
}

/// Get presences for all online members of a space.
/// Requires the set of member user IDs for that space.
pub fn get_space_presences(state: &AppState, member_ids: &HashSet<String>) -> Vec<Presence> {
    member_ids
        .iter()
        .filter_map(|id| current_presence(state, id))
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

/// A remote user's expiring presence, or an authoritative local presence.
pub fn current_presence(state: &AppState, user_id: &str) -> Option<Presence> {
    let local_id = match state.federation.as_ref() {
        Some(fed)
            if crate::federation::mapping::domain_of(user_id)
                .is_some_and(|d| d.eq_ignore_ascii_case(&fed.domain)) =>
        {
            crate::federation::mapping::local_part(user_id)
        }
        _ => user_id,
    };
    if let Some(p) = state.presences.get(local_id) {
        let mut p = p.clone();
        p.user_id = user_id.to_string();
        return Some(p);
    }
    let remote = state.federated_presence.remote.get(user_id)?;
    let mut p = remote.presence.clone();
    if remote.expires_at <= chrono::Utc::now().timestamp_millis() {
        p.status = "offline".into();
    }
    Some(p)
}

pub fn offline_presence(user_id: &str) -> Presence {
    Presence {
        user_id: user_id.into(),
        status: "offline".into(),
        client_status: ClientStatus {
            desktop: None,
            mobile: None,
            web: None,
        },
        activities: vec![],
        space_id: None,
    }
}

/// Invisible and offline disclose neither client status nor custom activities.
/// Only the local user's own private snapshot retains their Invisible choice.
pub fn visible_presence(mut p: Presence, viewer: Option<&str>) -> Presence {
    if p.status == "invisible" && viewer == Some(p.user_id.as_str()) {
        return p;
    }
    if !matches!(p.status.as_str(), "online" | "idle" | "dnd") {
        p.status = "offline".into();
        p.client_status = ClientStatus {
            desktop: None,
            mobile: None,
            web: None,
        };
        p.activities.clear();
    }
    p
}

/// Complete, member-scoped snapshot. Missing entries explicitly mean offline.
pub async fn snapshot(
    state: &AppState,
    space_id: &str,
    viewer: &str,
) -> Result<Vec<Presence>, crate::error::AppError> {
    let ids = crate::db::spaces::list_member_ids_for_space(&state.db, space_id).await?;
    Ok(ids
        .iter()
        .map(|id| {
            visible_presence(
                current_presence(state, id).unwrap_or_else(|| offline_presence(id)),
                Some(viewer),
            )
        })
        .collect())
}
