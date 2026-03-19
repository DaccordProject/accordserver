use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use sqlx::Row;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::{create_token_hash, generate_token, AuthUser};
use crate::snowflake;
use crate::state::{
    AppState, GuestAttemptTracker, LoginFailureTracker, MfaTicket, RegisterAttemptTracker,
    TotpAttemptTracker,
};

// ---------------------------------------------------------------------------
// Session limit constant
// ---------------------------------------------------------------------------
const MAX_SESSIONS_PER_USER: i64 = 25;

// ---------------------------------------------------------------------------
// TOTP rate limiting constants
// ---------------------------------------------------------------------------
const TOTP_MAX_FAILURES: u32 = 5;
const TOTP_WINDOW_SECS: u64 = 900; // 15 minutes

// ---------------------------------------------------------------------------
// Login brute-force protection constants
// ---------------------------------------------------------------------------
const LOGIN_MAX_FAILURES: u32 = 5;
const LOGIN_WINDOW_SECS: u64 = 900; // 15 minutes

// ---------------------------------------------------------------------------
// Register rate limiting constants
// ---------------------------------------------------------------------------
const REGISTER_MAX_ATTEMPTS: u32 = 5;
const REGISTER_WINDOW_SECS: u64 = 900; // 15 minutes

// ---------------------------------------------------------------------------
// Guest token constants
// ---------------------------------------------------------------------------
const GUEST_MAX_ATTEMPTS: u32 = 10;
const GUEST_WINDOW_SECS: u64 = 3600; // 1 hour
const GUEST_TOKEN_LIFETIME_SECS: i64 = 3600; // 1 hour

