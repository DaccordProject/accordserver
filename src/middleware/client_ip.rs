use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use std::net::{IpAddr, SocketAddr};

/// Forwarded headers are trusted only when an operator explicitly opts in.
/// Such deployments must prevent direct access and have the proxy replace headers.
pub fn request_ip(parts: &Parts) -> String {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let trusted = peer.is_some_and(|ip| {
        std::env::var("TRUSTED_PROXY_IPS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<IpAddr>().ok())
            .any(|allowed| allowed == ip)
    });
    if trusted
        && std::env::var("TRUST_PROXY_HEADERS")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        let forwarded = parts
            .headers
            .get("X-Forwarded-For")
            .or_else(|| parts.headers.get("X-Real-IP"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        if let Some(ip) = forwarded {
            return ip.to_string();
        }
    }
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

pub struct ClientIp(pub String);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(request_ip(parts)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[serial_test::serial]
    fn only_allowlisted_socket_peers_can_supply_forwarded_addresses() {
        let previous_flag = std::env::var("TRUST_PROXY_HEADERS").ok();
        let previous_ips = std::env::var("TRUSTED_PROXY_IPS").ok();
        std::env::set_var("TRUST_PROXY_HEADERS", "true");
        std::env::set_var("TRUSTED_PROXY_IPS", "192.0.2.10");
        let (mut parts, _) = http::Request::builder()
            .header("X-Forwarded-For", "198.51.100.25")
            .body(())
            .unwrap()
            .into_parts();
        parts.extensions.insert(ConnectInfo(
            "192.0.2.11:1234".parse::<SocketAddr>().unwrap(),
        ));
        assert_eq!(request_ip(&parts), "192.0.2.11");
        parts.extensions.insert(ConnectInfo(
            "192.0.2.10:1234".parse::<SocketAddr>().unwrap(),
        ));
        assert_eq!(request_ip(&parts), "198.51.100.25");
        std::env::remove_var("TRUSTED_PROXY_IPS");
        assert_eq!(request_ip(&parts), "192.0.2.10");
        for (name, previous) in [
            ("TRUST_PROXY_HEADERS", previous_flag),
            ("TRUSTED_PROXY_IPS", previous_ips),
        ] {
            if let Some(value) = previous {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}
