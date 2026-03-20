use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_permission;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListAuditLogQuery {
    pub action_type: Option<String>,
    pub user_id: Option<String>,
    pub before: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_audit_log(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Query(query): Query<ListAuditLogQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "view_audit_log").await?;
    let limit = query.limit.unwrap_or(25).min(100);
    let entries = db::audit_log::list_entries(
        &state.db,
        &space_id,
        query.action_type.as_deref(),
        query.user_id.as_deref(),
        query.before.as_deref(),
        limit,
    )
    .await?;
    let data: Vec<serde_json::Value> = entries.iter().map(entry_to_json).collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

/// Broadcast an audit_log.create gateway event for the given entry.
pub async fn broadcast_entry(state: &AppState, entry: &db::audit_log::AuditLogRow) {
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let data = entry_to_json(entry);
        let event = serde_json::json!({
            "op": 0,
            "type": "audit_log.create",
            "data": data
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: Some(entry.space_id.clone()),
            target_user_ids: None,
            event,
            intent: "moderation".to_string(),
        });
    }
}

fn entry_to_json(e: &db::audit_log::AuditLogRow) -> serde_json::Value {
    let changes = e
        .changes
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    serde_json::json!({
        "id": e.id,
        "space_id": e.space_id,
        "user_id": e.user_id,
        "action_type": e.action_type,
        "target_id": e.target_id,
        "target_type": e.target_type,
        "reason": e.reason,
        "changes": changes,
        "created_at": e.created_at,
    })
}
