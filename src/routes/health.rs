use axum::Json;

pub async fn health() -> &'static str {
    "ok"
}

pub async fn version() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": env!("GIT_SHA"),
    }))
}
