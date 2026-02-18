use axum::extract::State;
use axum::Json;

use crate::state::AppState;

pub async fn get_gateway() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": {
            "url": "wss://gateway.accord.local/?v=1&encoding=json"
        }
    }))
}

pub async fn get_gateway_bot(_state: State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "data": {
            "url": "wss://gateway.accord.local/?v=1&encoding=json",
            "shards": 1,
            "session_start_limit": {
                "total": 1000,
                "remaining": 999,
                "reset_after": 14400000,
                "max_concurrency": 1
            }
        }
    }))
}
