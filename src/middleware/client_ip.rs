use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use std::net::{IpAddr, SocketAddr};

/// Forwarded headers are trusted only when an operator explicitly opts in.
/// Such deployments must prevent direct access and have the proxy replace headers.
pub fn request_ip(parts: &Parts) -> String {
    if std::env::var("TRUST_PROXY_HEADERS")
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
