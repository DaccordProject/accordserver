//! This server's federation identity: an Ed25519 keypair used to sign all
//! outbound server-to-server requests.
//!
//! The private key is persisted as raw base64 beside the master-server-id file
//! (reusing the storage-dir convention from [`crate::config`]). It is created
//! on first use with `0600` permissions and is never logged.

use data_encoding::BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::Path;

/// This server's signing identity.
#[derive(Clone)]
pub struct ServerIdentity {
    signing_key: SigningKey,
}

impl ServerIdentity {
    /// Load the keypair from `path`, generating and persisting a new one only
    /// when the file does **not exist**.
    ///
    /// A file that exists but is unreadable (e.g. a permissions error) or
    /// unparseable is treated as a hard error rather than silently overwritten:
    /// regenerating the key would rotate this server's federation identity and
    /// break every established peer's pinned trust.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                return parse_signing_key(contents.trim())
                    .map(|key| Self { signing_key: key })
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "federation key at {path:?} is present but unparseable; \
                                 refusing to overwrite (fix or remove it to regenerate)"
                            ),
                        )
                    });
            }
            // No key yet: fall through and generate one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Exists but unreadable: do not clobber an existing identity.
            Err(e) => return Err(e),
        }

        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let encoded = BASE64.encode(&signing_key.to_bytes());

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &encoded)?;
        set_owner_only(path);

        Ok(Self { signing_key })
    }

    /// The base64-encoded public key published at the `.well-known` endpoint.
    pub fn public_key_b64(&self) -> String {
        BASE64.encode(self.signing_key.verifying_key().as_bytes())
    }

    /// Sign a message, returning a base64-encoded detached signature.
    pub fn sign_b64(&self, message: &[u8]) -> String {
        let sig: Signature = self.signing_key.sign(message);
        BASE64.encode(&sig.to_bytes())
    }
}

fn parse_signing_key(encoded: &str) -> Option<SigningKey> {
    let bytes = BASE64.decode(encoded.as_bytes()).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(SigningKey::from_bytes(&arr))
}

/// Verify a base64 detached signature of `message` against a base64 public key.
pub fn verify_b64(public_key_b64: &str, message: &[u8], signature_b64: &str) -> bool {
    let Some(key) = parse_verifying_key(public_key_b64) else {
        return false;
    };
    let Ok(sig_bytes) = BASE64.decode(signature_b64.as_bytes()) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_arr);
    key.verify(message, &signature).is_ok()
}

fn parse_verifying_key(encoded: &str) -> Option<VerifyingKey> {
    let bytes = BASE64.decode(encoded.as_bytes()).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

#[cfg(unix)]
fn set_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let dir = std::env::temp_dir().join(format!("accord-fed-{}", crate::snowflake::generate()));
        let path = dir.join("federation_key");
        let id = ServerIdentity::load_or_create(&path).unwrap();

        let msg = b"hello federation";
        let sig = id.sign_b64(msg);
        assert!(verify_b64(&id.public_key_b64(), msg, &sig));
        assert!(!verify_b64(&id.public_key_b64(), b"tampered", &sig));

        // Reloading from disk yields the same key.
        let id2 = ServerIdentity::load_or_create(&path).unwrap();
        assert_eq!(id.public_key_b64(), id2.public_key_b64());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
