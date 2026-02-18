use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_server_admin;
use crate::state::{AppState, SfuNode};

#[derive(serde::Deserialize)]
pub struct RegisterNodeRequest {
    pub id: String,
    pub endpoint: String,
    pub region: String,
    pub capacity: i64,
}

#[derive(serde::Deserialize)]
pub struct HeartbeatRequest {
    pub current_load: i64,
}

pub async fn register_node(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(input): Json<RegisterNodeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    let node = db::sfu::upsert_node(
        &state.db,
        &input.id,
        &input.endpoint,
        &input.region,
        input.capacity,
    )
    .await?;

    state.sfu_nodes.insert(
        node.id.clone(),
        SfuNode {
            id: node.id.clone(),
            endpoint: node.endpoint.clone(),
            region: node.region.clone(),
            capacity: node.capacity,
            current_load: node.current_load,
            status: node.status.clone(),
        },
    );

    Ok(Json(serde_json::json!({
        "data": {
            "id": node.id,
            "endpoint": node.endpoint,
            "region": node.region,
            "capacity": node.capacity,
            "current_load": node.current_load,
            "status": node.status
        }
    })))
}

pub async fn heartbeat(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<HeartbeatRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    db::sfu::heartbeat_node(&state.db, &node_id, input.current_load).await?;

    if let Some(mut entry) = state.sfu_nodes.get_mut(&node_id) {
        entry.current_load = input.current_load;
    }

    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

pub async fn deregister_node(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    db::sfu::deregister_node(&state.db, &node_id).await?;
    state.sfu_nodes.remove(&node_id);

    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

pub async fn list_nodes(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;
    let nodes: Vec<serde_json::Value> = state
        .sfu_nodes
        .iter()
        .map(|entry| {
            let node = entry.value();
            serde_json::json!({
                "id": node.id,
                "endpoint": node.endpoint,
                "region": node.region,
                "capacity": node.capacity,
                "current_load": node.current_load,
                "status": node.status
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "data": nodes })))
}
