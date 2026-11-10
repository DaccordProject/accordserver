//! Authenticated identity discovery for federated private chats.
use super::{authority, mapping, sender};
use crate::{
    db,
    e2ee::{
        self,
        account::{self, Succession},
        Identity,
    },
    error::AppError,
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub const IDENTITY_PATH: &str = "/federation/v1/encryption/identity";
pub const CHAT_PATH: &str = "/federation/v1/encryption/chat";
/// The chat home refreshes remote participants older than this during discovery.
const REFRESH_MS: i64 = 30_000;
const MAX_REMOTE_DEVICES: usize = 1000;

#[derive(Deserialize)]
struct RemoteDevice {
    device_id: String,
    exchange_key: String,
    signing_key: String,
    certificate: String,
    account_version: i64,
    #[serde(default)]
    revoked: bool,
}
impl RemoteDevice {
    fn keys(&self) -> Identity {
        Identity {
            exchange_key: self.exchange_key.clone(),
            signing_key: self.signing_key.clone(),
        }
    }
}
#[derive(Deserialize)]
struct IdentityRequest {
    user_id: String,
    nonce: String,
}
#[derive(Deserialize)]
struct ChatRequest {
    actor_id: String,
    channel_id: String,
    nonce: String,
}

pub async fn handle_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (domain, _, request): (_, _, IdentityRequest) =
        match super::verify::prepare(&state, &headers, IDENTITY_PATH, &body).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let _ = &request.nonce;
    let result = async {
        authority::require_homed_on(&request.user_id, &domain, "identity")?;
        let id = mapping::participant_storage_id(&request.user_id, Some(&domain));
        let mut conn = state.db.acquire().await?;
        let mut response = account::public_account(&mut conn, &id).await?;
        response["user_id"] = json!(request.user_id);
        Ok::<_, AppError>(response)
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Fetch a remote user's account and devices from their home server. A cached
/// account only moves forward along a valid succession chain.
pub async fn refresh_remote(state: &AppState, user_id: &str) -> Result<(), AppError> {
    let Some((_, domain)) = user_id.rsplit_once('@') else {
        return Ok(());
    };
    let body =
        serde_json::to_vec(&json!({"user_id":user_id,"nonce":uuid::Uuid::new_v4().to_string()}))
            .unwrap();
    let (status, bytes) = sender::request_signed(state, domain, IDENTITY_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(
            "Participant server does not support private encryption".into(),
        ));
    }
    let response: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::BadRequest("Invalid remote identity response".into()))?;
    if response["user_id"] != user_id {
        return Err(AppError::BadRequest(
            "Remote identity context mismatch".into(),
        ));
    }
    cache_account(state, user_id, user_id, &response).await
}

/// Refresh a current remote participant whose cache is older than 30 seconds.
/// Failures keep the cached keys (the participant's server may be offline).
async fn refresh_if_stale(state: &AppState, user_id: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    let refreshed: Option<Option<i64>> = sqlx::query_scalar(&db::q(
        "SELECT refreshed_at FROM e2ee_identities WHERE user_id=?",
    ))
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);
    if refreshed.flatten().is_some_and(|at| now - at < REFRESH_MS) {
        return;
    }
    // Record the attempt first so an offline server is not retried on every request.
    let _ = sqlx::query(&db::q(
        "UPDATE e2ee_identities SET refreshed_at=? WHERE user_id=?",
    ))
    .bind(now)
    .bind(user_id)
    .execute(&state.db)
    .await;
    if let Err(error) = refresh_remote(state, user_id).await {
        tracing::warn!(user_id, ?error, "remote encryption identity refresh failed");
    }
}

/// At the chat home, a forwarded v2 message may come from a device approved
/// since our last refresh. Refresh its author once before admission.
pub async fn refresh_sender(state: &AppState, author_id: &str, content: &str) {
    if !author_id.contains('@') {
        return;
    }
    let Some(device_id) = content
        .strip_prefix(e2ee::PREFIX_V2)
        .and_then(|body| serde_json::from_str::<Value>(body).ok())
        .and_then(|e| e["payload"][3].as_str().map(str::to_owned))
    else {
        return;
    };
    if !account::valid_device_id(&device_id) {
        return;
    }
    let known = match state.db.acquire().await {
        Ok(mut conn) => account::device(&mut conn, author_id, &device_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|d| d.is_active()),
        Err(_) => return,
    };
    if !known {
        if let Err(error) = refresh_remote(state, author_id).await {
            tracing::warn!(author_id, ?error, "remote sender device refresh failed");
        }
    }
}