// ---------------------------------------------------------------------------
// Request structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct MfaLoginRequest {
    pub ticket: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct Enable2faRequest {
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct Verify2faRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct Disable2faRequest {
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct RegenerateBackupCodesRequest {
    pub password: String,
}

// ---------------------------------------------------------------------------
// TOTP encryption helpers
// ---------------------------------------------------------------------------

fn encrypt_totp_secret(secret: &str, key: Option<&[u8; 32]>) -> String {
    let Some(key) = key else {
        return secret.to_string();
    };

    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, secret.as_bytes())
        .expect("TOTP encryption failed");

    // Store as "nonce_hex:ciphertext_hex"
    format!(
        "enc:{}:{}",
        hex_encode(&nonce_bytes),
        hex_encode(&ciphertext)
    )
}

fn decrypt_totp_secret(stored: &str, key: Option<&[u8; 32]>) -> Result<String, AppError> {
    if !stored.starts_with("enc:") {
        // Plaintext (either no key was set when stored, or legacy)
        return Ok(stored.to_string());
    }

    let key = key.ok_or_else(|| {
        AppError::Internal(
            "TOTP secret is encrypted but TOTP_ENCRYPTION_KEY is not set".to_string(),
        )
    })?;

    let parts: Vec<&str> = stored.splitn(3, ':').collect();
    if parts.len() != 3 {
        return Err(AppError::Internal(
            "malformed encrypted TOTP secret".to_string(),
        ));
    }

    let nonce_bytes =
        hex_decode(parts[1]).map_err(|_| AppError::Internal("bad TOTP nonce".to_string()))?;
    let ciphertext =
        hex_decode(parts[2]).map_err(|_| AppError::Internal("bad TOTP ciphertext".to_string()))?;

    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(&nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| AppError::Internal("TOTP decryption failed — wrong key?".to_string()))?;

    String::from_utf8(plaintext)
        .map_err(|_| AppError::Internal("decrypted TOTP secret is not valid UTF-8".to_string()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

// ---------------------------------------------------------------------------
// TOTP rate limiting
// ---------------------------------------------------------------------------

fn check_totp_rate_limit(state: &AppState, user_id: &str) -> Result<(), AppError> {
    let now = Instant::now();
    if let Some(tracker) = state.totp_attempts.get(user_id) {
        let elapsed = now.duration_since(tracker.window_start).as_secs();
        if elapsed < TOTP_WINDOW_SECS && tracker.failures >= TOTP_MAX_FAILURES {
            let retry_after = TOTP_WINDOW_SECS - elapsed;
            return Err(AppError::RateLimited { retry_after });
        }
    }
    Ok(())
}

fn record_totp_failure(state: &AppState, user_id: &str) {
    let now = Instant::now();
    state
        .totp_attempts
        .entry(user_id.to_string())
        .and_modify(|t| {
            let elapsed = now.duration_since(t.window_start).as_secs();
            if elapsed >= TOTP_WINDOW_SECS {
                // Reset window
                t.failures = 1;
                t.window_start = now;
            } else {
                t.failures += 1;
            }
        })
        .or_insert(TotpAttemptTracker {
            failures: 1,
            window_start: now,
        });
}

fn clear_totp_failures(state: &AppState, user_id: &str) {
    state.totp_attempts.remove(user_id);
}

// ---------------------------------------------------------------------------
// Login brute-force protection
// ---------------------------------------------------------------------------

fn check_login_rate_limit(state: &AppState, username: &str) -> Result<(), AppError> {
    let now = Instant::now();
    if let Some(tracker) = state.login_failures.get(username) {
        let elapsed = now.duration_since(tracker.window_start).as_secs();
        if elapsed < LOGIN_WINDOW_SECS && tracker.failures >= LOGIN_MAX_FAILURES {
            let retry_after = LOGIN_WINDOW_SECS - elapsed;
            return Err(AppError::RateLimited { retry_after });
        }
    }
    Ok(())
}

fn record_login_failure(state: &AppState, username: &str) {
    let now = Instant::now();
    state
        .login_failures
        .entry(username.to_string())
        .and_modify(|t| {
            let elapsed = now.duration_since(t.window_start).as_secs();
            if elapsed >= LOGIN_WINDOW_SECS {
                t.failures = 1;
                t.window_start = now;
            } else {
                t.failures += 1;
            }
        })
        .or_insert(LoginFailureTracker {
            failures: 1,
            window_start: now,
        });
}

fn clear_login_failures(state: &AppState, username: &str) {
    state.login_failures.remove(username);
}

// ---------------------------------------------------------------------------
// Register rate limiting (per IP)
// ---------------------------------------------------------------------------

fn hash_ip(ip: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ip.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn extract_request_ip(headers: &HeaderMap) -> String {
    headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("X-Real-IP")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn check_register_rate_limit(state: &AppState, ip: &str) -> Result<(), AppError> {
    let ip_hash = hash_ip(ip);
    let now = Instant::now();
    if let Some(tracker) = state.register_attempts.get(&ip_hash) {
        let elapsed = now.duration_since(tracker.window_start).as_secs();
        if elapsed < REGISTER_WINDOW_SECS && tracker.attempts >= REGISTER_MAX_ATTEMPTS {
            let retry_after = REGISTER_WINDOW_SECS - elapsed;
            return Err(AppError::RateLimited { retry_after });
        }
    }
    Ok(())
}

fn record_register_attempt(state: &AppState, ip: &str) {
    let ip_hash = hash_ip(ip);
    let now = Instant::now();
    state
        .register_attempts
        .entry(ip_hash)
        .and_modify(|t| {
            let elapsed = now.duration_since(t.window_start).as_secs();
            if elapsed >= REGISTER_WINDOW_SECS {
                t.attempts = 1;
                t.window_start = now;
            } else {
                t.attempts += 1;
            }
        })
        .or_insert(RegisterAttemptTracker {
            attempts: 1,
            window_start: now,
        });
}

// ---------------------------------------------------------------------------
// Backup code helpers
// ---------------------------------------------------------------------------

fn hash_backup_code(code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code.to_uppercase().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn generate_backup_codes() -> Vec<String> {
    let charset = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rngs::OsRng;
    (0..10)
        .map(|_| {
            (0..8)
                .map(|_| {
                    let idx = (rng.next_u32() as usize) % charset.len();
                    charset[idx] as char
                })
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

fn issue_bearer_token() -> (String, String, String) {
    let token = generate_token();
    let token_hash = create_token_hash(&token);
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(30))
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string();
    (token, token_hash, expires_at)
}

/// Verify a user's password given their user_id. Returns the stored hash for reuse.
async fn verify_user_password(
    state: &AppState,
    user_id: &str,
    password: &str,
) -> Result<(), AppError> {
    let row = sqlx::query_as::<_, (Option<String>,)>(&crate::db::q(
        "SELECT password_hash FROM users WHERE id = ?",
    ))
    .bind(user_id)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;

    let stored_hash = row
        .0
        .ok_or_else(|| AppError::BadRequest("account has no password".to_string()))?;

    let parsed_hash = PasswordHash::new(&stored_hash)
        .map_err(|e| AppError::Internal(format!("stored hash parse failed: {e}")))?;

    if Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_err()
    {
        return Err(AppError::Unauthorized("invalid password".to_string()));
    }

    Ok(())
}

// =========================================================================
// Registration
// =========================================================================

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Per-IP rate limit: max 5 registration attempts per 15 minutes
    let ip = extract_request_ip(&headers);
    check_register_rate_limit(&state, &ip)?;
    record_register_attempt(&state, &ip);

    // Check registration policy
    let policy = &state.settings.load().registration_policy;
    match policy.as_str() {
        "closed" => {
            return Err(AppError::Forbidden(
                "registration is currently closed".to_string(),
            ));
        }
        "invite_only" => {
            return Err(AppError::Forbidden(
                "registration requires an invitation".to_string(),
            ));
        }
        _ => {} // "open" or any other value allows registration
    }

    // Validate username length
    let username = input.username.trim();
    if username.is_empty() || username.len() > 32 {
        return Err(AppError::BadRequest(
            "username must be between 1 and 32 characters".to_string(),
        ));
    }

    // Validate display_name length if provided
    if let Some(ref dn) = input.display_name {
        if dn.len() > 32 {
            return Err(AppError::BadRequest(
                "display_name must not exceed 32 characters".to_string(),
            ));
        }
    }

    // Validate password length
    if input.password.len() < 8 || input.password.len() > 128 {
        return Err(AppError::BadRequest(
            "password must be between 8 and 128 characters".to_string(),
        ));
    }

    // Check for username conflict
    let existing = sqlx::query_scalar::<_, String>(&crate::db::q(
        "SELECT id FROM users WHERE username = ? AND bot = false",
    ))
    .bind(username)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;

    if existing.is_some() {
        return Err(AppError::Conflict("registration failed".to_string()));
    }

    // Hash password with Argon2id (OWASP-recommended params: 19 MiB memory, 3 iterations)
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(19456, 3, 1, None)
            .map_err(|e| AppError::Internal(format!("argon2 params failed: {e}")))?,
    );
    let password_hash = argon2
        .hash_password(input.password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("password hashing failed: {e}")))?
        .to_string();

    // Create user
    let id = snowflake::generate();
    let display_name = input.display_name.as_deref().unwrap_or(username);

    // First registered user becomes admin when no admins exist yet
    let admin_count = db::admin::count_admins(&state.db).await?;
    let is_admin = admin_count == 0;

    sqlx::query(
        &crate::db::q("INSERT INTO users (id, username, display_name, password_hash, is_admin) VALUES (?, ?, ?, ?, ?)"),
    )
    .bind(&id)
    .bind(username)
    .bind(display_name)
    .bind(&password_hash)
    .bind(is_admin)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    let user = db::users::get_user(&state.db, &id).await?;

    // Auto-join the default space (first space created on the server)
    let default_space: Option<(String,)> =
        sqlx::query_as("SELECT id FROM spaces ORDER BY created_at ASC LIMIT 1")
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::from)?;
    if let Some((space_id,)) = default_space {
        match db::members::add_member(&state.db, &space_id, &id, state.db_is_postgres).await {
            Ok(_) => {
                tracing::info!("auto-joined user {} to default space {}", id, space_id);
                // Post a system message in the welcome/system channel (if configured)
                super::system_messages::broadcast_member_join_message(&state, &space_id, &id)
                    .await;
            }
            Err(e) => {
                tracing::error!(
                    "failed to auto-join user {} to default space {}: {:?}",
                    id,
                    space_id,
                    e
                );
            }
        }

        // First registered user (server admin) becomes the owner of the default space
        // and receives the Admin role within it
        if is_admin {
            // Transfer space ownership from System user to first real user
            let now_fn = crate::db::now_sql(state.db_is_postgres);
            if let Err(e) = sqlx::query(&crate::db::q(&format!(
                "UPDATE spaces SET owner_id = ?, updated_at = {now_fn} WHERE id = ?"
            )))
            .bind(&id)
            .bind(&space_id)
            .execute(&state.db)
            .await
            {
                tracing::error!(
                    "failed to transfer default space ownership to user {}: {:?}",
                    id,
                    e
                );
            } else {
                tracing::info!(
                    "transferred default space {} ownership to first admin user {}",
                    space_id,
                    id
                );
            }

            // Assign the Admin role to the first user in the default space
            let admin_role: Option<(String,)> = sqlx::query_as(&crate::db::q(
                "SELECT id FROM roles WHERE space_id = ? AND name = 'Admin' LIMIT 1",
            ))
            .bind(&space_id)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if let Some((admin_role_id,)) = admin_role {
                if let Err(e) = db::members::add_role_to_member(
                    &state.db,
                    &space_id,
                    &id,
                    &admin_role_id,
                    state.db_is_postgres,
                )
                .await
                {
                    tracing::error!(
                        "failed to assign Admin role to user {} in default space: {:?}",
                        id,
                        e
                    );
                } else {
                    tracing::info!(
                        "assigned Admin role to first admin user {} in default space {}",
                        id,
                        space_id
                    );
                }
            }
        }

        // Broadcast member.join to the space
        if let Ok(member) = db::members::get_member_row(&state.db, &space_id, &id).await {
            if let Some(ref dispatcher) = *state.gateway_tx.read().await {
                let event = serde_json::json!({
                    "op": 0,
                    "type": "member.join",
                    "data": {
                        "space_id": space_id,
                        "user": user,
                        "joined_at": member.joined_at
                    }
                });
                let _ = dispatcher.send(GatewayBroadcast {
                    space_id: Some(space_id),
                    target_user_ids: None,
                    event,
                    intent: "members".to_string(),
                });
            }
        }
    }

    // Generate bearer token with 30-day expiry
    let (token, token_hash, expires_at) = issue_bearer_token();

    sqlx::query(&crate::db::q(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, ?)",
    ))
    .bind(&token_hash)
    .bind(&id)
    .bind(&expires_at)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Clean up expired tokens and enforce session limit
    cleanup_expired_tokens(&state.db, &id).await;
    enforce_session_limit(&state.db, &id).await;

    Ok(Json(serde_json::json!({
        "data": {
            "user": user,
            "token": token
        }
    })))
}

// =========================================================================
// Login (step 1 — password verification)
// =========================================================================

pub async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Per-username brute-force protection: max 5 failed attempts per 15 minutes
    check_login_rate_limit(&state, &input.username)?;

    // Look up user by username (must not be a bot, must have password_hash)
    let row = sqlx::query(
        &crate::db::q("SELECT id, password_hash, disabled, force_password_reset, totp_enabled FROM users WHERE username = ? AND bot = false AND password_hash IS NOT NULL"),
    )
    .bind(&input.username)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;

    let (user_id, stored_hash, disabled, force_password_reset, totp_enabled) = match row {
        Some(r) => {
            let id: String = r.get("id");
            let hash: String = r.get("password_hash");
            let dis = crate::db::get_bool(&r, "disabled");
            let fpr = crate::db::get_bool(&r, "force_password_reset");
            let totp = crate::db::get_bool(&r, "totp_enabled");
            (id, hash, dis, fpr, totp)
        }
        None => {
            // Run a dummy Argon2 verification to prevent timing-based user enumeration.
            let dummy_salt = SaltString::generate(&mut OsRng);
            let _ = Argon2::default().hash_password(b"dummy", &dummy_salt);
            // Count as a failure against this username to prevent enumeration abuse
            record_login_failure(&state, &input.username);
            return Err(AppError::Unauthorized("invalid credentials".to_string()));
        }
    };

    // Disabled users cannot log in
    if disabled {
        return Err(AppError::Forbidden("account is disabled".to_string()));
    }

    // Verify password
    let parsed_hash = PasswordHash::new(&stored_hash)
        .map_err(|e| AppError::Internal(format!("stored hash parse failed: {e}")))?;

    if Argon2::default()
        .verify_password(input.password.as_bytes(), &parsed_hash)
        .is_err()
    {
        record_login_failure(&state, &input.username);
        return Err(AppError::Unauthorized("invalid credentials".to_string()));
    }

    // Password is correct — clear any tracked failures for this username
    clear_login_failures(&state, &input.username);

    // If 2FA is enabled, issue a short-lived MFA ticket instead of a token
    if totp_enabled {
        let ticket = generate_token();
        let ticket_hash = create_token_hash(&ticket);
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);

        // Invalidate any existing MFA tickets for this user to prevent concurrent
        // brute-force via multiple independent tickets.
        state.mfa_tickets.retain(|_, v| v.user_id != user_id);

        state.mfa_tickets.insert(
            ticket_hash,
            MfaTicket {
                user_id: user_id.clone(),
                expires_at,
            },
        );

        return Ok(Json(serde_json::json!({
            "data": {
                "mfa_required": true,
                "ticket": ticket
            }
        })));
    }

    // No 2FA — issue token directly
    let user = db::users::get_user(&state.db, &user_id).await?;
    let (token, token_hash, expires_at) = issue_bearer_token();

    sqlx::query(&crate::db::q(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, ?)",
    ))
    .bind(&token_hash)
    .bind(&user_id)
    .bind(&expires_at)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    cleanup_expired_tokens(&state.db, &user_id).await;
    enforce_session_limit(&state.db, &user_id).await;

    let mut data = serde_json::json!({
        "user": user,
        "token": token
    });
    if force_password_reset {
        data["force_password_reset"] = serde_json::json!(true);
    }

    Ok(Json(serde_json::json!({ "data": data })))
}

// =========================================================================
// Login MFA (step 2 — TOTP or backup code verification)
// =========================================================================

pub async fn login_mfa(
    State(state): State<AppState>,
    Json(input): Json<MfaLoginRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Resolve the MFA ticket
    let ticket_hash = create_token_hash(&input.ticket);
    let ticket = state
        .mfa_tickets
        .remove(&ticket_hash)
        .map(|(_, t)| t)
        .ok_or_else(|| AppError::Unauthorized("invalid or expired MFA ticket".to_string()))?;

    // Check ticket expiry
    if ticket.expires_at < chrono::Utc::now() {
        return Err(AppError::Unauthorized(
            "MFA ticket has expired — please log in again".to_string(),
        ));
    }

    let user_id = &ticket.user_id;

    // Rate limit TOTP attempts
    check_totp_rate_limit(&state, user_id)?;

    let code = input.code.trim();

    // Determine if this is a TOTP code (6 digits) or a backup code (8 alphanumeric chars)
    let is_totp_code = code.len() == 6 && code.chars().all(|c| c.is_ascii_digit());

    if is_totp_code {
        // Verify TOTP code
        verify_totp_code(&state, user_id, code).await?;
    } else {
        // Try as backup code
        verify_and_consume_backup_code(&state, user_id, code).await?;
    }

    clear_totp_failures(&state, user_id);

    // Issue token
    let user = db::users::get_user(&state.db, user_id).await?;
    let (token, token_hash, expires_at) = issue_bearer_token();

    sqlx::query(&crate::db::q(
        "INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, ?)",
    ))
    .bind(&token_hash)
    .bind(user_id)
    .bind(&expires_at)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    cleanup_expired_tokens(&state.db, user_id).await;
    enforce_session_limit(&state.db, user_id).await;

    // Check force_password_reset
    let force_reset = sqlx::query(&crate::db::q(
        "SELECT force_password_reset FROM users WHERE id = ?",
    ))
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|r| crate::db::get_bool(&r, "force_password_reset"))
    .unwrap_or(false);

    let mut data = serde_json::json!({
        "user": user,
        "token": token
    });
    if force_reset {
        data["force_password_reset"] = serde_json::json!(true);
    }

    Ok(Json(serde_json::json!({ "data": data })))
}

