//! Encryption v2 account versions and per-device keys. The server stores and
//! verifies public keys, certificates and sealed key packages only.
use super::{bytes, invalid, Identity};
use crate::{db, error::AppError};
use data_encoding::BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{AnyConnection, Row};

pub const DEVICE_LABEL: &str = "daccord-e2ee-device-v2";
pub const ROTATE_LABEL: &str = "daccord-e2ee-rotate-v2";
/// Non-revoked (pending + active) devices per account.
pub const MAX_DEVICES: i64 = 20;
pub const PENDING_TTL_HOURS: i64 = 24;
pub const MAX_KEY_PACKAGE: usize = 16 * 1024;
pub const MAX_NAME_CHARS: usize = 64;
pub const REVOKED_MESSAGE: &str = "This device was removed from encrypted chats.";
pub const NOT_APPROVED_MESSAGE: &str = "This device isn't approved for encrypted chats yet.";

/// RFC 3339 UTC with fixed millisecond precision, so stored values also sort
/// lexicographically in both database backends.
pub fn now_ts() -> String {
    timestamp(chrono::Utc::now())
}
fn timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn pending_cutoff() -> String {
    timestamp(chrono::Utc::now() - chrono::Duration::hours(PENDING_TTL_HOURS))
}

/// 16 random bytes, base64url without padding.
pub fn valid_device_id(id: &str) -> bool {
    id.len() == 22
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn require_device_id(id: &str) -> Result<(), AppError> {
    if !valid_device_id(id) {
        return Err(invalid("invalid encryption device ID"));
    }
    Ok(())
}
pub fn validate_name(name: &str) -> Result<(), AppError> {
    let count = name.chars().count();
    if count == 0 || count > MAX_NAME_CHARS {
        return Err(invalid("Device name must be 1-64 characters"));
    }
    Ok(())
}

fn compact(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("JSON arrays always serialize")
}
pub fn certificate_message(
    user_context: &str,
    version: i64,
    device_id: &str,
    exchange_key: &str,
    signing_key: &str,
) -> Vec<u8> {
    compact(&json!([
        DEVICE_LABEL,
        user_context,
        version,
        device_id,
        exchange_key,
        signing_key
    ]))
}
pub fn succession_message(user_context: &str, version: i64, account: &Identity) -> Vec<u8> {
    compact(&json!([
        ROTATE_LABEL,
        user_context,
        version,
        account.exchange_key,
        account.signing_key
    ]))
}

/// Strict Ed25519 verification of a padded base64 signature.
pub fn verify(signing_key: &str, message: &[u8], signature: &str) -> Result<(), AppError> {
    let key: [u8; 32] = bytes(signing_key, 32)?.try_into().unwrap();
    let signature = Signature::from_slice(&bytes(signature, 64)?)
        .map_err(|_| invalid("invalid E2EE signature"))?;
    VerifyingKey::from_bytes(&key)
        .map_err(|_| invalid("invalid E2EE signing key"))?
        .verify_strict(message, &signature)
        .map_err(|_| invalid("E2EE signature verification failed"))
}

pub fn verify_certificate(
    account_signing_key: &str,
    user_context: &str,
    version: i64,
    device_id: &str,
    device: &Identity,
    certificate: &str,
) -> Result<(), AppError> {
    verify(
        account_signing_key,
        &certificate_message(
            user_context,
            version,
            device_id,
            &device.exchange_key,
            &device.signing_key,
        ),
        certificate,
    )
    .map_err(|_| invalid("Device certificate verification failed"))
}

/// `{"ephemeral": base64(32 bytes), "box": base64(nonce || ciphertext || tag)}`
/// as a JSON string of at most 16 KiB. The server cannot open it.
pub fn validate_key_package(package: &str) -> Result<(), AppError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Package {
        ephemeral: String,
        #[serde(rename = "box")]
        sealed: String,
    }
    let fail = || invalid("invalid encryption key package");
    if package.len() > MAX_KEY_PACKAGE {
        return Err(invalid("encryption key package is too large"));
    }
    let parsed: Package = serde_json::from_str(package).map_err(|_| fail())?;
    let ephemeral = bytes(&parsed.ephemeral, 32).map_err(|_| fail())?;
    if ephemeral.iter().all(|b| *b == 0) {
        return Err(fail());
    }
    let sealed = BASE64
        .decode(parsed.sealed.as_bytes())
        .map_err(|_| fail())?;
    if sealed.len() < 29 || BASE64.encode(&sealed) != parsed.sealed {
        return Err(fail());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Succession {
    pub version: i64,
    pub exchange_key: String,
    pub signing_key: String,
    pub signature: String,
}
impl Succession {
    pub fn keys(&self) -> Identity {
        Identity {
            exchange_key: self.exchange_key.clone(),
            signing_key: self.signing_key.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Account {
    pub user_context: String,
    pub keys: Identity,
    pub version: i64,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub keys: Identity,
    pub certificate: Option<String>,
    pub account_version: Option<i64>,
    pub status: String,
    pub token_hash: Option<String>,
    pub key_package: Option<String>,
    pub created_at: String,
    pub last_seen_at: String,
}
impl Device {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
    /// The REST `DeviceRow`; `current` marks the row bound to the caller's session.
    pub fn row(&self, session: Option<&str>) -> Value {
        json!({
            "device_id": self.device_id,
            "name": self.name,
            "exchange_key": self.keys.exchange_key,
            "signing_key": self.keys.signing_key,
            "certificate": self.certificate,
            "account_version": self.account_version,
            "status": self.status,
            "created_at": self.created_at,
            "last_seen_at": self.last_seen_at,
            "current": session.is_some() && self.token_hash.as_deref() == session,
        })
    }
    /// Discovery/federation shape: public keys and certificate, no session data.
    pub fn public(&self) -> Value {
        json!({
            "device_id": self.device_id,
            "exchange_key": self.keys.exchange_key,
            "signing_key": self.keys.signing_key,
            "certificate": self.certificate,
            "account_version": self.account_version,
            "revoked": self.status == "revoked",
        })
    }
}

fn device_from_row(row: &sqlx::any::AnyRow) -> Device {
    Device {
        device_id: row.get("device_id"),
        name: row.get("name"),
        keys: Identity {
            exchange_key: row.get("exchange_key"),
            signing_key: row.get("signing_key"),
        },
        certificate: row.get("certificate"),
        account_version: row.get("account_version"),
        status: row.get("status"),
        token_hash: row.get("token_hash"),
        key_package: row.get("key_package"),
        created_at: row.get("created_at"),
        last_seen_at: row.get("last_seen_at"),
    }
}
const DEVICE_COLUMNS: &str = "device_id,name,exchange_key,signing_key,certificate,account_version,status,token_hash,key_package,created_at,last_seen_at";

pub async fn account(conn: &mut AnyConnection, user_id: &str) -> Result<Option<Account>, AppError> {
    let row = sqlx::query(&db::q(
        "SELECT user_context,exchange_key,signing_key,account_version FROM e2ee_identities WHERE user_id=?",
    ))
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| Account {
        user_context: row.get("user_context"),
        keys: Identity {
            exchange_key: row.get("exchange_key"),
            signing_key: row.get("signing_key"),
        },
        version: row.get("account_version"),
    }))
}

/// Takes the account's write lock (row lock in PostgreSQL, the database write
/// lock in SQLite) so device and rotation changes serialize per account.
pub async fn lock_account(conn: &mut AnyConnection, user_id: &str) -> Result<(), AppError> {
    sqlx::query(&db::q(
        "UPDATE e2ee_identities SET account_version=account_version WHERE user_id=?",
    ))
    .bind(user_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn version_keys(
    conn: &mut AnyConnection,
    user_id: &str,
    version: i64,
) -> Result<Option<Identity>, AppError> {
    let row = sqlx::query(&db::q(
        "SELECT exchange_key,signing_key FROM e2ee_account_versions WHERE user_id=? AND version=?",
    ))
    .bind(user_id)
    .bind(version)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| Identity {
        exchange_key: row.get("exchange_key"),
        signing_key: row.get("signing_key"),
    }))
}

pub async fn all_version_keys(
    conn: &mut AnyConnection,
    user_id: &str,
) -> Result<Vec<Identity>, AppError> {
    let rows = sqlx::query(&db::q(
        "SELECT exchange_key,signing_key FROM e2ee_account_versions WHERE user_id=?",
    ))
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|row| Identity {
            exchange_key: row.get("exchange_key"),
            signing_key: row.get("signing_key"),
        })
        .collect())
}

/// Signed successions for versions >= 2, ascending.
pub async fn successions(
    conn: &mut AnyConnection,
    user_id: &str,
) -> Result<Vec<Succession>, AppError> {
    let rows = sqlx::query(&db::q(
        "SELECT version,exchange_key,signing_key,signature FROM e2ee_account_versions WHERE user_id=? AND version>=2 AND signature IS NOT NULL ORDER BY version",
    ))
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|row| Succession {
            version: row.get("version"),
            exchange_key: row.get("exchange_key"),
            signing_key: row.get("signing_key"),
            signature: row.get("signature"),
        })
        .collect())
}

