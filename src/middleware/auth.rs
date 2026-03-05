use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub is_bot: bool,
    pub is_admin: bool,
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

async fn resolve_bot_token(pool: &SqlitePool, token: &str) -> Option<AuthUser> {
    let token_hash = hash_token(token);
    let row = sqlx::query_as::<_, (String, bool, bool)>(
        "SELECT bt.user_id, u.is_admin, u.disabled FROM bot_tokens bt JOIN users u ON bt.user_id = u.id WHERE bt.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    // Disabled users cannot authenticate
    if row.2 {
        return None;
    }

    Some(AuthUser {
        user_id: row.0,
        is_bot: true,
        is_admin: row.1,
    })
}

async fn resolve_bearer_token(pool: &SqlitePool, token: &str) -> Option<AuthUser> {
    let token_hash = hash_token(token);
    let row = sqlx::query_as::<_, (String, String, bool, bool)>(
        "SELECT ut.user_id, ut.expires_at, u.is_admin, u.disabled FROM user_tokens ut JOIN users u ON ut.user_id = u.id WHERE ut.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    // Parse expiry with chrono instead of string comparison
    let expires_at = chrono::NaiveDateTime::parse_from_str(&row.1, "%Y-%m-%dT%H:%M:%S")
        .ok()?;
    let expires_utc = expires_at.and_utc();
    if expires_utc < chrono::Utc::now() {
        return None;
    }

    // Disabled users cannot authenticate
    if row.3 {
        return None;
    }

    Some(AuthUser {
        user_id: row.0,
        is_bot: false,
        is_admin: row.2,
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
                    resolve_bearer_token(&pool, &header[7..]).await
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
                    resolve_bearer_token(&pool, &header[7..]).await
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