// =========================================================================
// Logout
// =========================================================================

pub async fn logout(
    State(state): State<AppState>,
    _auth: AuthUser,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let raw_token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    let token_hash = create_token_hash(raw_token);

    sqlx::query(&crate::db::q(
        "DELETE FROM user_tokens WHERE token_hash = ?",
    ))
    .bind(&token_hash)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": { "ok": true }
    })))
}

// =========================================================================
// Revoke all sessions
// =========================================================================

pub async fn revoke_all_sessions(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    sqlx::query(&crate::db::q("DELETE FROM user_tokens WHERE user_id = ?"))
        .bind(&auth.user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": { "ok": true }
    })))
}

// =========================================================================
// Change Password (self-service)
// =========================================================================

pub async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(input): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Validate new password length
    if input.new_password.len() < 8 || input.new_password.len() > 128 {
        return Err(AppError::BadRequest(
            "password must be between 8 and 128 characters".to_string(),
        ));
    }

    // Verify old password
    verify_user_password(&state, &auth.user_id, &input.old_password).await?;

    // Hash the new password with Argon2id
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(19456, 3, 1, None)
            .map_err(|e| AppError::Internal(format!("argon2 params failed: {e}")))?,
    );
    let password_hash = argon2
        .hash_password(input.new_password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("password hashing failed: {e}")))?
        .to_string();

    // Update password and clear force_password_reset flag
    let now_fn = crate::db::now_sql(state.db_is_postgres);
    sqlx::query(&crate::db::q(&format!(
        "UPDATE users SET password_hash = ?, force_password_reset = FALSE, updated_at = {now_fn} WHERE id = ?",
    )))
    .bind(&password_hash)
    .bind(&auth.user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Revoke all other sessions (keep the current one)
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let raw_token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    let current_token_hash = create_token_hash(raw_token);

    sqlx::query(&crate::db::q(
        "DELETE FROM user_tokens WHERE user_id = ? AND token_hash != ?",
    ))
    .bind(&auth.user_id)
    .bind(&current_token_hash)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": { "ok": true }
    })))
}

