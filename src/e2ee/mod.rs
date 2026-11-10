//! Private-message admission. This server has public keys only.
//!
//! v1 (`daccord-e2ee:1:`) envelopes are signed by the account key and wrapped
//! to each participant's account key. v2 (`daccord-e2ee:2:`) envelopes are
//! signed by a certified sender device and wrapped to every participant's
//! account key plus each of their active devices (see [`account`]).
pub mod account;

use crate::{db, error::AppError};
use data_encoding::BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::AnyPool;

pub const PREFIX: &str = "daccord-e2ee:1:";
pub const PREFIX_V2: &str = "daccord-e2ee:2:";
pub const MAX_ENVELOPE: usize = 256 * 1024;
pub const MAX_V2_RECIPIENTS: usize = 400;
pub const STALE_KEYS: &str = "Encryption keys changed. Refresh before sending.";

/// True for any private-chat encryption envelope version.
pub fn is_encrypted(content: &str) -> bool {
    content.starts_with(PREFIX) || content.starts_with(PREFIX_V2)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub exchange_key: String,
    pub signing_key: String,
}

fn invalid(message: &str) -> AppError {
    AppError::BadRequest(message.into())
}
pub fn bytes(value: &str, size: usize) -> Result<Vec<u8>, AppError> {
    let decoded = BASE64
        .decode(value.as_bytes())
        .map_err(|_| invalid("invalid E2EE base64"))?;
    if decoded.len() != size || BASE64.encode(&decoded) != value {
        return Err(invalid("invalid E2EE key or nonce length"));
    }
    Ok(decoded)
}
impl Identity {
    pub fn validate(&self) -> Result<(), AppError> {
        let exchange = bytes(&self.exchange_key, 32)?;
        if exchange.iter().all(|b| *b == 0) {
            return Err(invalid("invalid exchange key"));
        }
        let signing: [u8; 32] = bytes(&self.signing_key, 32)?.try_into().unwrap();
        VerifyingKey::from_bytes(&signing).map_err(|_| invalid("invalid signing key"))?;
        Ok(())
    }
}

pub async fn identity(pool: &AnyPool, user_id: &str) -> Result<Option<Identity>, AppError> {
    let row: Option<(String, String)> = sqlx::query_as(&db::q(
        "SELECT exchange_key, signing_key FROM e2ee_identities WHERE user_id=?",
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(exchange_key, signing_key)| Identity {
        exchange_key,
        signing_key,
    }))
}

