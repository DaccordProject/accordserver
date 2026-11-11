use crate::{
    e2ee::{
        account::{self, Device},
        Identity,
    },
    error::AppError,
    gateway::events::GatewayBroadcast,
    middleware::{
        auth::{create_token_hash, AuthUser},
        permissions::require_channel_membership,
    },
    state::AppState,
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::AnyConnection;
use std::collections::HashSet;

/// Account version 1 (v1 identity). Later versions are created by rotation.
pub async fn register(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(key): Json<Identity>,
) -> Result<Json<serde_json::Value>, AppError> {
    key.validate()?;
    if let Some(existing) = crate::e2ee::identity(&state.db, &auth.user_id).await? {
        if existing != key {
            return Err(AppError::Conflict("This account already has an encryption identity. Import its encrypted backup on this device.".into()));
        }
    }
    let wire = wire_id(&state, &auth.user_id);
    crate::federation::e2ee::cache_identity(&state, &auth.user_id, &wire, &key).await?;
    Ok(Json(serde_json::json!({"data":key})))
}

pub async fn participants(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let response = crate::federation::e2ee::discover(&state, &channel_id, &auth.user_id).await?;
    Ok(Json(serde_json::json!({"data":response})))
}

fn wire_id(state: &AppState, user_id: &str) -> String {
    state
        .federation
        .as_ref()
        .map(|fed| crate::federation::mapping::qualify(user_id, &fed.domain))
        .unwrap_or_else(|| user_id.to_owned())
}

/// Devices are bound to `user_tokens` sessions, so bot and guest tokens cannot
/// manage them. Returns the caller's session token hash.
fn session(auth: &AuthUser, headers: &HeaderMap) -> Result<String, AppError> {
    raw_session_token(auth, headers).map(create_token_hash)
}
fn raw_session_token<'h>(auth: &AuthUser, headers: &'h HeaderMap) -> Result<&'h str, AppError> {
    let token = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match token {
        Some(token) if !auth.is_bot && !auth.is_guest => Ok(token),
        _ => Err(AppError::Forbidden(
            "Encrypted chat devices require a signed-in user session.".into(),
        )),
    }
}

async fn emit(state: &AppState, user_id: &str, kind: &str, data: Value) {
    if let Some(ref gtx) = *state.gateway_tx.read().await {
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(vec![user_id.to_owned()]),
            event: json!({"op": 0, "type": kind, "data": data}),
            intent: "direct_messages".to_string(),
            required_permission: None,
        });
    }
}

async fn devices_update(state: &AppState, user_id: &str) {
    emit(state, user_id, "encryption.devices_update", json!({})).await;
}

fn stale_account() -> AppError {
    AppError::Conflict("Account encryption key changed. Refresh and try again.".into())
}
fn missing_account() -> AppError {
    AppError::BadRequest("Set up the account encryption identity first.".into())
}

async fn state_json(
    state: &AppState,
    conn: &mut AnyConnection,
    user_id: &str,
    session: &str,
) -> Result<Value, AppError> {
    let account = account::account(conn, user_id).await?;
    let devices: Vec<Value> = account::devices(conn, user_id)
        .await?
        .iter()
        .filter(|d| d.status != "revoked")
        .map(|d| d.row(Some(session)))
        .collect();
    Ok(json!({
        "user_context": account
            .as_ref()
            .map(|a| a.user_context.clone())
            .unwrap_or_else(|| wire_id(state, user_id)),
        "account": account.as_ref().map(|a| json!({
            "version": a.version,
            "exchange_key": a.keys.exchange_key,
            "signing_key": a.keys.signing_key,
        })),
        "successions": account::successions(conn, user_id).await?,
        "devices": devices,
    }))
}

/// `GET /users/@me/encryption`
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session(&auth, &headers)?;
    let mut conn = state.db.acquire().await?;
    let data = state_json(&state, &mut conn, &auth.user_id, &session).await?;
    Ok(Json(json!({ "data": data })))
}

#[derive(Deserialize)]
pub struct RegisterDevice {
    name: String,
    exchange_key: String,
    signing_key: String,
    certificate: Option<String>,
    account_version: Option<i64>,
    /// Device signing key over `["daccord-e2ee-bind-v2", user_context,
    /// device_id, base64(SHA-256(raw bearer token))]`.
    binding: Option<String>,
}