// =========================================================================
// Two-Factor Authentication — Enable (step 1)
// =========================================================================

pub async fn enable_2fa(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(input): Json<Enable2faRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Require password confirmation
    verify_user_password(&state, &auth.user_id, &input.password).await?;

    // Check if 2FA is already enabled
    let already_enabled = {
        let row = sqlx::query(&crate::db::q("SELECT totp_enabled FROM users WHERE id = ?"))
            .bind(&auth.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::from)?;
        crate::db::get_bool(&row, "totp_enabled")
    };

    if already_enabled {
        return Err(AppError::BadRequest("2FA is already enabled".to_string()));
    }

    // Generate a TOTP secret (20 bytes = 160 bits, standard for TOTP)
    let mut secret_bytes = [0u8; 20];
    OsRng.fill_bytes(&mut secret_bytes);
    let secret_base32 = data_encoding::BASE32_NOPAD.encode(&secret_bytes);

    // Encrypt and store the secret (not yet enabled — user must verify first)
    let encrypted_secret = encrypt_totp_secret(&secret_base32, state.totp_key.as_ref());
    let now_fn = crate::db::now_sql(state.db_is_postgres);
    sqlx::query(&crate::db::q(&format!(
        "UPDATE users SET totp_secret = ?, updated_at = {now_fn} WHERE id = ?",
    )))
    .bind(&encrypted_secret)
    .bind(&auth.user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Fetch username for the otpauth URI
    let username: String =
        sqlx::query_scalar(&crate::db::q("SELECT username FROM users WHERE id = ?"))
            .bind(&auth.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::from)?;

    let otpauth_uri = format!(
        "otpauth://totp/Accord:{}?secret={}&issuer=Accord&algorithm=SHA1&digits=6&period=30",
        urlencoded(&username),
        secret_base32
    );

    Ok(Json(serde_json::json!({
        "data": {
            "secret": secret_base32,
            "otpauth_uri": otpauth_uri
        }
    })))
}

// =========================================================================
// Two-Factor Authentication — Verify (step 2, completes enable)
// =========================================================================

pub async fn verify_2fa(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(input): Json<Verify2faRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let code = input.code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::BadRequest(
            "code must be a 6-digit number".to_string(),
        ));
    }

    // Rate limit
    check_totp_rate_limit(&state, &auth.user_id)?;

    // Fetch the stored secret
    let confirm_row = sqlx::query(&crate::db::q(
        "SELECT totp_secret, totp_enabled FROM users WHERE id = ?",
    ))
    .bind(&auth.user_id)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;

    if crate::db::get_bool(&confirm_row, "totp_enabled") {
        return Err(AppError::BadRequest("2FA is already enabled".to_string()));
    }

    let encrypted_secret: Option<String> = confirm_row.get("totp_secret");
    let encrypted_secret = encrypted_secret.ok_or_else(|| {
        AppError::BadRequest("no 2FA setup in progress — call enable first".to_string())
    })?;

    let secret_base32 = decrypt_totp_secret(&encrypted_secret, state.totp_key.as_ref())?;

    // Verify the TOTP code
    let secret_bytes = data_encoding::BASE32_NOPAD
        .decode(secret_base32.as_bytes())
        .map_err(|_| AppError::Internal("stored secret is invalid".to_string()))?;

    let totp = totp_rs::TOTP::new(totp_rs::Algorithm::SHA1, 6, 1, 30, secret_bytes)
        .map_err(|e| AppError::Internal(format!("TOTP init failed: {e}")))?;

    if !totp
        .check_current(code)
        .map_err(|e| AppError::Internal(format!("TOTP check failed: {e}")))?
    {
        record_totp_failure(&state, &auth.user_id);
        return Err(AppError::Unauthorized("invalid code".to_string()));
    }

    clear_totp_failures(&state, &auth.user_id);

    // Enable 2FA
    let now_fn = crate::db::now_sql(state.db_is_postgres);
    sqlx::query(&crate::db::q(&format!(
        "UPDATE users SET totp_enabled = TRUE, updated_at = {now_fn} WHERE id = ?",
    )))
    .bind(&auth.user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    // Generate and store hashed backup codes
    let codes = generate_backup_codes();

    sqlx::query(&crate::db::q("DELETE FROM backup_codes WHERE user_id = ?"))
        .bind(&auth.user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    for code in &codes {
        let code_hash = hash_backup_code(code);
        sqlx::query(&crate::db::q(
            "INSERT INTO backup_codes (user_id, code_hash) VALUES (?, ?)",
        ))
        .bind(&auth.user_id)
        .bind(&code_hash)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;
    }

    Ok(Json(serde_json::json!({
        "data": {
            "backup_codes": codes
        }
    })))
}

// =========================================================================
// Two-Factor Authentication — Disable
// =========================================================================

pub async fn disable_2fa(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(input): Json<Disable2faRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Verify password
    verify_user_password(&state, &auth.user_id, &input.password).await?;

    // Disable 2FA and clear secret + backup codes
    let now_fn = crate::db::now_sql(state.db_is_postgres);
    sqlx::query(&crate::db::q(&format!(
        "UPDATE users SET totp_enabled = FALSE, totp_secret = NULL, updated_at = {now_fn} WHERE id = ?",
    )))
    .bind(&auth.user_id)
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;

    sqlx::query(&crate::db::q("DELETE FROM backup_codes WHERE user_id = ?"))
        .bind(&auth.user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "data": { "ok": true }
    })))
}

