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
    let row = sqlx::query_as::<_, (String, bool)>(
        "SELECT bt.user_id, u.is_admin FROM bot_tokens bt JOIN users u ON bt.user_id = u.id WHERE bt.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    Some(AuthUser {
        user_id: row.0,
        is_bot: true,
        is_admin: row.1,
    })
}

async fn resolve_bearer_token(pool: &SqlitePool, token: &str) -> Option<AuthUser> {
    let token_hash = hash_token(token);
    let row = sqlx::query_as::<_, (String, String, bool)>(
        "SELECT ut.user_id, ut.expires_at, u.is_admin FROM user_tokens ut JOIN users u ON ut.user_id = u.id WHERE ut.token_hash = ?",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .ok()??;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    if row.1 < now {
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

/// Generate a random token string.
pub fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let random: u64 = rand::random();
    format!("{ts:x}.{random:x}")
}
