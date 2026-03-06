use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_permission;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateReportBody {
    pub target_type: String,
    pub target_id: String,
    pub channel_id: Option<String>,
    pub category: String,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub struct ListReportsQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub before: Option<String>,
}

#[derive(Deserialize)]
pub struct ResolveReportBody {
    pub status: String,
    pub action_taken: Option<String>,
}

const VALID_CATEGORIES: &[&str] = &[
    "csam",
    "terrorism",
    "fraud",
    "hate",
    "violence",
    "self_harm",
    "other",
];

pub async fn create_report(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(body): Json<CreateReportBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !VALID_CATEGORIES.contains(&body.category.as_str()) {
        return Err(AppError::BadRequest(format!(
            "invalid category: {}",
            body.category
        )));
    }
    if body.target_type != "message" && body.target_type != "user" {
        return Err(AppError::BadRequest(
            "target_type must be 'message' or 'user'".into(),
        ));
    }

    // Verify user is a member of the space
    db::members::get_member_row(&state.db, &space_id, &auth.user_id)
        .await
        .map_err(|_| AppError::Forbidden("you must be a member of this space".into()))?;

    let report = db::reports::create_report(
        &state.db,
        &space_id,
        &auth.user_id,
        &body.target_type,
        &body.target_id,
        body.channel_id.as_deref(),
        &body.category,
        body.description.as_deref(),
    )
    .await?;

    let json = report_to_json(&report);

    // Broadcast to gateway (moderation intent)
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": "report.create",
            "data": json
        });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id),
            target_user_ids: None,
            event,
            intent: "moderation".to_string(),
        });
    }

    Ok(Json(serde_json::json!({ "data": json })))
}

pub async fn list_reports(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Query(query): Query<ListReportsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "moderate_members").await?;
    let limit = query.limit.unwrap_or(25).min(100);
    let reports = db::reports::list_reports(
        &state.db,
        &space_id,
        query.status.as_deref(),
        limit,
        query.before.as_deref(),
    )
    .await?;
    let data: Vec<serde_json::Value> = reports.iter().map(report_to_json).collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

pub async fn get_report(
    state: State<AppState>,
    Path((space_id, report_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "moderate_members").await?;
    let report = db::reports::get_report(&state.db, &report_id).await?;
    if report.space_id != space_id {
        return Err(AppError::NotFound("report not found".to_string()));
    }
    Ok(Json(serde_json::json!({ "data": report_to_json(&report) })))
}

pub async fn resolve_report(
    state: State<AppState>,
    Path((space_id, report_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(body): Json<ResolveReportBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "moderate_members").await?;

    if body.status != "actioned" && body.status != "dismissed" {
        return Err(AppError::BadRequest(
            "status must be 'actioned' or 'dismissed'".into(),
        ));
    }

    // Verify report belongs to this space
    let existing = db::reports::get_report(&state.db, &report_id).await?;
    if existing.space_id != space_id {
        return Err(AppError::NotFound("report not found".to_string()));
    }

    let report = db::reports::resolve_report(
        &state.db,
        &report_id,
        &auth.user_id,
        &body.status,
        body.action_taken.as_deref(),
    )
    .await?;

    Ok(Json(serde_json::json!({ "data": report_to_json(&report) })))
}

fn report_to_json(r: &db::reports::ReportRow) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "space_id": r.space_id,
        "reporter_id": r.reporter_id,
        "target_type": r.target_type,
        "target_id": r.target_id,
        "channel_id": r.channel_id,
        "category": r.category,
        "description": r.description,
        "status": r.status,
        "actioned_by": r.actioned_by,
        "action_taken": r.action_taken,
        "created_at": r.created_at,
        "resolved_at": r.resolved_at,
    })
}