/// v1 registration/caching of account version 1. Keys are immutable: a later
/// version must arrive through [`cache_account`]'s succession chain.
pub async fn cache_identity(
    state: &AppState,
    user_id: &str,
    wire_id: &str,
    key: &Identity,
) -> Result<(), AppError> {
    key.validate()?;
    let mut tx = state.db.begin().await?;
    let inserted = sqlx::query(&db::q("INSERT INTO e2ee_identities(user_id,exchange_key,signing_key,user_context) VALUES (?,?,?,?) ON CONFLICT(user_id) DO NOTHING"))
        .bind(user_id).bind(&key.exchange_key).bind(&key.signing_key).bind(wire_id).execute(&mut *tx).await?;
    if inserted.rows_affected() == 1 {
        account::insert_version(&mut tx, user_id, 1, key, None).await?;
    }
    let current = account::account(&mut tx, user_id).await?;
    tx.commit().await?;
    if current
        .as_ref()
        .is_none_or(|a| &a.keys != key || a.user_context != wire_id)
    {
        return Err(AppError::Conflict(
            "Remote encryption identity changed".into(),
        ));
    }
    Ok(())
}

fn bad_remote(message: &str) -> AppError {
    AppError::Conflict(format!("Remote encryption identity rejected: {message}"))
}

