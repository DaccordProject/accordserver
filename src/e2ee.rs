//! Private-message admission. This server has public keys only.
use crate::{db, error::AppError};
use data_encoding::BASE64;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::AnyPool;

pub const PREFIX: &str = "daccord-e2ee:1:";
pub const MAX_ENVELOPE: usize = 256 * 1024;

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
        if content.starts_with(PREFIX) {
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
pub async fn reserve_token(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    author_id: &str,
    content: &str,
) -> Result<(), AppError> {
    if let Some(body) = content.strip_prefix(PREFIX) {
        let e: Envelope =
            serde_json::from_str(body).map_err(|_| invalid("invalid E2EE envelope"))?;
        let token = string(&e.payload[3])?;
        let admitted = sqlx::query(&db::q("INSERT INTO e2ee_message_tokens (user_id,token) VALUES (?,?) ON CONFLICT(user_id,token) DO NOTHING"))
            .bind(author_id).bind(token).execute(&mut **tx).await?;
        if admitted.rows_affected() == 0 {
            return Err(AppError::Conflict(
                "Encrypted message was already accepted".into(),
            ));
        }
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
