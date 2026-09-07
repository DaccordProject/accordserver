use axum::extract::{FromRequestParts, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::time::Instant;

use crate::error::AppError;
use crate::state::{AppState, RateLimitBucket};

/// Maximum requests per window.
const RATE_LIMIT: u32 = 60;
/// Burst allowance on top of the base rate.
const BURST: u32 = 10;
/// Total bucket capacity (base + burst).
const CAPACITY: u32 = RATE_LIMIT + BURST;
/// Window duration in seconds — tokens refill fully after this period.
const WINDOW_SECS: u64 = 60;

/// Token-bucket rate limiter keyed by auth header hash or remote IP.
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = req.into_parts();
    // Only verified tokens get a user bucket. Random tokens cannot buy capacity.
    let auth = crate::middleware::auth::AuthUser::from_request_parts(&mut parts, &state)
        .await
        .ok();
    let key = match auth {
        Some(auth) if !auth.is_guest => format!("user:{}", auth.user_id),
        _ => format!("ip:{}", crate::middleware::client_ip::request_ip(&parts)),
    };
    let req = Request::from_parts(parts, body);

    let now = Instant::now();
    // Bound stale bucket growth without scanning on every request.
    if state.rate_limits.len() >= 10_000 {
        state
            .rate_limits
            .retain(|_, bucket| now.duration_since(bucket.last_refill).as_secs() < WINDOW_SECS);
        if state.rate_limits.len() >= 10_000 && !state.rate_limits.contains_key(&key) {
            return AppError::RateLimited {
                retry_after: WINDOW_SECS,
            }
            .into_response();
        }
    }

    let (remaining, retry_after) = {
        let mut entry = state
            .rate_limits
            .entry(key)
            .or_insert_with(|| RateLimitBucket {
                remaining: CAPACITY,
                last_refill: now,
            });

        let bucket = entry.value_mut();

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs();
        if elapsed >= WINDOW_SECS {
            bucket.remaining = CAPACITY;
            bucket.last_refill = now;
        } else if elapsed > 0 {
            let refill = ((elapsed as f64 / WINDOW_SECS as f64) * CAPACITY as f64) as u32;
            bucket.remaining = (bucket.remaining + refill).min(CAPACITY);
            bucket.last_refill = now;
        }

        if bucket.remaining == 0 {
            let secs_until_refill =
                WINDOW_SECS.saturating_sub(now.duration_since(bucket.last_refill).as_secs());
            (0u32, Some(secs_until_refill.max(1)))
        } else {
            bucket.remaining -= 1;
            (bucket.remaining, None)
        }
    };

    if let Some(retry_after) = retry_after {
        return AppError::RateLimited { retry_after }.into_response();
    }

    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert("X-RateLimit-Limit", CAPACITY.to_string().parse().unwrap());
    headers.insert(
        "X-RateLimit-Remaining",
        remaining.to_string().parse().unwrap(),
    );
    // Reset timestamp: seconds until next full refill
    let reset = chrono::Utc::now().timestamp() + WINDOW_SECS as i64;
    headers.insert("X-RateLimit-Reset", reset.to_string().parse().unwrap());
    response
}