// =========================================================================
// Backup Codes — Regenerate (requires password)
// =========================================================================

pub async fn regenerate_backup_codes(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(input): Json<RegenerateBackupCodesRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Require password
    verify_user_password(&state, &auth.user_id, &input.password).await?;

    // Verify 2FA is enabled
    let enabled = {
        let row = sqlx::query(&crate::db::q("SELECT totp_enabled FROM users WHERE id = ?"))
            .bind(&auth.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::from)?;
        crate::db::get_bool(&row, "totp_enabled")
    };

    if !enabled {
        return Err(AppError::BadRequest("2FA is not enabled".to_string()));
    }

    // Regenerate backup codes (hashed)
    let codes = generate_backup_codes();

    sqlx::query(&crate::db::q("DELETE FROM backup_codes WHERE user_id = ?"))
        .bind(&auth.user_id)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

    for code in &codes {
        let code_hash = hash_backup_code(code);
        sqlx::query(&crate::db::q(
            "INSERT INTO backup_codes (user_id, code_hash) VALUES (?, ?)",
        ))
        .bind(&auth.user_id)
        .bind(&code_hash)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;
    }

    Ok(Json(serde_json::json!({
        "data": {
            "backup_codes": codes
        }
    })))
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Verify a TOTP code for a given user.
async fn verify_totp_code(state: &AppState, user_id: &str, code: &str) -> Result<(), AppError> {
    let encrypted_secret: Option<String> =
        sqlx::query_scalar(&crate::db::q("SELECT totp_secret FROM users WHERE id = ?"))
            .bind(user_id)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::from)?;

    let encrypted_secret = encrypted_secret
        .ok_or_else(|| AppError::Internal("2FA is enabled but no secret stored".to_string()))?;

    let secret_base32 = decrypt_totp_secret(&encrypted_secret, state.totp_key.as_ref())?;

    let secret_bytes = data_encoding::BASE32_NOPAD
        .decode(secret_base32.as_bytes())
        .map_err(|_| AppError::Internal("stored TOTP secret is invalid".to_string()))?;

    let totp = totp_rs::TOTP::new(totp_rs::Algorithm::SHA1, 6, 1, 30, secret_bytes)
        .map_err(|e| AppError::Internal(format!("TOTP init failed: {e}")))?;

    if !totp
        .check_current(code)
        .map_err(|e| AppError::Internal(format!("TOTP check failed: {e}")))?
    {
        record_totp_failure(state, user_id);
        return Err(AppError::Unauthorized("invalid TOTP code".to_string()));
    }

    Ok(())
}

