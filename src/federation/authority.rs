//! The single chokepoint that enforces federation authority invariants before
//! any inbound event is applied (S1 + S2 in the plan).
//!
//! The dominant federation bug class is *authority confusion*: a peer signing
//! an event that claims to originate from a different server's user or space.
//! Every inbound event must satisfy: **the authenticated signing peer is the
//! home server of the resource being mutated.**

use crate::error::AppError;
use crate::federation::mapping;

/// Verify that `peer_domain` (the authenticated signer) is allowed to author
/// `envelope`. Returns `Forbidden` on any authority violation.
pub fn check(peer_domain: &str, envelope: &mapping::FederationEnvelope) -> Result<(), AppError> {
    // The event's claimed origin must be the signing peer.
    if !envelope.origin.eq_ignore_ascii_case(peer_domain) {
        return Err(AppError::Forbidden(format!(
            "event origin `{}` does not match signing peer `{peer_domain}`",
            envelope.origin
        )));
    }

    // Any space the event targets must be homed on the signing peer. A peer is
    // never authoritative for another server's space.
    if let Some(space_id) = &envelope.space_id {
        require_homed_on(space_id, peer_domain, "space")?;
    }

    Ok(())
}

/// Assert that a qualified ID is homed on `peer_domain`. Used by event
/// appliers to bind, e.g., a message's author to the signing peer.
pub fn require_homed_on(qualified_id: &str, peer_domain: &str, kind: &str) -> Result<(), AppError> {
    match mapping::domain_of(qualified_id) {
        Some(d) if d.eq_ignore_ascii_case(peer_domain) => Ok(()),
        _ => Err(AppError::Forbidden(format!(
            "{kind} `{qualified_id}` is not homed on signing peer `{peer_domain}`"
        ))),
    }
}

/// Reject any federation write that targets a *local* (`origin IS NULL`) entity.
/// Federation input must never create or overwrite local rows (S2). A qualified
/// ID always carries a domain, so a bare/unqualified ID here means the event is
/// trying to touch a local entity.
pub fn require_remote_target(qualified_id: &str) -> Result<(), AppError> {
    if mapping::domain_of(qualified_id).is_none() {
        return Err(AppError::Forbidden(
            "federation event may not target a local entity".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(origin: &str, space: Option<&str>) -> mapping::FederationEnvelope {
        mapping::FederationEnvelope::new(
            "1",
            origin,
            space.map(|s| s.to_string()),
            "m.ping",
            json!({}),
        )
    }

    #[test]
    fn origin_must_match_peer() {
        assert!(check("b.example", &env("b.example", None)).is_ok());
        assert!(check("b.example", &env("c.example", None)).is_err());
    }

    #[test]
    fn space_must_be_homed_on_peer() {
        assert!(check("b.example", &env("b.example", Some("99@b.example"))).is_ok());
        // space homed elsewhere -> rejected
        assert!(check("b.example", &env("b.example", Some("99@c.example"))).is_err());
        // local/unqualified space -> rejected
        assert!(check("b.example", &env("b.example", Some("99"))).is_err());
    }

    #[test]
    fn remote_target_guard() {
        assert!(require_remote_target("5@b.example").is_ok());
        assert!(require_remote_target("5").is_err());
    }
}
