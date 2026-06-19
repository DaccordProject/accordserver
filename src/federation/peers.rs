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
/// `localhost`. This is the synchronous half (scheme + IP-literal checks);
/// [`validate_peer_url_resolved`] additionally resolves hostnames and rejects
/// those that map to internal addresses. A determined attacker controlling DNS
/// could still rebind between validation and connect (TOCTOU); fully closing
/// that requires pinning the resolved IP into the HTTP client.
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

/// Full SSRF guard: the synchronous checks plus DNS resolution of the host,
/// rejecting any name that resolves to a private/loopback/link-local address.
/// Use this before every outbound federation request.
pub async fn validate_peer_url_resolved(url: &str) -> Result<(), AppError> {
    validate_peer_url(url)?;
    if allow_insecure() {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| AppError::BadRequest("invalid peer url".to_string()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("peer url has no host".to_string()))?;
    // IP literals were already validated synchronously.
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let mut resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| AppError::BadRequest(format!("could not resolve peer host {host}: {e}")))?
        .peekable();
    if resolved.peek().is_none() {
        return Err(AppError::BadRequest(format!(
            "peer host {host} did not resolve"
        )));
    }
    for addr in resolved {
        if is_private(&addr.ip()) {
            return Err(AppError::BadRequest(format!(
                "peer host {host} resolves to a private address"
            )));
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
    validate_peer_url_resolved(&url).await?;

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
    validate_peer_url_resolved(&wk.inbox_url).await?;
    Ok(wk)
}