/// Verify and consume a backup code. Returns error if invalid or already used.
async fn verify_and_consume_backup_code(
    state: &AppState,
    user_id: &str,
    code: &str,
) -> Result<(), AppError> {
    let code_hash = hash_backup_code(code);

    let row = sqlx::query(&crate::db::q(
        "SELECT id, used FROM backup_codes WHERE user_id = ? AND code_hash = ?",
    ))
    .bind(user_id)
    .bind(&code_hash)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::from)?;

    match row {
        Some(r) => {
            let id: i64 = r.get("id");
            let used = crate::db::get_bool(&r, "used");
            if used {
                record_totp_failure(state, user_id);
                return Err(AppError::Unauthorized(
                    "backup code has already been used".to_string(),
                ));
            }
            // Mark as used
            sqlx::query(&crate::db::q(
                "UPDATE backup_codes SET used = TRUE WHERE id = ?",
            ))
            .bind(id)
            .execute(&state.db)
            .await
            .map_err(AppError::from)?;
            Ok(())
        }
        None => {
            record_totp_failure(state, user_id);
            Err(AppError::Unauthorized("invalid code".to_string()))
        }
    }
}

/// Enforce maximum concurrent session limit per user.
/// Deletes the oldest tokens when the user exceeds MAX_SESSIONS_PER_USER active tokens.
async fn enforce_session_limit(pool: &sqlx::AnyPool, user_id: &str) {
    let count: i64 = sqlx::query_scalar(&crate::db::q(
        "SELECT COUNT(*) FROM user_tokens WHERE user_id = ?",
    ))
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let excess = count - MAX_SESSIONS_PER_USER;
    if excess > 0 {
        let _ = sqlx::query(&crate::db::q(
            "DELETE FROM user_tokens WHERE user_id = ? AND token_hash IN \
             (SELECT token_hash FROM user_tokens WHERE user_id = ? \
              ORDER BY created_at ASC LIMIT ?)",
        ))
        .bind(user_id)
        .bind(user_id)
        .bind(excess)
        .execute(pool)
        .await;
    }
}