pub async fn user_context(pool: &AnyPool, user_id: &str) -> Result<String, AppError> {
    Ok(sqlx::query_scalar(&db::q(
        "SELECT user_context FROM e2ee_identities WHERE user_id=?",
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or_else(|| user_id.to_owned()))
}
pub async fn chat_context(pool: &AnyPool, channel_id: &str) -> Result<String, AppError> {
    Ok(sqlx::query_scalar(&db::q(
        "SELECT wire_id FROM e2ee_chat_contexts WHERE channel_id=?",
    ))
    .bind(channel_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or_else(|| channel_id.to_owned()))
}

/// Message IDs and replies use the chat home namespace on every client.
pub fn message_context(chat_context: &str, message_id: &str) -> String {
    match crate::federation::mapping::domain_of(chat_context) {
        Some(domain) => crate::federation::mapping::qualify(message_id, domain),
        None => message_id.to_owned(),
    }
}

// Arrays give Dart and Rust the same signing bytes without object key ordering.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: Vec<Value>,
    signature: String,
}
fn string(value: &Value) -> Result<&str, AppError> {
    value
        .as_str()
        .ok_or_else(|| invalid("invalid E2EE envelope"))
}
fn box_bytes(value: &Value, min: usize, max: usize) -> Result<(), AppError> {
    let encoded = string(value)?;
    let decoded = BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| invalid("invalid encrypted payload"))?;
    if !(min..=max).contains(&decoded.len()) || BASE64.encode(&decoded) != encoded {
        return Err(invalid("invalid encrypted payload size"));
    }
    Ok(())
}

/// Called by the database writer too, so MCP and federation cannot bypass it.
pub async fn validate_message(
    pool: &AnyPool,
    channel_id: &str,
    author_id: &str,
    content: &str,
    reply_to: Option<&str>,
    edit_id: Option<&str>,
) -> Result<(), AppError> {
    let channel = db::channels::get_channel_row(pool, channel_id).await?;
    if !matches!(channel.channel_type.as_str(), "dm" | "group_dm") {
        if is_encrypted(content) {
            return Err(invalid("E2EE is only supported in private chats"));
        }
        if content.len() > 4000 {
            return Err(invalid("message content must be at most 4000 bytes"));
        }
        return Ok(());
    }
    if content.len() > MAX_ENVELOPE {
        return Err(invalid("encrypted message is too large"));
    }
    if let Some(body) = content.strip_prefix(PREFIX_V2) {
        return validate_v2(pool, channel_id, author_id, body, reply_to, edit_id).await;
    }
    let body = content.strip_prefix(PREFIX).ok_or_else(|| {
        invalid("Private chats require end-to-end encryption. Upgrade your client.")
    })?;
    let envelope: Envelope =
        serde_json::from_str(body).map_err(|_| invalid("invalid E2EE envelope"))?;
    let p = &envelope.payload;
    let context = chat_context(pool, channel_id).await?;
    let expected_reply = reply_to
        .map(|id| Value::from(message_context(&context, id)))
        .unwrap_or(Value::Null);
    let expected_edit = edit_id
        .map(|id| Value::from(message_context(&context, id)))
        .unwrap_or(Value::Null);
    if p.len() != 9
        || p[0] != 1
        || string(&p[1])? != chat_context(pool, channel_id).await?
        || string(&p[2])? != user_context(pool, author_id).await?
        || p[4] != expected_reply
        || p[5] != expected_edit
    {
        return Err(invalid("E2EE message context does not match request"));
    }
    bytes(string(&p[3])?, 16)?;
    bytes(string(&p[6])?, 32)?;
    box_bytes(&p[7], 29, 64 * 1024)?;
    let participants = db::dm_participants::list_participant_ids(pool, channel_id).await?;
    if !participants.iter().any(|id| id == author_id) {
        return Err(AppError::Forbidden("not a private chat participant".into()));
    }
    let recipients = p[8]
        .as_array()
        .ok_or_else(|| invalid("invalid E2EE recipients"))?;
    if recipients.len() != participants.len() || recipients.len() > 100 {
        return Err(invalid(
            "E2EE recipients must match current chat membership",
        ));
    }
    let mut expected = std::collections::HashMap::new();
    for id in &participants {
        expected.insert(user_context(pool, id).await?, id.clone());
    }
    let mut previous = "";
    for value in recipients {
        let r = value
            .as_array()
            .ok_or_else(|| invalid("invalid E2EE recipient"))?;
        if r.len() != 3 {
            return Err(invalid("invalid E2EE recipient"));
        }
        let id = string(&r[0])?;
        if id <= previous || !expected.contains_key(id) {
            return Err(invalid("invalid or duplicate E2EE recipient"));
        }
        previous = id;
        let key = identity(pool, expected[id].as_str())
            .await?
            .ok_or_else(|| invalid("Every participant must set up encryption before sending."))?;
        if string(&r[1])? != key.exchange_key {
            return Err(AppError::Conflict(
                "Encryption keys changed. Refresh before sending.".into(),
            ));
        }
        box_bytes(&r[2], 60, 60)?;
    }
    let key = identity(pool, author_id)
        .await?
        .ok_or_else(|| invalid("sender encryption key missing"))?;
    let signing: [u8; 32] = bytes(&key.signing_key, 32)?.try_into().unwrap();
    let signature = Signature::from_slice(&bytes(&envelope.signature, 64)?)
        .map_err(|_| invalid("invalid E2EE signature"))?;
    VerifyingKey::from_bytes(&signing)
        .map_err(|_| invalid("invalid E2EE signing key"))?
        .verify_strict(
            &serde_json::to_vec(p).map_err(|_| invalid("invalid E2EE payload"))?,
            &signature,
        )
        .map_err(|_| invalid("E2EE signature verification failed"))?;
    Ok(())
}

pub fn validate_private_metadata(
    input: &crate::models::message::CreateMessage,
) -> Result<(), AppError> {
    if input.tts.unwrap_or(false)
        || input.embeds.as_ref().is_some_and(|e| !e.is_empty())
        || input.title.is_some()
        || input.thread_id.is_some()
    {
        return Err(invalid(
            "Private message content and metadata must be encrypted",
        ));
    }
    Ok(())
}

/// Retain all accepted tokens, including previous revisions and deleted messages.
/// The token is payload item 3 in v1 and item 4 (after the device ID) in v2.
pub async fn reserve_token(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    author_id: &str,
    content: &str,
) -> Result<(), AppError> {
    let (body, index) = if let Some(body) = content.strip_prefix(PREFIX) {
        (body, 3)
    } else if let Some(body) = content.strip_prefix(PREFIX_V2) {
        (body, 4)
    } else {
        return Ok(());
    };
    let e: Envelope = serde_json::from_str(body).map_err(|_| invalid("invalid E2EE envelope"))?;
    let token = string(
        e.payload
            .get(index)
            .ok_or_else(|| invalid("invalid E2EE envelope"))?,
    )?;
    let admitted = sqlx::query(&db::q("INSERT INTO e2ee_message_tokens (user_id,token) VALUES (?,?) ON CONFLICT(user_id,token) DO NOTHING"))
        .bind(author_id).bind(token).execute(&mut **tx).await?;
    if admitted.rows_affected() == 0 {
        return Err(AppError::Conflict(
            "Encrypted message was already accepted".into(),
        ));
    }
    Ok(())
}

/// Serialize membership mutations and private sends on the same channel row.
/// SQLite serializes writers; PostgreSQL additionally needs an explicit row lock.
pub async fn lock_chat(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    channel_id: &str,
) -> Result<(), AppError> {
    if db::is_pg() {
        let row: Option<(String,)> =
            sqlx::query_as(&db::q("SELECT id FROM channels WHERE id=? FOR UPDATE"))
                .bind(channel_id)
                .fetch_optional(&mut **tx)
                .await?;
        if row.is_none() {
            return Err(AppError::NotFound("Unknown private chat".into()));
        }
    }
    Ok(())
}

/// Recheck after taking the write lock, closing the discovery/admission race.
pub async fn enforce_recipients(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    author_id: &str,
    channel_id: &str,
    content: &str,
) -> Result<(), AppError> {
    if let Some(body) = content.strip_prefix(PREFIX_V2) {
        return enforce_v2(tx, author_id, channel_id, body).await;
    }
    let Some(body) = content.strip_prefix(PREFIX) else {
        return Ok(());
    };
    let envelope: Envelope =
        serde_json::from_str(body).map_err(|_| invalid("Invalid encrypted envelope"))?;
    let rows: Vec<(String,Option<String>,Option<String>)>=sqlx::query_as(&db::q("SELECT p.user_id,e.user_context,e.exchange_key FROM dm_participants p LEFT JOIN e2ee_identities e ON e.user_id=p.user_id WHERE p.channel_id=?"))
        .bind(channel_id).fetch_all(&mut **tx).await?;
    if !rows.iter().any(|(id, _, _)| id == author_id) {
        return Err(AppError::Forbidden("Not a private chat participant".into()));
    }
    let mut expected = Vec::new();
    for (_, context, key) in rows {
        let (Some(context), Some(key)) = (context, key) else {
            return Err(invalid(
                "Every participant must set up encryption before sending",
            ));
        };
        expected.push((context, key));
    }
    expected.sort();
    let recipients = envelope.payload[8]
        .as_array()
        .ok_or_else(|| invalid("Invalid recipients"))?;
    if recipients.len() != expected.len()
        || recipients
            .iter()
            .zip(expected)
            .any(|(row, (id, key))| row[0] != id || row[1] != key)
    {
        return Err(AppError::Conflict(
            "Private chat membership changed. Refresh before sending.".into(),
        ));
    }
    Ok(())
}

/// One v2 recipient entry: `[user_context, key_id, exchange_public_key, wrapped_key]`.
type Recipient = (String, String, String);

/// Structure, ordering and sizes of the v2 recipient list. Entries must be
/// strictly ascending by (user_context, key_id) in code-unit order.
fn v2_recipients(value: &Value) -> Result<Vec<Recipient>, AppError> {
    let list = value
        .as_array()
        .ok_or_else(|| invalid("invalid E2EE recipients"))?;
    if list.is_empty() || list.len() > MAX_V2_RECIPIENTS {
        return Err(invalid("invalid E2EE recipient count"));
    }
    let mut out: Vec<Recipient> = Vec::with_capacity(list.len());
    for value in list {
        let r = value
            .as_array()
            .filter(|r| r.len() == 4)
            .ok_or_else(|| invalid("invalid E2EE recipient"))?;
        let (user, key_id, key) = (string(&r[0])?, string(&r[1])?, string(&r[2])?);
        if key_id != "account" && !account::valid_device_id(key_id) {
            return Err(invalid("invalid E2EE recipient key"));
        }
        bytes(key, 32)?;
        box_bytes(&r[3], 60, 60)?;
        if let Some((prev_user, prev_key, _)) = out.last() {
            let order = account::code_unit_cmp(prev_user, user)
                .then_with(|| account::code_unit_cmp(prev_key, key_id));
            if order != std::cmp::Ordering::Less {
                return Err(invalid("invalid or duplicate E2EE recipient"));
            }
        }
        out.push((user.to_owned(), key_id.to_owned(), key.to_owned()));
    }
    Ok(out)
}

/// The exact v2 recipient set for the chat's current participants: one
/// `"account"` entry per participant plus one entry per active device.
async fn expected_v2_recipients(
    conn: &mut sqlx::AnyConnection,
    channel_id: &str,
) -> Result<Vec<Recipient>, AppError> {
    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(&db::q(
        "SELECT p.user_id,e.user_context,e.exchange_key FROM dm_participants p LEFT JOIN e2ee_identities e ON e.user_id=p.user_id WHERE p.channel_id=?",
    ))
    .bind(channel_id)
    .fetch_all(&mut *conn)
    .await?;
    let devices: Vec<(String, String, String)> = sqlx::query_as(&db::q(
        "SELECT d.user_id,d.device_id,d.exchange_key FROM e2ee_devices d JOIN dm_participants p ON p.user_id=d.user_id WHERE p.channel_id=? AND d.status='active'",
    ))
    .bind(channel_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut expected = Vec::new();
    for (user_id, context, key) in rows {
        let (Some(context), Some(key)) = (context, key) else {
            return Err(invalid(
                "Every participant must set up encryption before sending.",
            ));
        };
        for (_, device_id, device_key) in devices.iter().filter(|d| d.0 == user_id) {
            expected.push((context.clone(), device_id.clone(), device_key.clone()));
        }
        expected.push((context, "account".to_owned(), key));
    }
    expected.sort_by(|a, b| {
        account::code_unit_cmp(&a.0, &b.0).then_with(|| account::code_unit_cmp(&a.1, &b.1))
    });
    Ok(expected)
}

fn same_recipients(actual: &[Recipient], expected: &[Recipient]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(a, e)| a.0 == e.0 && a.1 == e.1 && a.2 == e.2)
}

/// The sender device must be active and certified by the author's current
/// account version. Returns the device's signing key.
async fn sender_device(
    conn: &mut sqlx::AnyConnection,
    author_id: &str,
    device_id: &str,
) -> Result<String, AppError> {
    let account = account::account(conn, author_id)
        .await?
        .ok_or_else(|| invalid("sender encryption key missing"))?;
    let device = account::device(conn, author_id, device_id)
        .await?
        .filter(|d| d.status != "pending");
    let Some(device) = device else {
        return Err(AppError::Forbidden(account::NOT_APPROVED_MESSAGE.into()));
    };
    if device.status == "revoked" {
        return Err(AppError::Forbidden(account::REVOKED_MESSAGE.into()));
    }
    let (Some(certificate), Some(version)) = (&device.certificate, device.account_version) else {
        return Err(AppError::Forbidden(account::NOT_APPROVED_MESSAGE.into()));
    };
    if version != account.version {
        return Err(AppError::Conflict(STALE_KEYS.into()));
    }
    account::verify_certificate(
        &account.keys.signing_key,
        &account.user_context,
        version,
        device_id,
        &device.keys,
        certificate,
    )?;
    Ok(device.keys.signing_key)
}

/// v2 admission: structure, contexts, sender device certificate and signature,
/// and the recipient set. The exact set is rechecked under the chat lock by
/// [`enforce_recipients`].
async fn validate_v2(
    pool: &AnyPool,
    channel_id: &str,
    author_id: &str,
    body: &str,
    reply_to: Option<&str>,
    edit_id: Option<&str>,
) -> Result<(), AppError> {
    let envelope: Envelope =
        serde_json::from_str(body).map_err(|_| invalid("invalid E2EE envelope"))?;
    let p = &envelope.payload;
    let context = chat_context(pool, channel_id).await?;
    let expected_reply = reply_to
        .map(|id| Value::from(message_context(&context, id)))
        .unwrap_or(Value::Null);
    let expected_edit = edit_id
        .map(|id| Value::from(message_context(&context, id)))
        .unwrap_or(Value::Null);
    if p.len() != 10
        || p[0] != 2
        || string(&p[1])? != context
        || string(&p[2])? != user_context(pool, author_id).await?
        || p[5] != expected_reply
        || p[6] != expected_edit
    {
        return Err(invalid("E2EE message context does not match request"));
    }
    let device_id = string(&p[3])?;
    account::require_device_id(device_id)?;
    bytes(string(&p[4])?, 16)?;
    bytes(string(&p[7])?, 32)?;
    box_bytes(&p[8], 29, 64 * 1024)?;
    let recipients = v2_recipients(&p[9])?;
    let participants = db::dm_participants::list_participant_ids(pool, channel_id).await?;
    if !participants.iter().any(|id| id == author_id) {
        return Err(AppError::Forbidden("not a private chat participant".into()));
    }
    let signing_key = sender_device(&mut *pool.acquire().await?, author_id, device_id).await?;
    account::verify(
        &signing_key,
        &serde_json::to_vec(p).map_err(|_| invalid("invalid E2EE payload"))?,
        &envelope.signature,
    )?;
    if db::federation::channel_origin(pool, channel_id)
        .await?
        .is_some()
    {
        // A replica is not the admission authority: the chat home enforces the
        // exact set under its chat lock. Our own key view can legitimately be
        // newer than the home's cache, so only membership is checked here.
        let mut contexts = std::collections::HashSet::new();
        for id in &participants {
            contexts.insert(user_context(pool, id).await?);
        }
        let accounts: Vec<&str> = recipients
            .iter()
            .filter(|r| r.1 == "account")
            .map(|r| r.0.as_str())
            .collect();
        if accounts.len() != contexts.len() || recipients.iter().any(|r| !contexts.contains(&r.0)) {
            return Err(AppError::Conflict(STALE_KEYS.into()));
        }
        return Ok(());
    }
    let expected = expected_v2_recipients(&mut *pool.acquire().await?, channel_id).await?;
    if !same_recipients(&recipients, &expected) {
        return Err(AppError::Conflict(STALE_KEYS.into()));
    }
    Ok(())
}

/// Under the chat lock: the sender device is still active and the recipient
/// set still matches. In PostgreSQL the participants' identity rows are share
/// locked so a concurrent rotation (which updates them) cannot interleave.
async fn enforce_v2(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    author_id: &str,
    channel_id: &str,
    body: &str,
) -> Result<(), AppError> {
    let envelope: Envelope =
        serde_json::from_str(body).map_err(|_| invalid("Invalid encrypted envelope"))?;
    let p = &envelope.payload;
    if p.len() != 10 {
        return Err(invalid("Invalid encrypted envelope"));
    }
    let origin: Option<Option<String>> =
        sqlx::query_scalar(&db::q("SELECT origin FROM channels WHERE id=?"))
            .bind(channel_id)
            .fetch_optional(&mut **tx)
            .await?;
    if db::is_pg() {
        sqlx::query(&db::q(
            "SELECT e.user_id FROM e2ee_identities e JOIN dm_participants p ON p.user_id=e.user_id WHERE p.channel_id=? FOR SHARE OF e",
        ))
        .bind(channel_id)
        .fetch_all(&mut **tx)
        .await?;
    }
    let participant: Option<(String,)> = sqlx::query_as(&db::q(
        "SELECT user_id FROM dm_participants WHERE channel_id=? AND user_id=?",
    ))
    .bind(channel_id)
    .bind(author_id)
    .fetch_optional(&mut **tx)
    .await?;
    if participant.is_none() {
        return Err(AppError::Forbidden("Not a private chat participant".into()));
    }
    sender_device(tx, author_id, string(&p[3])?).await?;
    if origin.flatten().is_some() {
        return Ok(());
    }
    let recipients = v2_recipients(&p[9])?;
    let expected = expected_v2_recipients(tx, channel_id).await?;
    if !same_recipients(&recipients, &expected) {
        return Err(AppError::Conflict(STALE_KEYS.into()));
    }
    Ok(())
}
