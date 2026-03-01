use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::require_server_admin;
use crate::models::space::AdminUpdateSpace;
use crate::models::user::AdminUpdateUser;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AdminListQuery {
    pub after: Option<String>,
    pub limit: Option<i64>,
    pub search: Option<String>,
}

// =========================================================================
// Spaces
// =========================================================================

pub async fn list_spaces(
    state: State<AppState>,
    auth: AuthUser,
    Query(params): Query<AdminListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    let limit = params.limit.unwrap_or(50).min(1000);
    let mut rows = db::admin::list_all_spaces(
        &state.db,
        params.after.as_deref(),
        limit,
        params.search.as_deref(),
    )
    .await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let last_id = rows.last().map(|s| s.id.clone());
    let mut response = serde_json::json!({ "data": rows });
    if has_more {
        response["cursor"] = serde_json::json!({
            "after": last_id.unwrap_or_default(),
            "has_more": has_more
        });
    }
    Ok(Json(response))
}

pub async fn update_space(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<AdminUpdateSpace>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Verify the space exists
    db::spaces::get_space_row(&state.db, &space_id).await?;

    // If transferring ownership, verify the target user exists
    if let Some(ref owner_id) = input.owner_id {
        db::users::get_user(&state.db, owner_id).await?;
    }

    db::admin::admin_update_space(&state.db, &space_id, &input).await?;

    let space = db::spaces::get_space_row(&state.db, &space_id).await?;
    Ok(Json(serde_json::json!({ "data": space })))
}

// =========================================================================
// Users
// =========================================================================

pub async fn list_users(
    state: State<AppState>,
    auth: AuthUser,
    Query(params): Query<AdminListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    let limit = params.limit.unwrap_or(50).min(1000);
    let mut rows = db::admin::list_all_users(
        &state.db,
        params.after.as_deref(),
        limit,
        params.search.as_deref(),
    )
    .await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let last_id = rows
        .last()
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut response = serde_json::json!({ "data": rows });
    if has_more {
        response["cursor"] = serde_json::json!({
            "after": last_id.unwrap_or_default(),
            "has_more": has_more
        });
    }
    Ok(Json(response))
}

pub async fn update_user(
    state: State<AppState>,
    Path(user_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<AdminUpdateUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Verify target user exists
    let target = db::users::get_user(&state.db, &user_id).await?;

    // Self-demotion protection: can't remove your own admin flag
    if auth.user_id == user_id {
        if let Some(false) = input.is_admin {
            return Err(AppError::BadRequest(
                "cannot remove your own admin privileges".to_string(),
            ));
        }
    }

    // Last-admin protection: if removing admin from someone, ensure at least one admin remains
    if let Some(false) = input.is_admin {
        if target.is_admin {
            let admin_count = db::admin::count_admins(&state.db).await?;
            if admin_count <= 1 {
                return Err(AppError::BadRequest(
                    "cannot remove the last server admin".to_string(),
                ));
            }
        }
    }

    let user = db::admin::admin_update_user(&state.db, &user_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": user })))
}

pub async fn delete_user(
    state: State<AppState>,
    Path(user_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_server_admin(&auth)?;

    // Can't delete yourself
    if auth.user_id == user_id {
        return Err(AppError::BadRequest(
            "cannot delete your own account via admin API".to_string(),
        ));
    }

    // Can't delete another admin (must remove flag first)
    let target = db::users::get_user(&state.db, &user_id).await?;
    if target.is_admin {
        return Err(AppError::BadRequest(
            "cannot delete an admin user — remove admin flag first".to_string(),
        ));
    }

    db::admin::delete_user(&state.db, &user_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}