/// Delete expired tokens for a user (background cleanup).
async fn cleanup_expired_tokens(pool: &sqlx::AnyPool, user_id: &str) {
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string();
    let _ = sqlx::query(&crate::db::q(
        "DELETE FROM user_tokens WHERE user_id = ? AND expires_at < ?",
    ))
    .bind(user_id)
    .bind(&now)
    .execute(pool)
    .await;
}

// =========================================================================
// Guest token (anonymous read-only access)
// =========================================================================

pub async fn guest(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    // Per-IP rate limit: max 10 guest tokens per hour
    let ip = extract_request_ip(&headers);
    check_guest_rate_limit(&state, &ip)?;
    record_guest_attempt(&state, &ip);

    // Find the default space (first public space, or the first space on the server)
    let space = find_guest_space(&state.db).await?;

    // Generate a short-lived guest token
    let token = generate_token();
    let token_hash = create_token_hash(&token);
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(GUEST_TOKEN_LIFETIME_SECS))
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string();

    // Store the guest token
    sqlx::query(&crate::db::q(
        "INSERT INTO guest_tokens (token_hash, space_id, expires_at) VALUES (?, ?, ?)",
    ))
    .bind(&token_hash)
    .bind(&space.id)
    .bind(&expires_at)
    .execute(&state.db)
    .await?;

    // Clean up expired guest tokens (best-effort)
    let now_fn = crate::db::now_sql(state.db_is_postgres);
    let _ = sqlx::query(&crate::db::q(&format!(
        "DELETE FROM guest_tokens WHERE expires_at < {now_fn}"
    )))
    .execute(&state.db)
    .await;

    // Increment guest count for the space
    state
        .guest_counts
        .entry(space.id.clone())
        .and_modify(|c| *c += 1)
        .or_insert(1);

    Ok(Json(serde_json::json!({
        "token": token,
        "expires_at": expires_at,
        "space_id": space.id
    })))
}

