//! Fetching and validating a peer's published federation metadata.

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
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

/// True when an address must never be the target of an outbound federation
/// request: loopback, private/RFC1918, link-local, CGNAT, IPv6 ULA, etc. IPv6
/// addresses that embed an IPv4 address (mapped/compatible) are folded back to
/// their V4 form so an attacker cannot smuggle `::ffff:127.0.0.1` past the V4
/// checks.
fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_private_v4(&mapped);
            }
            // `to_ipv4()` also covers the deprecated IPv4-compatible range
            // (::a.b.c.d), which would otherwise slip through as a global V6.
            if let Some(compat) = v6.to_ipv4() {
                return is_private_v4(&compat);
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique local addresses (fc00::/7).
                || (seg[0] & 0xfe00) == 0xfc00
                // Link-local unicast (fe80::/10).
                || (seg[0] & 0xffc0) == 0xfe80
                // Documentation prefix (2001:db8::/32).
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        }
    }
}

fn is_private_v4(v4: &std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_documentation()
        // Carrier-grade NAT (100.64.0.0/10).
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // "This host on this network" (0.0.0.0/8).
        || o[0] == 0
        // Reserved/benchmarking and multicast ranges have no business being a
        // unicast federation peer.
        || v4.is_multicast()
}

/// Custom DNS resolver that re-applies [`is_private`] at connect time, closing
/// the TOCTOU/DNS-rebinding gap between [`validate_peer_url_resolved`] and the
/// actual TCP connection. Wired into the federation HTTP client via
/// `ClientBuilder::dns_resolver`.
#[derive(Debug, Clone, Default)]
pub struct SsrfGuardResolver;

impl Resolve for SsrfGuardResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            if allow_insecure() {
                // Defer to the system resolver without filtering.
                let host = name.as_str().to_string();
                let addrs = tokio::net::lookup_host((host, 0)).await?;
                let iter: Addrs = Box::new(addrs);
                return Ok(iter);
            }
            let host = name.as_str().to_string();
            let resolved: Vec<SocketAddr> =
                tokio::net::lookup_host((host.clone(), 0)).await?.collect();
            if resolved.is_empty() {
                return Err(format!("host {host} did not resolve").into());
            }
            for addr in &resolved {
                if is_private(&addr.ip()) {
                    return Err(
                        format!("host {host} resolves to a private/internal address").into(),
                    );
                }
            }
            let iter: Addrs = Box::new(resolved.into_iter());
            Ok(iter)
        })
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
