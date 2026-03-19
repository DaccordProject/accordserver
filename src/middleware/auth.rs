use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::AnyPool;

use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub is_bot: bool,
    pub is_admin: bool,
    /// True when the token is a guest token (anonymous read-only access).
    pub is_guest: bool,
    /// Space ID the guest token is scoped to (only set for guest tokens).
    pub guest_space_id: Option<String>,
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

async fn resolve_bot_token(pool: &AnyPool, token: &str) -> Option<AuthUser> {
    let token_hash = hash_token(token);
    let row = sqlx::query(
        &crate::db::q("SELECT bt.user_id, u.is_admin, u.disabled FROM bot_tokens bt JOIN users u ON bt.user_id = u.id WHERE bt.token_hash = ?"),
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    use sqlx::Row;
    let user_id: String = row.get("user_id");
    let is_admin = crate::db::get_bool(&row, "is_admin");
    let disabled = crate::db::get_bool(&row, "disabled");

    // Disabled users cannot authenticate
    if disabled {
        return None;
    }

    Some(AuthUser {
        user_id,
        is_bot: true,
        is_admin,
        is_guest: false,
        guest_space_id: None,
    })
}

async fn resolve_bearer_token(pool: &AnyPool, token: &str) -> Option<AuthUser> {
    let token_hash = hash_token(token);
    let row = sqlx::query(
        &crate::db::q("SELECT ut.user_id, ut.expires_at, u.is_admin, u.disabled FROM user_tokens ut JOIN users u ON ut.user_id = u.id WHERE ut.token_hash = ?"),
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    use sqlx::Row;
    let user_id: String = row.get("user_id");
    let expires_at: String = row.get("expires_at");
    let is_admin = crate::db::get_bool(&row, "is_admin");
    let disabled = crate::db::get_bool(&row, "disabled");

    // Parse expiry — handle both SQLite (NaiveDateTime) and Postgres (with timezone offset) formats
    let expires_utc = chrono::DateTime::parse_from_str(&expires_at, "%Y-%m-%dT%H:%M:%S%z")
        .map(|dt| dt.to_utc())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(&expires_at, "%Y-%m-%dT%H:%M:%S")
                .map(|dt| dt.and_utc())
        })
        .or_else(|_| {
            // Postgres may also return "2025-01-01 00:00:00+00" (space-separated)
            chrono::DateTime::parse_from_str(&expires_at, "%Y-%m-%d %H:%M:%S%z")
                .map(|dt| dt.to_utc())
        })
        .ok()?;
    if expires_utc < chrono::Utc::now() {
        return None;
    }

    // Disabled users cannot authenticate
    if disabled {
        return None;
    }

    Some(AuthUser {
        user_id,
        is_bot: false,
        is_admin,
        is_guest: false,
        guest_space_id: None,
    })
}

async fn resolve_guest_token(pool: &AnyPool, token: &str) -> Option<AuthUser> {
    let token_hash = hash_token(token);
    let row = sqlx::query(
        &crate::db::q("SELECT space_id, expires_at FROM guest_tokens WHERE token_hash = ?"),
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    use sqlx::Row;
    let space_id: String = row.get("space_id");
    let expires_at: String = row.get("expires_at");

    // Parse expiry — handle both SQLite and Postgres datetime formats
    let expires_utc = chrono::DateTime::parse_from_str(&expires_at, "%Y-%m-%dT%H:%M:%S%z")
        .map(|dt| dt.to_utc())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(&expires_at, "%Y-%m-%dT%H:%M:%S")
                .map(|dt| dt.and_utc())
        })
        .or_else(|_| {
            chrono::DateTime::parse_from_str(&expires_at, "%Y-%m-%d %H:%M:%S%z")
                .map(|dt| dt.to_utc())
        })
        .ok()?;
    if expires_utc < chrono::Utc::now() {
        return None;
    }

    // Guest tokens use a synthetic user_id derived from the token hash
    // (no real user account exists)
    let guest_user_id = format!("guest:{}", &token_hash[..16]);

    Some(AuthUser {
        user_id: guest_user_id,
        is_bot: false,
        is_admin: false,
        is_guest: true,
        guest_space_id: Some(space_id),
    })
}

/// Rejection type for when auth fails.
pub struct AuthRejection;

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        let body = json!({
            "error": {
                "code": "unauthorized",
                "message": "invalid or missing authentication"
            }
        });
        (StatusCode::UNAUTHORIZED, Json(body)).into_response()
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AuthRejection;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let pool = state.db.clone();
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        async move {
            let auth_user = match auth_header {
                Some(header) if header.starts_with("Bot ") => {
                    resolve_bot_token(&pool, &header[4..]).await
                }
                Some(header) if header.starts_with("Bearer ") => {
                    let token = &header[7..];
                    // Try regular bearer token first, fall back to guest token
                    let user = resolve_bearer_token(&pool, token).await;
                    if user.is_some() {
                        user
                    } else {
                        resolve_guest_token(&pool, token).await
                    }
                }
                _ => None,
            };

            auth_user.ok_or(AuthRejection)
        }
    }
}

/// Optional auth extractor. Returns `Some(AuthUser)` if valid auth is present,
/// `None` if no auth header is provided. Never rejects.
pub struct OptionalAuthUser(pub Option<AuthUser>);

impl FromRequestParts<AppState> for OptionalAuthUser {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let pool = state.db.clone();
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        async move {
            let auth_user = match auth_header {
                Some(header) if header.starts_with("Bot ") => {
                    resolve_bot_token(&pool, &header[4..]).await
                }
                Some(header) if header.starts_with("Bearer ") => {
                    let token = &header[7..];
                    let user = resolve_bearer_token(&pool, token).await;
                    if user.is_some() {
                        user
                    } else {
                        resolve_guest_token(&pool, token).await
                    }
                }
                _ => None,
            };

            Ok(OptionalAuthUser(auth_user))
        }
    }
}

/// Helper to create a token hash for token creation.
pub fn create_token_hash(token: &str) -> String {
    hash_token(token)
}

/// Generate a cryptographically secure random token string (256 bits of entropy).
pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(&bytes)
}

/// Hex-encode helper (avoids adding the `hex` crate — uses the same format as before).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