/// Find the space that guest tokens should be scoped to.
/// Prefers the first public space; falls back to the first space on the server.
async fn find_guest_space(
    pool: &sqlx::AnyPool,
) -> Result<crate::models::space::SpaceRow, AppError> {
    // Try public spaces first
    let public = sqlx::query_as::<_, (String,)>(&crate::db::q(
        "SELECT id FROM spaces WHERE public = true ORDER BY created_at LIMIT 1",
    ))
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = public {
        return db::spaces::get_space_row(pool, &id).await;
    }

    // Fall back to any space
    let any = sqlx::query_as::<_, (String,)>(&crate::db::q(
        "SELECT id FROM spaces ORDER BY created_at LIMIT 1",
    ))
    .fetch_optional(pool)
    .await?;

    match any {
        Some((id,)) => db::spaces::get_space_row(pool, &id).await,
        None => Err(AppError::NotFound(
            "no spaces available for guest access".into(),
        )),
    }
}

fn check_guest_rate_limit(state: &AppState, ip: &str) -> Result<(), AppError> {
    let ip_hash = hash_ip(ip);
    let now = Instant::now();
    if let Some(tracker) = state.guest_attempts.get(&ip_hash) {
        let elapsed = now.duration_since(tracker.window_start).as_secs();
        if elapsed < GUEST_WINDOW_SECS && tracker.attempts >= GUEST_MAX_ATTEMPTS {
            let retry_after = GUEST_WINDOW_SECS - elapsed;
            return Err(AppError::RateLimited { retry_after });
        }
    }
    Ok(())
}

fn record_guest_attempt(state: &AppState, ip: &str) {
    let ip_hash = hash_ip(ip);
    let now = Instant::now();
    state
        .guest_attempts
        .entry(ip_hash)
        .and_modify(|t| {
            let elapsed = now.duration_since(t.window_start).as_secs();
            if elapsed >= GUEST_WINDOW_SECS {
                t.attempts = 1;
                t.window_start = now;
            } else {
                t.attempts += 1;
            }
        })
        .or_insert(GuestAttemptTracker {
            attempts: 1,
            window_start: now,
        });
}

/// Minimal percent-encoding for otpauth URI values.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}
