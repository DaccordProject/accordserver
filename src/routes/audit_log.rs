use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_permission;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AuditLogQuery {
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list_audit_log(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Query(params): Query<AuditLogQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "view_audit_log").await?;
    let limit = params.limit.unwrap_or(50).min(100).max(1);
    let entries = db::audit_log::list(&state.db, &space_id, params.before, limit).await?;
    let data: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "space_id": e.space_id,
                "actor_id": e.actor_id,
                "action": e.action,
                "target_type": e.target_type,
                "target_id": e.target_id,
                "metadata": serde_json::from_str::<serde_json::Value>(&e.metadata)
                    .unwrap_or(serde_json::Value::Null),
                "created_at": e.created_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": data })))
}
