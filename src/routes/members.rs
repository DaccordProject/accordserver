use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{
    require_hierarchy, require_membership, require_permission, require_role_hierarchy,
};
use crate::models::member::{MemberRow, UpdateMember};
use crate::state::AppState;
use crate::storage;

#[derive(Deserialize)]
pub struct ListMembersQuery {
    pub after: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SearchMembersQuery {
    pub query: String,
    pub limit: Option<i64>,
}

pub async fn list_members(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Query(params): Query<ListMembersQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let limit = params.limit.unwrap_or(50).min(1000);
    let mut rows =
        db::members::list_members(&state.db, &space_id, params.after.as_deref(), limit).await?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let mut members = Vec::new();
    for row in &rows {
        let role_ids = db::members::get_member_role_ids(&state.db, &space_id, &row.user_id).await?;
        members.push(member_row_to_json(row, &role_ids));
    }

    let last_id = rows.last().map(|m| m.user_id.clone());
    let mut response = serde_json::json!({ "data": members });
    if has_more {
        response["cursor"] = serde_json::json!({
            "after": last_id.unwrap_or_default(),
            "has_more": has_more
        });
    }
    Ok(Json(response))
}

pub async fn search_members(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Query(params): Query<SearchMembersQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let limit = params.limit.unwrap_or(25).min(100);
    let rows = db::members::search_members(&state.db, &space_id, &params.query, limit).await?;

    let mut members = Vec::new();
    for row in &rows {
        let role_ids = db::members::get_member_role_ids(&state.db, &space_id, &row.user_id).await?;
        members.push(member_row_to_json(row, &role_ids));
    }

    Ok(Json(serde_json::json!({ "data": members })))
}

pub async fn get_member(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let row = db::members::get_member_row(&state.db, &space_id, &user_id).await?;
    let role_ids = db::members::get_member_role_ids(&state.db, &space_id, &user_id).await?;
    Ok(Json(
        serde_json::json!({ "data": member_row_to_json(&row, &role_ids) }),
    ))
}

pub async fn update_member(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(mut input): Json<UpdateMember>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Nickname changes require manage_nicknames
    if input.nickname.is_some() {
        require_permission(&state.db, &space_id, &auth, "manage_nicknames").await?;
    }

    // Avatar changes on other members require manage_nicknames
    if input.avatar.is_some() {
        require_permission(&state.db, &space_id, &auth, "manage_nicknames").await?;
    }

    // Role changes require manage_roles + hierarchy checks
    if let Some(ref roles) = input.roles {
        require_permission(&state.db, &space_id, &auth, "manage_roles").await?;
        require_hierarchy(&state.db, &space_id, &auth.user_id, &user_id).await?;
        // Verify each role being assigned is below the actor's highest role
        for role_id in roles {
            let role = db::roles::get_role_row(&state.db, role_id).await?;
            require_role_hierarchy(&state.db, &space_id, &auth.user_id, role.position).await?;
        }
    }

    // Mute/deafen require their respective permissions
    if input.mute.is_some() {
        require_permission(&state.db, &space_id, &auth, "mute_members").await?;
    }
    if input.deaf.is_some() {
        require_permission(&state.db, &space_id, &auth, "deafen_members").await?;
    }

    // Process avatar data URI
    let entity_id = format!("{}_{}", space_id, user_id);
    if let Some(ref avatar) = input.avatar {
        if avatar.starts_with("data:") {
            let old_member = db::members::get_member_row(&state.db, &space_id, &user_id).await?;
            if let Some(ref old_avatar) = old_member.avatar {
                let _ = storage::delete_file(&state.storage_path, old_avatar).await;
            }
            let (url, _, _, _) =
                storage::save_avatar_image(&state.storage_path, "avatars", &entity_id, avatar)
                    .await?;
            input.avatar = Some(url);
        } else if avatar.is_empty() {
            let old_member = db::members::get_member_row(&state.db, &space_id, &user_id).await?;
            if let Some(ref old_avatar) = old_member.avatar {
                let _ = storage::delete_file(&state.storage_path, old_avatar).await;
            }
            storage::delete_avatar(&state.storage_path, "avatars", &entity_id).await?;
            // Keep as Some("") — DB layer will treat empty string as NULL
        }
    }

    let row = db::members::update_member(&state.db, &space_id, &user_id, &input).await?;
    let role_ids = db::members::get_member_role_ids(&state.db, &space_id, &user_id).await?;
    Ok(Json(
        serde_json::json!({ "data": member_row_to_json(&row, &role_ids) }),
    ))
}

pub async fn kick_member(
    state: State<AppState>,
    Path((space_id, user_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "kick_members").await?;
    require_hierarchy(&state.db, &space_id, &auth.user_id, &user_id).await?;
    db::members::remove_member(&state.db, &space_id, &user_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn update_own_member(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(mut input): Json<UpdateMember>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "change_nickname").await?;

    // Process avatar data URI for self
    let entity_id = format!("{}_{}", space_id, auth.user_id);
    if let Some(ref avatar) = input.avatar {
        if avatar.starts_with("data:") {
            let old_member =
                db::members::get_member_row(&state.db, &space_id, &auth.user_id).await?;
            if let Some(ref old_avatar) = old_member.avatar {
                let _ = storage::delete_file(&state.storage_path, old_avatar).await;
            }
            let (url, _, _, _) =
                storage::save_avatar_image(&state.storage_path, "avatars", &entity_id, avatar)
                    .await?;
            input.avatar = Some(url);
        } else if avatar.is_empty() {
            let old_member =
                db::members::get_member_row(&state.db, &space_id, &auth.user_id).await?;
            if let Some(ref old_avatar) = old_member.avatar {
                let _ = storage::delete_file(&state.storage_path, old_avatar).await;
            }
            storage::delete_avatar(&state.storage_path, "avatars", &entity_id).await?;
            // Keep as Some("") — DB layer will treat empty string as NULL
        }
    }

    let limited = UpdateMember {
        nickname: input.nickname,
        avatar: input.avatar,
        roles: None,
        mute: None,
        deaf: None,
    };
    let row = db::members::update_member(&state.db, &space_id, &auth.user_id, &limited).await?;
    let role_ids = db::members::get_member_role_ids(&state.db, &space_id, &auth.user_id).await?;
    Ok(Json(
        serde_json::json!({ "data": member_row_to_json(&row, &role_ids) }),
    ))
}

pub async fn add_role(
    state: State<AppState>,
    Path((space_id, user_id, role_id)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_roles").await?;
    let role = db::roles::get_role_row(&state.db, &role_id).await?;
    require_role_hierarchy(&state.db, &space_id, &auth.user_id, role.position).await?;
    db::members::add_role_to_member(&state.db, &space_id, &user_id, &role_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_role(
    state: State<AppState>,
    Path((space_id, user_id, role_id)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_roles").await?;
    let role = db::roles::get_role_row(&state.db, &role_id).await?;
    require_role_hierarchy(&state.db, &space_id, &auth.user_id, role.position).await?;
    db::members::remove_role_from_member(&state.db, &space_id, &user_id, &role_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

fn member_row_to_json(row: &MemberRow, role_ids: &[String]) -> serde_json::Value {
    serde_json::json!({
        "user_id": row.user_id,
        "space_id": row.space_id,
        "nickname": row.nickname,
        "avatar": row.avatar,
        "roles": role_ids,
        "joined_at": row.joined_at,
        "premium_since": row.premium_since,
        "deaf": row.deaf,
        "mute": row.mute,
        "pending": row.pending,
        "timed_out_until": row.timed_out_until,
        "permissions": null
    })
}
