//! Qualified-ID helpers and the federation event envelope.
//!
//! Every federated entity is keyed by a qualified ID of the form
//! `"<snowflake>@<domain>"`, where `<domain>` is the entity's **home server**.
//! Local entities keep bare snowflakes in the database (`origin IS NULL`) and
//! are only qualified at the federation boundary. All ID translation routes
//! through this module so the rules live in exactly one place.

use serde::{Deserialize, Serialize};

/// Qualify a bare local ID with our domain. Already-qualified IDs (containing
/// `@`) are returned unchanged so this is idempotent.
pub fn qualify(id: &str, domain: &str) -> String {
    if id.contains('@') {
        id.to_string()
    } else {
        format!("{id}@{domain}")
    }
}

/// The local snowflake part of an ID, dropping any `@domain` suffix.
pub fn local_part(id: &str) -> &str {
    match id.split_once('@') {
        Some((local, _)) => local,
        None => id,
    }
}

/// The home domain encoded in a qualified ID, or `None` for a bare local ID.
pub fn domain_of(id: &str) -> Option<&str> {
    id.split_once('@').map(|(_, domain)| domain)
}

/// The stored `username` for a remote user: the fully-qualified handle
/// (`alice@b.example`). Idempotent if `username` is already qualified.
pub fn handle(username: &str, domain: &str) -> String {
    if username.contains('@') {
        username.to_string()
    } else {
        format!("{username}@{domain}")
    }
}

/// True when an ID belongs to `our_domain` (bare IDs are always local).
pub fn is_local(id: &str, our_domain: &str) -> bool {
    match domain_of(id) {
        None => true,
        Some(d) => d.eq_ignore_ascii_case(our_domain),
    }
}

/// A signed unit of federated state, exchanged via a peer's inbox.
///
/// `origin` is the home domain that is authoritative for the event; the inbox
/// enforces that it matches the signing peer (see [`crate::federation::authority`]).
/// `event_id` is the home server's snowflake for the event and is used for
/// at-least-once deduplication keyed by `(event_id, origin)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationEnvelope {
    pub event_id: String,
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub payload: serde_json::Value,
}

impl FederationEnvelope {
    pub fn new(
        event_id: impl Into<String>,
        origin: impl Into<String>,
        space_id: Option<String>,
        event_type: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            origin: origin.into(),
            space_id,
            event_type: event_type.into(),
            payload,
        }
    }
}

/// A reference to a federated user, exchanged in join snapshots, member/actor
/// payloads, and as a message author. `id` is the user's qualified ID;
/// `username` is its qualified handle, optional because message-author payloads
/// may carry only an id. The other fields cache the user's profile.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteUserRef {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

impl RemoteUserRef {
    /// Seed for this user's stored handle: the username when present, else the
    /// id — which, for a remote user, is already a qualified `name@domain` and
    /// so is itself a valid handle.
    pub fn username_or_id(&self) -> &str {
        self.username.as_deref().unwrap_or(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualify_is_idempotent() {
        assert_eq!(qualify("123", "a.example"), "123@a.example");
        assert_eq!(qualify("123@b.example", "a.example"), "123@b.example");
    }

    #[test]
    fn local_part_and_domain() {
        assert_eq!(local_part("123@b.example"), "123");
        assert_eq!(local_part("123"), "123");
        assert_eq!(domain_of("123@b.example"), Some("b.example"));
        assert_eq!(domain_of("123"), None);
    }

    #[test]
    fn is_local_rules() {
        assert!(is_local("123", "a.example"));
        assert!(is_local("123@a.example", "a.example"));
        assert!(is_local("123@A.EXAMPLE", "a.example"));
        assert!(!is_local("123@b.example", "a.example"));
    }
}
