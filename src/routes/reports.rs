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

/// The canonical report categories, in the order clients should offer them:
/// everyday moderation reasons first, severe/legal ones after, `other` last.
///
/// This is the single source of truth for the allowlist — it is enforced in
/// `create_report`, mirrored by the `reports.category` CHECK constraint, and
/// served to clients by `GET /api/v1/reports/categories` so the two cannot
/// drift apart.
pub const REPORT_CATEGORIES: &[(&str, &str)] = &[
    ("spam", "Spam"),
    ("harassment", "Harassment or bullying"),
    ("hate", "Hate speech"),
    ("nsfw", "Inappropriate content"),
    ("violence", "Violence or threats"),
    ("self_harm", "Self-harm or suicide"),
    ("csam", "Child sexual abuse material"),
    ("terrorism", "Terrorism or violent extremism"),
    ("fraud", "Fraud or scam"),
    ("other", "Other"),
];

/// Accepted spellings that are not the canonical value. Kept so older clients
/// keep working after a rename instead of hard-failing on every report.
const CATEGORY_ALIASES: &[(&str, &str)] = &[("hate_speech", "hate")];

/// Map a client-supplied category onto its canonical value, or `None` if it is
/// not a category this server accepts.
fn canonical_category(input: &str) -> Option<&'static str> {
    if let Some((canonical, _)) = REPORT_CATEGORIES.iter().find(|(v, _)| *v == input) {
        return Some(canonical);
    }
    CATEGORY_ALIASES
        .iter()
        .find(|(alias, _)| *alias == input)
        .map(|(_, canonical)| *canonical)
}

/// Public list of the report categories this server accepts, with display
/// labels. Unauthenticated so clients can populate the report dialog before a
/// space is even open.
pub async fn list_report_categories() -> Json<serde_json::Value> {
    let data: Vec<serde_json::Value> = REPORT_CATEGORIES
        .iter()
        .map(|(value, label)| serde_json::json!({ "value": value, "label": label }))
        .collect();
    Json(serde_json::json!({ "data": data }))
}

pub async fn create_report(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(body): Json<CreateReportBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let category = canonical_category(&body.category)
        .ok_or_else(|| AppError::BadRequest(format!("invalid category: {}", body.category)))?;
    if body.target_type != "message" && body.target_type != "user" {
        return Err(AppError::BadRequest(
            "target_type must be 'message' or 'user'".into(),
        ));
    }

    if let Some(ref desc) = body.description {
        if desc.len() > 4000 {
            return Err(AppError::BadRequest(
                "description must not exceed 4000 characters".to_string(),
            ));
        }
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
        category,
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
        state.db_is_postgres,
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