/// `PUT /users/@me/encryption/devices/{device_id}`: idempotent registration.
/// A valid certificate by the current account key makes the device active;
/// otherwise it is pending until approved. Binds the device to this session.
pub async fn put_device(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(input): Json<RegisterDevice>,
) -> Result<Json<Value>, AppError> {
    let raw_token = raw_session_token(&auth, &headers)?;
    let session = create_token_hash(raw_token);
    account::require_device_id(&device_id)?;
    account::validate_name(&input.name)?;
    let binding = input.binding.as_deref().ok_or_else(|| {
        AppError::BadRequest("Device registration requires a session binding signature.".into())
    })?;
    let keys = Identity {
        exchange_key: input.exchange_key,
        signing_key: input.signing_key,
    };
    keys.validate()?;
    let certified = match (&input.certificate, input.account_version) {
        (Some(certificate), Some(version)) => Some((certificate.as_str(), version)),
        (None, None) => None,
        _ => {
            return Err(AppError::BadRequest(
                "certificate and account_version must be sent together".into(),
            ))
        }
    };
    let user_id = auth.user_id.as_str();
    let now = account::now_ts();
    let mut tx = state.db.begin().await?;
    account::lock_account(&mut tx, user_id).await?;
    account::purge_expired(&mut tx, user_id).await?;
    let existing = account::device(&mut tx, user_id, &device_id).await?;
    if let Some(existing) = &existing {
        if existing.status == "revoked" {
            return Err(AppError::Forbidden(account::REVOKED_MESSAGE.into()));
        }
        if existing.keys != keys {
            return Err(AppError::Conflict(
                "This device ID is registered with different keys.".into(),
            ));
        }
    }
    // Proof of possession of the (immutable) device signing key for this session.
    let user_context = match account::account(&mut tx, user_id).await? {
        Some(account) => account.user_context,
        None => wire_id(&state, user_id),
    };
    account::verify(
        &keys.signing_key,
        &account::binding_message(&user_context, &device_id, &account::token_digest(raw_token)),
        binding,
    )
    .map_err(|_| AppError::BadRequest("Device session binding verification failed".into()))?;
    let was_active = existing.as_ref().is_some_and(Device::is_active);
    if let Some((certificate, version)) = certified {
        let account = account::account(&mut tx, user_id)
            .await?
            .ok_or_else(missing_account)?;
        if version != account.version {
            return Err(stale_account());
        }
        account::verify_certificate(
            &account.keys.signing_key,
            &account.user_context,
            version,
            &device_id,
            &keys,
            certificate,
        )?;
    }
    let status = if certified.is_some() || was_active {
        "active"
    } else {
        "pending"
    };
    if existing.is_none() {
        let (count,): (i64,) = sqlx::query_as(&crate::db::q(
            "SELECT COUNT(*) FROM e2ee_devices WHERE user_id=? AND status<>'revoked'",
        ))
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if count >= account::MAX_DEVICES {
            return Err(AppError::BadRequest(
                "This account has too many encrypted chat devices. Remove one first.".into(),
            ));
        }
    }
    // A session is bound to at most one device.
    sqlx::query(&crate::db::q(
        "UPDATE e2ee_devices SET token_hash=NULL WHERE user_id=? AND token_hash=? AND device_id<>?",
    ))
    .bind(user_id)
    .bind(&session)
    .bind(&device_id)
    .execute(&mut *tx)
    .await?;
    let certificate = certified.map(|c| c.0);
    let version = certified.map(|c| c.1);
    if existing.is_some() {
        sqlx::query(&crate::db::q(
            "UPDATE e2ee_devices SET name=?,status=?,token_hash=?,last_seen_at=?,certificate=COALESCE(?,certificate),account_version=COALESCE(?,account_version) WHERE user_id=? AND device_id=?",
        ))
        .bind(&input.name)
        .bind(status)
        .bind(&session)
        .bind(&now)
        .bind(certificate)
        .bind(version)
        .bind(user_id)
        .bind(&device_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(&crate::db::q(
            "INSERT INTO e2ee_devices(user_id,device_id,name,exchange_key,signing_key,certificate,account_version,status,token_hash,created_at,last_seen_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        ))
        .bind(user_id)
        .bind(&device_id)
        .bind(&input.name)
        .bind(&keys.exchange_key)
        .bind(&keys.signing_key)
        .bind(certificate)
        .bind(version)
        .bind(status)
        .bind(&session)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    let device = account::device(&mut tx, user_id, &device_id)
        .await?
        .ok_or_else(|| AppError::Internal("device registration vanished".into()))?;
    tx.commit().await?;
    if existing.is_none() && status == "pending" {
        emit(
            &state,
            user_id,
            "encryption.device_pending",
            device.row(None),
        )
        .await;
    } else if status == "active" && !was_active {
        devices_update(&state, user_id).await;
    }
    Ok(Json(json!({ "data": device.row(Some(&session)) })))
}

#[derive(Deserialize)]
pub struct ApproveDevice {
    certificate: String,
    account_version: i64,
    key_package: String,
}

/// The caller's session must be bound to an active device.
async fn require_active_caller(
    conn: &mut AnyConnection,
    user_id: &str,
    session: &str,
) -> Result<Device, AppError> {
    account::session_device(conn, user_id, session)
        .await?
        .filter(Device::is_active)
        .ok_or_else(|| {
            AppError::Forbidden("Use an approved device to manage encrypted chat devices.".into())
        })
}

/// `POST /users/@me/encryption/devices/{device_id}/approve`
pub async fn approve_device(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(input): Json<ApproveDevice>,
) -> Result<Json<Value>, AppError> {
    let session = session(&auth, &headers)?;
    account::require_device_id(&device_id)?;
    account::validate_key_package(&input.key_package)?;
    let user_id = auth.user_id.as_str();
    let mut tx = state.db.begin().await?;
    account::lock_account(&mut tx, user_id).await?;
    account::purge_expired(&mut tx, user_id).await?;
    let account = account::account(&mut tx, user_id)
        .await?
        .ok_or_else(missing_account)?;
    require_active_caller(&mut tx, user_id, &session).await?;
    let target = account::device(&mut tx, user_id, &device_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Unknown encrypted chat device".into()))?;
    if target.status != "pending" {
        return Err(AppError::Conflict(
            "Only pending devices can be approved.".into(),
        ));
    }
    if input.account_version != account.version {
        return Err(stale_account());
    }
    account::verify_certificate(
        &account.keys.signing_key,
        &account.user_context,
        account.version,
        &device_id,
        &target.keys,
        &input.certificate,
    )?;
    sqlx::query(&crate::db::q(
        "UPDATE e2ee_devices SET status='active',certificate=?,account_version=?,key_package=? WHERE user_id=? AND device_id=?",
    ))
    .bind(&input.certificate)
    .bind(account.version)
    .bind(&input.key_package)
    .bind(user_id)
    .bind(&device_id)
    .execute(&mut *tx)
    .await?;
    let device = account::device(&mut tx, user_id, &device_id)
        .await?
        .ok_or_else(|| AppError::Internal("approved device vanished".into()))?;
    tx.commit().await?;
    devices_update(&state, user_id).await;
    Ok(Json(json!({ "data": device.row(Some(&session)) })))
}

/// Only the session bound to the device may read or acknowledge its package.
async fn bound_device(
    conn: &mut AnyConnection,
    user_id: &str,
    device_id: &str,
    session: &str,
) -> Result<Device, AppError> {
    account::require_device_id(device_id)?;
    let device = account::device(conn, user_id, device_id)
        .await?
        .filter(|d| d.status != "revoked")
        .ok_or_else(|| AppError::NotFound("Unknown encrypted chat device".into()))?;
    if device.token_hash.as_deref() != Some(session) {
        return Err(AppError::Forbidden(
            "Only the device itself can read its key package.".into(),
        ));
    }
    Ok(device)
}

/// `GET /users/@me/encryption/devices/{device_id}/package`
pub async fn get_package(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session(&auth, &headers)?;
    let mut conn = state.db.acquire().await?;
    let device = bound_device(&mut conn, &auth.user_id, &device_id, &session).await?;
    let package = device
        .key_package
        .ok_or_else(|| AppError::NotFound("No key package for this device".into()))?;
    Ok(Json(json!({ "data": { "key_package": package } })))
}

/// `DELETE /users/@me/encryption/devices/{device_id}/package` acknowledges it.
pub async fn ack_package(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session(&auth, &headers)?;
    let mut conn = state.db.acquire().await?;
    let device = bound_device(&mut conn, &auth.user_id, &device_id, &session).await?;
    if device.key_package.is_none() {
        return Err(AppError::NotFound("No key package for this device".into()));
    }
    sqlx::query(&crate::db::q(
        "UPDATE e2ee_devices SET key_package=NULL WHERE user_id=? AND device_id=? AND token_hash=?",
    ))
    .bind(&auth.user_id)
    .bind(&device_id)
    .bind(&session)
    .execute(&mut *conn)
    .await?;
    Ok(Json(json!({ "data": {} })))
}

/// `DELETE /users/@me/encryption/devices/{device_id}` declines a pending device.
/// Allowed from an active device or from the pending device itself.
pub async fn decline_device(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session(&auth, &headers)?;
    account::require_device_id(&device_id)?;
    let user_id = auth.user_id.as_str();
    let mut tx = state.db.begin().await?;
    account::lock_account(&mut tx, user_id).await?;
    account::purge_expired(&mut tx, user_id).await?;
    let target = account::device(&mut tx, user_id, &device_id)
        .await?
        .filter(|d| d.status != "revoked")
        .ok_or_else(|| AppError::NotFound("Unknown encrypted chat device".into()))?;
    if target.is_active() {
        return Err(AppError::Conflict(
            "Use rotation to remove an active device.".into(),
        ));
    }
    if target.token_hash.as_deref() != Some(session.as_str()) {
        require_active_caller(&mut tx, user_id, &session).await?;
    }
    sqlx::query(&crate::db::q(
        "DELETE FROM e2ee_devices WHERE user_id=? AND device_id=? AND status='pending'",
    ))
    .bind(user_id)
    .bind(&device_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    devices_update(&state, user_id).await;
    Ok(Json(json!({ "data": {} })))
}

#[derive(Deserialize)]
pub struct RotationCertificate {
    device_id: String,
    certificate: String,
}
#[derive(Deserialize)]
pub struct RotationPackage {
    device_id: String,
    key_package: String,
}
#[derive(Deserialize)]
pub struct Rotate {
    revoke: Vec<String>,
    account: Identity,
    version: i64,
    signature: String,
    /// New signing key over the previous version's keys.
    back_signature: Option<String>,
    devices: Vec<RotationCertificate>,
    packages: Vec<RotationPackage>,
}

/// Exactly `expected`, no duplicates.
fn exact_set<'a>(ids: impl Iterator<Item = &'a str>, expected: &HashSet<&str>) -> bool {
    let mut seen = HashSet::new();
    for id in ids {
        if !expected.contains(id) || !seen.insert(id) {
            return false;
        }
    }
    seen.len() == expected.len()
}

/// `POST /users/@me/encryption/rotate`: create account version n+1, revoke
/// devices (deleting their sessions), re-certify and re-key the remaining
/// active devices, and drop pending devices — atomically.
pub async fn rotate(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(input): Json<Rotate>,
) -> Result<Json<Value>, AppError> {
    let session = session(&auth, &headers)?;
    input.account.validate()?;
    for package in &input.packages {
        account::validate_key_package(&package.key_package)?;
    }
    let user_id = auth.user_id.as_str();
    let now = account::now_ts();
    let mut tx = state.db.begin().await?;
    account::lock_account(&mut tx, user_id).await?;
    account::purge_expired(&mut tx, user_id).await?;
    let account = account::account(&mut tx, user_id)
        .await?
        .ok_or_else(missing_account)?;
    let caller = require_active_caller(&mut tx, user_id, &session).await?;
    if input.version != account.version + 1 {
        return Err(stale_account());
    }
    let devices = account::devices(&mut tx, user_id).await?;
    let active: Vec<&Device> = devices.iter().filter(|d| d.is_active()).collect();
    let mut revoke = HashSet::new();
    for id in &input.revoke {
        if id == &caller.device_id {
            return Err(AppError::BadRequest(
                "A device cannot remove itself while rotating.".into(),
            ));
        }
        if !active.iter().any(|d| &d.device_id == id) {
            return Err(AppError::Conflict(
                "Encrypted chat devices changed. Refresh and try again.".into(),
            ));
        }
        if !revoke.insert(id.as_str()) {
            return Err(AppError::BadRequest("Duplicate device in revoke".into()));
        }
    }
    let remaining: HashSet<&str> = active
        .iter()
        .map(|d| d.device_id.as_str())
        .filter(|id| !revoke.contains(id))
        .collect();
    let mut receivers = remaining.clone();
    receivers.remove(caller.device_id.as_str());
    if !exact_set(
        input.devices.iter().map(|d| d.device_id.as_str()),
        &remaining,
    ) || !exact_set(
        input.packages.iter().map(|p| p.device_id.as_str()),
        &receivers,
    ) {
        return Err(AppError::Conflict(
            "Encrypted chat devices changed. Refresh and try again.".into(),
        ));
    }
    if account::all_version_keys(&mut tx, user_id)
        .await?
        .iter()
        .chain(std::iter::once(&account.keys))
        .any(|k| {
            k.exchange_key == input.account.exchange_key
                || k.signing_key == input.account.signing_key
        })
    {
        return Err(AppError::BadRequest(
            "A rotated account key must be new.".into(),
        ));
    }
    account::verify(
        &account.keys.signing_key,
        &account::succession_message(&account.user_context, input.version, &input.account),
        &input.signature,
    )
    .map_err(|_| AppError::BadRequest("Succession signature verification failed".into()))?;
    let back_signature = input
        .back_signature
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Rotation requires a back_signature".into()))?;
    account::verify(
        &input.account.signing_key,
        &account::back_message(&account.user_context, input.version, &account.keys),
        back_signature,
    )
    .map_err(|_| AppError::BadRequest("Succession back-signature verification failed".into()))?;
    for entry in &input.devices {
        let device = active
            .iter()
            .find(|d| d.device_id == entry.device_id)
            .expect("checked above");
        account::verify_certificate(
            &input.account.signing_key,
            &account.user_context,
            input.version,
            &entry.device_id,
            &device.keys,
            &entry.certificate,
        )?;
    }

    account::insert_version(
        &mut tx,
        user_id,
        input.version,
        &input.account,
        Some(&input.signature),
        Some(back_signature),
    )
    .await?;
    let updated = sqlx::query(&crate::db::q(
        "UPDATE e2ee_identities SET exchange_key=?,signing_key=?,account_version=? WHERE user_id=? AND account_version=?",
    ))
    .bind(&input.account.exchange_key)
    .bind(&input.account.signing_key)
    .bind(input.version)
    .bind(user_id)
    .bind(account.version)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(stale_account());
    }
    for id in &revoke {
        let device = active.iter().find(|d| d.device_id == *id).unwrap();
        if let Some(hash) = device.token_hash.as_deref().filter(|h| *h != session) {
            sqlx::query(&crate::db::q(
                "DELETE FROM user_tokens WHERE user_id=? AND token_hash=?",
            ))
            .bind(user_id)
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(&crate::db::q(
            "UPDATE e2ee_devices SET status='revoked',revoked_at=?,token_hash=NULL,key_package=NULL WHERE user_id=? AND device_id=?",
        ))
        .bind(&now)
        .bind(user_id)
        .bind(*id)
        .execute(&mut *tx)
        .await?;
    }
    for entry in &input.devices {
        let package = input
            .packages
            .iter()
            .find(|p| p.device_id == entry.device_id)
            .map(|p| p.key_package.as_str());
        sqlx::query(&crate::db::q(
            "UPDATE e2ee_devices SET certificate=?,account_version=?,key_package=? WHERE user_id=? AND device_id=?",
        ))
        .bind(&entry.certificate)
        .bind(input.version)
        .bind(package)
        .bind(user_id)
        .bind(&entry.device_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(&crate::db::q(
        "DELETE FROM e2ee_devices WHERE user_id=? AND status='pending'",
    ))
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    let data = state_json(&state, &mut tx, user_id, &session).await?;
    tx.commit().await?;
    devices_update(&state, user_id).await;
    Ok(Json(json!({ "data": data })))
}
