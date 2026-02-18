use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_hierarchy, require_permission};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateBanBody {
    pub reason: Option<String>,
}

pub async fn list_bans(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    let bans = db::bans::list_bans(&state.db, &space_id).await?;
    let data: Vec<serde_json::Value> = bans
        .iter()
        .map(|b| {
            serde_json::json!({
                "user_id": b.user_id,
                "space_id": b.space_id,
                "reason": b.reason,
                "banned_by": b.banned_by,
                "created_at": b.created_at
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

pub async fn get_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    let ban = db::bans::get_ban(&state.db, &space_id, &user_id).await?;
    Ok(Json(serde_json::json!({
        "data": {
            "user_id": ban.user_id,
            "space_id": ban.space_id,
            "reason": ban.reason,
            "banned_by": ban.banned_by,
            "created_at": ban.created_at
        }
    })))
}

pub async fn create_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
    body: Option<Json<CreateBanBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    require_hierarchy(&state.db, &space_id, &auth.user_id, &user_id).await?;
    let reason = body.and_then(|b| b.reason.clone());
    let ban = db::bans::create_ban(
        &state.db,
        &space_id,
        &user_id,
        reason.as_deref(),
        &auth.user_id,
    )
    .await?;
    Ok(Json(serde_json::json!({
        "data": {
            "user_id": ban.user_id,
            "space_id": ban.space_id,
            "reason": ban.reason,
            "banned_by": ban.banned_by,
            "created_at": ban.created_at
        }
    })))
}

pub async fn delete_ban(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "ban_members").await?;
    db::bans::delete_ban(&state.db, &space_id, &user_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}
