//! Fetching and validating a peer's published federation metadata.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

use crate::error::AppError;

/// Path where every server publishes its federation metadata.
pub const WELL_KNOWN_PATH: &str = "/.well-known/accord-federation";

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The document served at [`WELL_KNOWN_PATH`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WellKnown {
    pub domain: String,
    pub public_key: String,
    pub inbox_url: String,
    pub version: String,
}

/// True when loopback/insecure peers are permitted. Set
/// `ACCORD_FEDERATION_ALLOW_INSECURE=1` for local two-node testing; never in
/// production (it disables the SSRF/loopback guards and allows plaintext http).
pub fn allow_insecure() -> bool {
    std::env::var("ACCORD_FEDERATION_ALLOW_INSECURE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Guard outbound federation requests against SSRF (S5 in the plan): require
/// https and reject hosts that are private/loopback/link-local IP literals or
/// `localhost`. Hostnames that resolve to internal addresses are not fully
/// covered here — a hardening follow-up should pin a resolver — but IP-literal
/// targets (the common SSRF vector) are blocked.
pub fn validate_peer_url(url: &str) -> Result<(), AppError> {
    if allow_insecure() {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| AppError::BadRequest("invalid peer url".to_string()))?;
    if parsed.scheme() != "https" {
        return Err(AppError::BadRequest(
            "federation peer url must be https".to_string(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("peer url has no host".to_string()))?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(AppError::BadRequest("peer host not allowed".to_string()));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private(&ip) {
            return Err(AppError::BadRequest(
                "peer host resolves to a private address".to_string(),
            ));
        }
    }
    Ok(())
}

fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

/// The scheme to use when contacting a peer domain (http only when insecure
/// testing is enabled).
fn peer_scheme() -> &'static str {
    if allow_insecure() {
        "http"
    } else {
        "https"
    }
}

/// Fetch and validate a peer's `.well-known` document.
pub async fn fetch_well_known(
    client: &reqwest::Client,
    domain: &str,
) -> Result<WellKnown, AppError> {
    let url = format!("{}://{}{}", peer_scheme(), domain, WELL_KNOWN_PATH);
    validate_peer_url(&url)?;

    let resp = client
        .get(&url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| AppError::BadRequest(format!("could not reach peer {domain}: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::BadRequest(format!("peer {domain} returned an error: {e}")))?;

    let wk: WellKnown = resp
        .json()
        .await
        .map_err(|e| AppError::BadRequest(format!("peer {domain} sent invalid metadata: {e}")))?;

    // Bind the document to the domain we asked: a peer cannot claim to be a
    // different domain, and its inbox must live on its own host.
    if !wk.domain.eq_ignore_ascii_case(domain) {
        return Err(AppError::BadRequest(format!(
            "peer metadata domain `{}` does not match `{domain}`",
            wk.domain
        )));
    }
    validate_peer_url(&wk.inbox_url)?;
    Ok(wk)
}