/// Cache a remote account from a home server's identity or discovery row:
/// `{identity, account_version, successions, devices}`. A new version is
/// accepted only through valid successions from the cached version; device
/// rows are replaced after certificate verification. Older responses are
/// ignored. Revoked devices remain revoked.
pub async fn cache_account(
    state: &AppState,
    user_id: &str,
    wire_id: &str,
    row: &Value,
) -> Result<(), AppError> {
    if row["identity"].is_null() {
        return Ok(());
    }
    let identity: Identity = serde_json::from_value(row["identity"].clone())
        .map_err(|_| AppError::BadRequest("Invalid remote identity".into()))?;
    identity.validate()?;
    let version = match &row["account_version"] {
        Value::Null => 1,
        value => value
            .as_i64()
            .filter(|v| *v >= 1)
            .ok_or_else(|| bad_remote("invalid account version"))?,
    };
    let successions: Vec<Succession> = match &row["successions"] {
        Value::Null => Vec::new(),
        value => {
            serde_json::from_value(value.clone()).map_err(|_| bad_remote("invalid successions"))?
        }
    };
    let devices: Vec<RemoteDevice> = match &row["devices"] {
        Value::Null => Vec::new(),
        value => {
            serde_json::from_value(value.clone()).map_err(|_| bad_remote("invalid devices"))?
        }
    };
    if devices.len() > MAX_REMOTE_DEVICES || successions.len() as i64 > version {
        return Err(bad_remote("too many entries"));
    }
    for s in &successions {
        s.keys().validate()?;
    }
    let succession = |v: i64| successions.iter().find(|s| s.version == v);
    if version > 1 && succession(version).is_none_or(|s| s.keys() != identity) {
        return Err(bad_remote("succession does not end at the current key"));
    }
    let mut tx = state.db.begin().await?;
    account::lock_account(&mut tx, user_id).await?;
    match account::account(&mut tx, user_id).await? {
        None => {
            // First sight: trust on first use, as v1. Keep whatever part of the
            // chain verifies internally so older device certificates can be checked.
            sqlx::query(&db::q("INSERT INTO e2ee_identities(user_id,exchange_key,signing_key,user_context,account_version) VALUES (?,?,?,?,?)"))
                .bind(user_id).bind(&identity.exchange_key).bind(&identity.signing_key).bind(wire_id).bind(version)
                .execute(&mut *tx).await?;
            if version == 1 {
                account::insert_version(&mut tx, user_id, 1, &identity, None).await?;
            }
            let mut v = version;
            while let Some(s) = succession(v) {
                if v == 1 {
                    account::insert_version(&mut tx, user_id, 1, &s.keys(), None).await?;
                    break;
                }
                let Some(signature) = s.signature.as_deref() else {
                    break;
                };
                account::insert_version(&mut tx, user_id, v, &s.keys(), Some(signature)).await?;
                let Some(previous) = succession(v - 1) else {
                    break;
                };
                if account::verify(
                    &previous.signing_key,
                    &account::succession_message(wire_id, v, &s.keys()),
                    signature,
                )
                .is_err()
                {
                    break;
                }
                v -= 1;
            }
        }
        Some(cached) => {
            if cached.user_context != wire_id {
                return Err(AppError::Conflict(
                    "Remote encryption identity changed".into(),
                ));
            }
            if version < cached.version {
                return Ok(()); // a stale view (e.g. another server's cache)
            }
            if version == cached.version && cached.keys != identity {
                return Err(AppError::Conflict(
                    "Remote encryption identity changed".into(),
                ));
            }
            let mut previous = cached.keys.clone();
            for v in cached.version + 1..=version {
                let s = succession(v).ok_or_else(|| bad_remote("missing succession"))?;
                let signature = s
                    .signature
                    .as_deref()
                    .ok_or_else(|| bad_remote("unsigned succession"))?;
                account::verify(
                    &previous.signing_key,
                    &account::succession_message(wire_id, v, &s.keys()),
                    signature,
                )
                .map_err(|_| bad_remote("invalid succession signature"))?;
                account::insert_version(&mut tx, user_id, v, &s.keys(), Some(signature)).await?;
                previous = s.keys();
            }
            if version > cached.version {
                sqlx::query(&db::q("UPDATE e2ee_identities SET exchange_key=?,signing_key=?,account_version=? WHERE user_id=?"))
                    .bind(&identity.exchange_key).bind(&identity.signing_key).bind(version).bind(user_id)
                    .execute(&mut *tx).await?;
            }
        }
    }
    let cached: std::collections::HashMap<String, account::Device> =
        account::devices(&mut tx, user_id)
            .await?
            .into_iter()
            .map(|d| (d.device_id.clone(), d))
            .collect();
    let now = account::now_ts();
    let mut seen = std::collections::HashSet::new();
    for d in &devices {
        account::require_device_id(&d.device_id)?;
        if !seen.insert(d.device_id.as_str()) {
            return Err(bad_remote("duplicate device"));
        }
        d.keys().validate()?;
        if !d.revoked && d.account_version != version {
            return Err(bad_remote(
                "active device is not certified by the current key",
            ));
        }
        let Some(signer) = account::version_keys(&mut tx, user_id, d.account_version).await? else {
            continue; // certified by a version this server never saw; cannot verify
        };
        account::verify_certificate(
            &signer.signing_key,
            wire_id,
            d.account_version,
            &d.device_id,
            &d.keys(),
            &d.certificate,
        )
        .map_err(|_| bad_remote("invalid device certificate"))?;
        let status = if d.revoked { "revoked" } else { "active" };
        match cached.get(&d.device_id) {
            Some(old) if old.keys != d.keys() => {
                return Err(bad_remote("device keys changed"));
            }
            Some(old) if old.status == "revoked" && !d.revoked => {
                return Err(bad_remote("revoked device reactivated"));
            }
            Some(old) if old.status == "revoked" => {}
            Some(_) => {
                sqlx::query(&db::q("UPDATE e2ee_devices SET status=?,certificate=?,account_version=?,last_seen_at=?,revoked_at=CASE WHEN ?='revoked' THEN ? ELSE NULL END WHERE user_id=? AND device_id=?"))
                    .bind(status).bind(&d.certificate).bind(d.account_version).bind(&now).bind(status).bind(&now).bind(user_id).bind(&d.device_id)
                    .execute(&mut *tx).await?;
            }
            None => {
                sqlx::query(&db::q("INSERT INTO e2ee_devices(user_id,device_id,name,exchange_key,signing_key,certificate,account_version,status,created_at,last_seen_at,revoked_at) VALUES (?,?,'',?,?,?,?,?,?,?,?)"))
                    .bind(user_id).bind(&d.device_id).bind(&d.exchange_key).bind(&d.signing_key).bind(&d.certificate).bind(d.account_version).bind(status).bind(&now).bind(&now)
                    .bind(d.revoked.then_some(now.as_str()))
                    .execute(&mut *tx).await?;
            }
        }
    }
    // Tombstones stay even when the home omits them; everything else is replaced.
    for (id, old) in &cached {
        if old.status != "revoked" && !seen.contains(id.as_str()) {
            sqlx::query(&db::q(
                "DELETE FROM e2ee_devices WHERE user_id=? AND device_id=?",
            ))
            .bind(user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query(&db::q(
        "UPDATE e2ee_identities SET refreshed_at=? WHERE user_id=?",
    ))
    .bind(chrono::Utc::now().timestamp_millis())
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn discover(
    state: &AppState,
    channel_id: &str,
    actor_id: &str,
) -> Result<Value, AppError> {
    let channel = db::channels::get_channel_row(&state.db, channel_id).await?;
    if !matches!(channel.channel_type.as_str(), "dm" | "group_dm") {
        return Err(AppError::BadRequest(
            "Encryption is only available in private chats".into(),
        ));
    }
    if !db::dm_participants::is_participant(&state.db, channel_id, actor_id).await? {
        return Err(AppError::Forbidden("Not a private chat participant".into()));
    }
    if let Some(home) = db::federation::channel_origin(&state.db, channel_id).await? {
        let fed = state
            .federation
            .as_ref()
            .ok_or_else(|| AppError::BadRequest("Federation is disabled".into()))?;
        let actor = mapping::qualify(actor_id, &fed.domain);
        let body=serde_json::to_vec(&json!({"actor_id":actor,"channel_id":channel_id,"nonce":uuid::Uuid::new_v4().to_string()})).unwrap();
        let (status, bytes) = sender::request_signed(state, &home, CHAT_PATH, &body).await?;
        if !status.is_success() {
            return Err(AppError::BadRequest(
                "Home server could not provide private encryption keys".into(),
            ));
        }
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::BadRequest("Invalid encryption discovery response".into()))?;
        if response["channel_id"] != channel_id || response["self_user_id"] != actor {
            return Err(AppError::BadRequest(
                "Encryption discovery context mismatch".into(),
            ));
        }
        let mut rows = response["participants"]
            .as_array()
            .ok_or_else(|| AppError::BadRequest("Invalid encryption discovery response".into()))?
            .clone();
        for row in &mut rows {
            let wire = row["wire_user_id"]
                .as_str()
                .ok_or_else(|| AppError::BadRequest("Invalid participant identity".into()))?
                .to_owned();
            let local = mapping::participant_storage_id(&wire, Some(&fed.domain));
            // A former author may not yet be cached on a replica; don't invent profile
            // data. Our own users' keys are authoritative here, never the home's cache.
            if local.contains('@') && db::users::get_user(&state.db, &local).await.is_ok() {
                cache_account(state, &local, &wire, row).await?;
            }
            row["user_id"] = json!(local);
        }
        remember_chat(state, channel_id, channel_id).await?;
        return Ok(
            json!({"channel_id":channel_id,"self_user_id":actor,"cdn_url":response["cdn_url"],"participants":rows}),
        );
    }
    let current = db::dm_participants::list_participant_ids(&state.db, channel_id).await?;
    let former: Vec<(String,)> = sqlx::query_as(&db::q(
        "SELECT DISTINCT author_id FROM messages WHERE channel_id=?",
    ))
    .bind(channel_id)
    .fetch_all(&state.db)
    .await?;
    let mut users = current.clone();
    for (id,) in former {
        if !users.contains(&id) {
            users.push(id);
        }
    }
    users.sort();
    let mut rows = Vec::new();
    for id in users {
        // Old history continues to use a pinned key when a former member's server is offline.
        if id.contains('@') {
            if e2ee::identity(&state.db, &id).await?.is_none() {
                refresh_remote(state, &id).await?;
            } else if current.contains(&id) {
                refresh_if_stale(state, &id).await;
            }
        }
        let wire = if id.contains('@') {
            id.clone()
        } else if let Some(fed) = &state.federation {
            mapping::qualify(&id, &fed.domain)
        } else {
            id.clone()
        };
        let mut row = account::public_account(&mut *state.db.acquire().await?, &id).await?;
        row["user_id"] = json!(id);
        row["wire_user_id"] = json!(wire);
        row["current"] = json!(current.contains(&id));
        rows.push(row);
    }
    let context = state
        .federation
        .as_ref()
        .map(|fed| mapping::qualify(channel_id, &fed.domain))
        .unwrap_or_else(|| channel_id.into());
    remember_chat(state, channel_id, &context).await?;
    let self_id = state
        .federation
        .as_ref()
        .map(|fed| mapping::qualify(actor_id, &fed.domain))
        .unwrap_or_else(|| actor_id.into());
    Ok(
        json!({"channel_id":context,"self_user_id":self_id,"cdn_url":state.federation.as_ref().map(|fed|format!("{}/cdn",fed.public_url.trim_end_matches('/'))),"participants":rows}),
    )
}

async fn remember_chat(state: &AppState, channel_id: &str, context: &str) -> Result<(), AppError> {
    sqlx::query(&db::q("INSERT INTO e2ee_chat_contexts(channel_id,wire_id) VALUES (?,?) ON CONFLICT(channel_id) DO NOTHING"))
        .bind(channel_id).bind(context).execute(&state.db).await?;
    if e2ee::chat_context(&state.db, channel_id).await? != context {
        return Err(AppError::Conflict(
            "Private chat encryption context changed".into(),
        ));
    }
    Ok(())
}

pub async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (domain, peer, request): (_, _, ChatRequest) =
        match super::verify::prepare(&state, &headers, CHAT_PATH, &body).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let _ = &request.nonce;
    let result = async {
        authority::require_homed_on(&request.actor_id, &peer.domain, "actor")?;
        authority::require_homed_on(&request.channel_id, &domain, "channel")?;
        let channel_id = mapping::participant_storage_id(&request.channel_id, Some(&domain));
        discover(&state, &channel_id, &request.actor_id).await
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}
