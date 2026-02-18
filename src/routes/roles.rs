use axum::extract::{Path, State};
use axum::Json;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{
    require_membership, require_permission, require_role_hierarchy,
    resolve_member_permissions_with_admin,
};
use crate::models::permission::ALL_PERMISSIONS;
use crate::models::role::{CreateRole, RolePositionUpdate, RoleRow, UpdateRole};
use crate::state::AppState;

/// Validate that every permission in the list is known and that the actor holds
/// all of them. This prevents privilege escalation via role creation/editing.
async fn validate_role_permissions(
    pool: &sqlx::SqlitePool,
    space_id: &str,
    auth: &AuthUser,
    permissions: &[String],
) -> Result<(), AppError> {
    // Reject unknown permission strings
    for p in permissions {
        if !ALL_PERMISSIONS.contains(&p.as_str()) {
            return Err(AppError::BadRequest(format!("unknown permission: {p}")));
        }
    }
    // Actor can only grant permissions they themselves hold
    let actor_perms =
        resolve_member_permissions_with_admin(pool, space_id, &auth.user_id, auth.is_admin).await?;
    // Administrators can grant anything
    if actor_perms.iter().any(|p| p == "administrator") {
        return Ok(());
    }
    for p in permissions {
        if !actor_perms.contains(p) {
            return Err(AppError::Forbidden(format!(
                "you cannot grant a permission you do not have: {p}"
            )));
        }
    }
    Ok(())
}

pub async fn list_roles(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let rows = db::roles::list_roles(&state.db, &space_id).await?;
    let roles: Vec<serde_json::Value> = rows.iter().map(role_row_to_json).collect();
    Ok(Json(serde_json::json!({ "data": roles })))
}

pub async fn create_role(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateRole>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_roles").await?;
    if let Some(ref perms) = input.permissions {
        validate_role_permissions(&state.db, &space_id, &auth, perms).await?;
    }
    let row = db::roles::create_role(&state.db, &space_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": role_row_to_json(&row) })))
}

pub async fn update_role(
    state: State<AppState>,
    Path((space_id, role_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(mut input): Json<UpdateRole>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_roles").await?;
    let target_role = db::roles::get_role_row(&state.db, &role_id).await?;
    require_role_hierarchy(&state.db, &space_id, &auth.user_id, target_role.position).await?;
    if let Some(ref perms) = input.permissions {
        validate_role_permissions(&state.db, &space_id, &auth, perms).await?;
    }
    // Strip position — must use the dedicated reorder_roles endpoint
    input.position = None;
    let row = db::roles::update_role(&state.db, &role_id, &input).await?;
    Ok(Json(serde_json::json!({ "data": role_row_to_json(&row) })))
}

pub async fn delete_role(
    state: State<AppState>,
    Path((space_id, role_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_roles").await?;
    let target_role = db::roles::get_role_row(&state.db, &role_id).await?;
    if target_role.position == 0 {
        return Err(AppError::Forbidden("cannot delete the @everyone role".into()));
    }
    require_role_hierarchy(&state.db, &space_id, &auth.user_id, target_role.position).await?;
    db::roles::delete_role(&state.db, &role_id).await?;
    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn reorder_roles(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<Vec<RolePositionUpdate>>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_roles").await?;

    // Validate: only @everyone (position 0) can stay at position 0
    let roles = db::roles::list_roles(&state.db, &space_id).await?;
    let everyone_id = roles.iter().find(|r| r.position == 0).map(|r| r.id.clone());
    for u in &input {
        if u.position == 0 && everyone_id.as_deref() != Some(&u.id) {
            return Err(AppError::BadRequest(
                "only @everyone can be at position 0".into(),
            ));
        }
    }

    let updates: Vec<(String, i64)> = input.into_iter().map(|u| (u.id, u.position)).collect();
    db::roles::reorder_roles(&state.db, &space_id, &updates).await?;
    let rows = db::roles::list_roles(&state.db, &space_id).await?;
    let roles: Vec<serde_json::Value> = rows.iter().map(role_row_to_json).collect();
    Ok(Json(serde_json::json!({ "data": roles })))
}

fn role_row_to_json(row: &RoleRow) -> serde_json::Value {
    let permissions: Vec<String> = serde_json::from_str(&row.permissions).unwrap_or_default();
    serde_json::json!({
        "id": row.id,
        "name": row.name,
        "color": row.color,
        "hoist": row.hoist,
        "icon": row.icon,
        "position": row.position,
        "permissions": permissions,
        "managed": row.managed,
        "mentionable": row.mentionable
    })
}
