//! HTTP-signature signing (outbound) and verification (inbound) for
//! server-to-server federation requests.
//!
//! The signature covers a fixed canonical string built from the request
//! method, path, `Host`, `Date`, and a SHA-256 `Digest` of the body. Binding
//! the body digest means a tampered payload fails verification even without
//! TLS (defense in depth, S3 in the plan), and the `Date` header bounds replay
//! to a small clock-skew window.

use data_encoding::BASE64;
use sha2::{Digest, Sha256};

/// Maximum allowed clock skew between peers for the `Date` header.
const MAX_SKEW_SECS: i64 = 300;

/// Headers to attach to a signed outbound request.
pub struct SignedHeaders {
    pub date: String,
    pub digest: String,
    pub signature: String,
}

/// `SHA-256=<base64>` digest header value for a body.
pub fn body_digest(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    format!("SHA-256={}", BASE64.encode(&hasher.finalize()))
}

/// The canonical string both peers sign/verify. Field order is fixed.
fn signing_string(method: &str, path: &str, host: &str, date: &str, digest: &str) -> String {
    format!(
        "{} {}\nhost: {}\ndate: {}\ndigest: {}",
        method.to_uppercase(),
        path,
        host,
        date,
        digest
    )
}

/// Sign a request, producing the `Date`, `Digest`, and `Signature` headers.
///
/// `key_id` is this server's domain (echoed in the signature so the receiver
/// knows which peer key to verify against).
pub fn sign_request(
    identity: &super::identity::ServerIdentity,
    key_id: &str,
    method: &str,
    path: &str,
    host: &str,
    body: &[u8],
) -> SignedHeaders {
    let date = httpdate_now();
    let digest = body_digest(body);
    let signing = signing_string(method, path, host, &date, &digest);
    let sig = identity.sign_b64(signing.as_bytes());
    let signature = format!("keyId=\"{key_id}\",algorithm=\"ed25519\",signature=\"{sig}\"");
    SignedHeaders {
        date,
        digest,
        signature,
    }
}

/// The `keyId` (signing peer domain) and detached signature parsed out of a
/// `Signature` header.
pub struct ParsedSignature {
    pub key_id: String,
    pub signature_b64: String,
}

/// Parse a `Signature` header of the form
/// `keyId="...",algorithm="ed25519",signature="..."`.
pub fn parse_signature_header(value: &str) -> Option<ParsedSignature> {
    let mut key_id = None;
    let mut signature_b64 = None;
    for part in value.split(',') {
        let (k, v) = part.split_once('=')?;
        let v = v.trim().trim_matches('"');
        match k.trim() {
            "keyId" => key_id = Some(v.to_string()),
            "signature" => signature_b64 = Some(v.to_string()),
            _ => {}
        }
    }
    Some(ParsedSignature {
        key_id: key_id?,
        signature_b64: signature_b64?,
    })
}

/// Why a signed request was rejected. Mapped to HTTP status by the inbox.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    DigestMismatch,
    StaleDate,
    BadSignature,
}

/// Verify an inbound signed request against a peer's published public key.
#[allow(clippy::too_many_arguments)]
pub fn verify_request(
    public_key_b64: &str,
    method: &str,
    path: &str,
    host: &str,
    date: &str,
    digest: &str,
    body: &[u8],
    signature_b64: &str,
) -> Result<(), VerifyError> {
    // Body integrity: the signed digest must match the actual bytes received.
    if body_digest(body) != digest {
        return Err(VerifyError::DigestMismatch);
    }
    // Replay bound: reject timestamps outside the skew window.
    if !date_within_skew(date) {
        return Err(VerifyError::StaleDate);
    }
    let signing = signing_string(method, path, host, date, digest);
    if super::identity::verify_b64(public_key_b64, signing.as_bytes(), signature_b64) {
        Ok(())
    } else {
        Err(VerifyError::BadSignature)
    }
}

fn httpdate_now() -> String {
    // RFC 2822 is accepted by chrono's RFC2822 parser on the way back in.
    chrono::Utc::now().to_rfc2822()
}

fn date_within_skew(date: &str) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc2822(date) else {
        return false;
    };
    let delta = (chrono::Utc::now() - parsed.to_utc()).num_seconds().abs();
    delta <= MAX_SKEW_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::identity::ServerIdentity;

    fn test_identity() -> ServerIdentity {
        let dir = std::env::temp_dir().join(format!("accord-sig-{}", crate::snowflake::generate()));
        ServerIdentity::load_or_create(&dir.join("k")).unwrap()
    }

    #[test]
    fn sign_then_verify_ok() {
        let id = test_identity();
        let body = br#"{"hello":"world"}"#;
        let h = sign_request(
            &id,
            "a.example",
            "POST",
            "/federation/v1/inbox",
            "b.example",
            body,
        );
        let res = verify_request(
            &id.public_key_b64(),
            "POST",
            "/federation/v1/inbox",
            "b.example",
            &h.date,
            &h.digest,
            body,
            &parse_signature_header(&h.signature).unwrap().signature_b64,
        );
        assert_eq!(res, Ok(()));
    }

    #[test]
    fn tampered_body_fails_digest() {
        let id = test_identity();
        let body = br#"{"hello":"world"}"#;
        let h = sign_request(&id, "a.example", "POST", "/inbox", "b.example", body);
        let sig = parse_signature_header(&h.signature).unwrap().signature_b64;
        let res = verify_request(
            &id.public_key_b64(),
            "POST",
            "/inbox",
            "b.example",
            &h.date,
            &h.digest,
            b"different body",
            &sig,
        );
        assert_eq!(res, Err(VerifyError::DigestMismatch));
    }

    #[test]
    fn wrong_key_fails_signature() {
        let id = test_identity();
        let other = test_identity();
        let body = b"{}";
        let h = sign_request(&id, "a.example", "POST", "/inbox", "b.example", body);
        let sig = parse_signature_header(&h.signature).unwrap().signature_b64;
        let res = verify_request(
            &other.public_key_b64(),
            "POST",
            "/inbox",
            "b.example",
            &h.date,
            &h.digest,
            body,
            &sig,
        );
        assert_eq!(res, Err(VerifyError::BadSignature));
    }

    #[test]
    fn stale_date_rejected() {
        let id = test_identity();
        let body = b"{}";
        let digest = body_digest(body);
        let old_date =
            (chrono::Utc::now() - chrono::Duration::seconds(MAX_SKEW_SECS + 60)).to_rfc2822();
        let signing = signing_string("POST", "/inbox", "b.example", &old_date, &digest);
        let sig = id.sign_b64(signing.as_bytes());
        let res = verify_request(
            &id.public_key_b64(),
            "POST",
            "/inbox",
            "b.example",
            &old_date,
            &digest,
            body,
            &sig,
        );
        assert_eq!(res, Err(VerifyError::StaleDate));
    }
}
