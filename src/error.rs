use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    Database(sqlx::Error),
    Internal(String),
    BadRequest(String),
    NotFound(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    PayloadTooLarge(String),
    RateLimited { retry_after: u64 },
}

impl AppError {
    fn code(&self) -> &'static str {
        match self {
            AppError::Database(_) => "internal_error",
            AppError::Internal(_) => "internal_error",
            AppError::BadRequest(_) => "invalid_request",
            AppError::NotFound(_) => "not_found",
            AppError::Unauthorized(_) => "unauthorized",
            AppError::Forbidden(_) => "forbidden",
            AppError::Conflict(_) => "already_exists",
            AppError::PayloadTooLarge(_) => "payload_too_large",
            AppError::RateLimited { .. } => "rate_limited",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            AppError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        }
    }

    fn message(&self) -> String {
        match self {
            AppError::Database(e) => {
                tracing::error!("database error: {e}");
                "internal database error".to_string()
            }
            AppError::Internal(e) => {
                tracing::error!("internal error: {e}");
                "internal server error".to_string()
            }
            AppError::BadRequest(msg) => msg.clone(),
            AppError::NotFound(msg) => msg.clone(),
            AppError::Unauthorized(msg) => msg.clone(),
            AppError::Forbidden(msg) => msg.clone(),
            AppError::Conflict(msg) => msg.clone(),
            AppError::PayloadTooLarge(msg) => msg.clone(),
            AppError::RateLimited { retry_after } => {
                format!("rate limited, retry after {retry_after}s")
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = json!({
            "error": {
                "code": self.code(),
                "message": self.message()
            }
        });

        let mut response = (status, Json(body)).into_response();
        if let AppError::RateLimited { retry_after } = &self {
            response
                .headers_mut()
                .insert("Retry-After", retry_after.to_string().parse().unwrap());
        }
        response
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => AppError::NotFound("resource not found".to_string()),
            _ => AppError::Database(e),
        }
    }
}