pub async fn insert_version(
    conn: &mut AnyConnection,
    user_id: &str,
    version: i64,
    keys: &Identity,
    signature: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(&db::q(
        "INSERT INTO e2ee_account_versions(user_id,version,exchange_key,signing_key,signature,created_at) VALUES (?,?,?,?,?,?) ON CONFLICT(user_id,version) DO NOTHING",
    ))
    .bind(user_id)
    .bind(version)
    .bind(&keys.exchange_key)
    .bind(&keys.signing_key)
    .bind(signature)
    .bind(now_ts())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Pending devices expire 24 hours after creation.
pub async fn purge_expired(conn: &mut AnyConnection, user_id: &str) -> Result<(), AppError> {
    sqlx::query(&db::q(
        "DELETE FROM e2ee_devices WHERE user_id=? AND status='pending' AND created_at<?",
    ))
    .bind(user_id)
    .bind(pending_cutoff())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Every device row, oldest first (expired pending rows excluded).
pub async fn devices(conn: &mut AnyConnection, user_id: &str) -> Result<Vec<Device>, AppError> {
    let rows = sqlx::query(&db::q(&format!(
        "SELECT {DEVICE_COLUMNS} FROM e2ee_devices WHERE user_id=? AND (status<>'pending' OR created_at>=?) ORDER BY created_at, device_id"
    )))
    .bind(user_id)
    .bind(pending_cutoff())
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(device_from_row).collect())
}

pub async fn device(
    conn: &mut AnyConnection,
    user_id: &str,
    device_id: &str,
) -> Result<Option<Device>, AppError> {
    let row = sqlx::query(&db::q(&format!(
        "SELECT {DEVICE_COLUMNS} FROM e2ee_devices WHERE user_id=? AND device_id=? AND (status<>'pending' OR created_at>=?)"
    )))
    .bind(user_id)
    .bind(device_id)
    .bind(pending_cutoff())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.as_ref().map(device_from_row))
}

/// Active device bound to the calling session, if any.
pub async fn session_device(
    conn: &mut AnyConnection,
    user_id: &str,
    token_hash: &str,
) -> Result<Option<Device>, AppError> {
    Ok(devices(conn, user_id)
        .await?
        .into_iter()
        .find(|d| d.token_hash.as_deref() == Some(token_hash)))
}

/// Public account state used by channel discovery and the federation identity
/// endpoint: current key, version, successions, and certified (active and
/// revoked, never pending) devices.
pub async fn public_account(conn: &mut AnyConnection, user_id: &str) -> Result<Value, AppError> {
    let Some(account) = account(conn, user_id).await? else {
        return Ok(json!({
            "identity": null,
            "account_version": null,
            "successions": [],
            "devices": [],
        }));
    };
    let devices: Vec<Value> = devices(conn, user_id)
        .await?
        .iter()
        .filter(|d| d.status != "pending" && d.certificate.is_some())
        .map(Device::public)
        .collect();
    Ok(json!({
        "identity": account.keys,
        "account_version": account.version,
        "successions": successions(conn, user_id).await?,
        "devices": devices,
    }))
}

/// Code-unit order, matching Dart's `String.compareTo`.
pub fn code_unit_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
