use sqlx::SqlitePool;

use crate::db;
use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::models::permission::has_permission;

/// Default permissions granted to the @everyone role when a space is created.
pub const DEFAULT_EVERYONE_PERMISSIONS: &[&str] = &[
    "view_channel",
    "send_messages",
    "read_history",
    "add_reactions",
    "create_invites",
    "change_nickname",
    "connect",
    "speak",
    "use_vad",
    "embed_links",
    "attach_files",
    "use_external_emojis",
    "stream",
];

/// Permissions granted to the default Moderator role.
pub const MODERATOR_PERMISSIONS: &[&str] = &[
    // @everyone base
    "view_channel",
    "send_messages",
    "read_history",
    "add_reactions",
    "create_invites",
    "change_nickname",
    "connect",
    "speak",
    "use_vad",
    "embed_links",
    "attach_files",
    "use_external_emojis",
    "stream",
    // Moderation extras
    "kick_members",
    "ban_members",
    "manage_messages",
    "mute_members",
    "deafen_members",
    "move_members",
    "manage_nicknames",
    "moderate_members",
    "mention_everyone",
    "manage_threads",
    "manage_events",
];

/// Permissions granted to the default Admin role.
/// Note: `administrator` is intentionally excluded -- it's a God-mode bypass.
pub const ADMIN_PERMISSIONS: &[&str] = &[
    // @everyone base
    "view_channel",
    "send_messages",
    "read_history",
    "add_reactions",
    "create_invites",
    "change_nickname",
    "connect",
    "speak",
    "use_vad",
    "embed_links",
    "attach_files",
    "use_external_emojis",
    "stream",
    // Moderation
    "kick_members",
    "ban_members",
    "manage_messages",
    "mute_members",
    "deafen_members",
    "move_members",
    "manage_nicknames",
    "moderate_members",
    "mention_everyone",
    "manage_threads",
    "manage_events",
    // Admin extras
    "manage_channels",
    "manage_space",
    "manage_roles",
    "manage_webhooks",
    "manage_emojis",
    "view_audit_log",
    "priority_speaker",
];

/// Check that the authenticated user is a server (instance) admin.
pub fn require_server_admin(auth: &AuthUser) -> Result<(), AppError> {
    if !auth.is_admin {
        return Err(AppError::Forbidden(
            "server admin privileges required".into(),
        ));
    }
    Ok(())
}

/// Compute effective permissions for a user in a space.
///
/// - If `is_server_admin` is true, returns `["administrator"]` (instance-level bypass).
/// - If the user is the space owner, returns `["administrator"]`.
/// - If the user is not a member, returns `Forbidden`.
/// - Otherwise, merges @everyone permissions with all assigned role permissions.
pub async fn resolve_member_permissions(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    resolve_member_permissions_inner(pool, space_id, user_id, false).await
}

/// Like `resolve_member_permissions` but allows an instance-admin bypass.
pub async fn resolve_member_permissions_with_admin(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    is_server_admin: bool,
) -> Result<Vec<String>, AppError> {
    resolve_member_permissions_inner(pool, space_id, user_id, is_server_admin).await
}

async fn resolve_member_permissions_inner(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    is_server_admin: bool,
) -> Result<Vec<String>, AppError> {
    // Instance-level admin bypass
    if is_server_admin {
        return Ok(vec!["administrator".to_string()]);
    }

    // Check ownership first
    let space = db::spaces::get_space_row(pool, space_id).await?;
    if space.owner_id == user_id {
        return Ok(vec!["administrator".to_string()]);
    }

    // Verify membership (will return NotFound → we convert to Forbidden)
    db::members::get_member_row(pool, space_id, user_id)
        .await
        .map_err(|e| match e {
            AppError::NotFound(_) => {
                AppError::Forbidden("you are not a member of this space".to_string())
            }
            other => other,
        })?;

    // Start with @everyone role permissions
    let roles = db::roles::list_roles(pool, space_id).await?;
    let mut perms: Vec<String> = Vec::new();

    // Find @everyone role (position 0)
    if let Some(everyone) = roles.iter().find(|r| r.position == 0) {
        let everyone_perms: Vec<String> =
            serde_json::from_str(&everyone.permissions).unwrap_or_default();
        perms.extend(everyone_perms);
    }

    // Get member's assigned roles and merge their permissions
    let member_role_ids = db::members::get_member_role_ids(pool, space_id, user_id).await?;
    for role in &roles {
        if member_role_ids.contains(&role.id) {
            let role_perms: Vec<String> =
                serde_json::from_str(&role.permissions).unwrap_or_default();
            for p in role_perms {
                if !perms.contains(&p) {
                    perms.push(p);
                }
            }
        }
    }

    Ok(perms)
}

/// Check that a user has a specific permission in a space.
/// Returns `Forbidden` if the user lacks the permission or is not a member.
pub async fn require_permission(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    perm: &str,
) -> Result<(), AppError> {
    let perms = resolve_member_permissions(pool, space_id, user_id).await?;
    if !has_permission(&perms, perm) {
        return Err(AppError::Forbidden(format!("missing permission: {perm}")));
    }
    Ok(())
}

/// Shorthand: require that a user is a member of the space (has view_channel).
pub async fn require_membership(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    require_permission(pool, space_id, user_id, "view_channel").await
}

/// Resolve effective permissions for a user in a specific channel,
/// accounting for permission overwrites (role, member).
///
/// Algorithm (Discord-style):
/// 1. Start with base space permissions from `resolve_member_permissions`.
/// 2. If base includes `administrator`, return immediately (bypass).
/// 3. Apply @everyone role overwrite: deny removes, allow adds.
/// 4. Union of user's role overwrites: collect all allow/deny, allow wins, then apply.
/// 5. Apply member-specific overwrite: deny removes, allow adds.
pub async fn resolve_channel_permissions(
    pool: &SqlitePool,
    channel_id: &str,
    space_id: &str,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    let mut perms = resolve_member_permissions(pool, space_id, user_id).await?;

    // Administrator bypasses all overwrites
    if perms.iter().any(|p| p == "administrator") {
        return Ok(perms);
    }

    let overwrites = db::permission_overwrites::list_overwrites(pool, channel_id).await?;
    if overwrites.is_empty() {
        return Ok(perms);
    }

    // Find the @everyone role (its ID is the role at position 0)
    let roles = db::roles::list_roles(pool, space_id).await?;
    let everyone_role_id = roles.iter().find(|r| r.position == 0).map(|r| r.id.clone());

    // Step 1: Apply @everyone role overwrite
    if let Some(ref eid) = everyone_role_id {
        if let Some(ow) = overwrites.iter().find(|o| o.overwrite_type == "role" && o.id == *eid) {
            for d in &ow.deny {
                perms.retain(|p| p != d);
            }
            for a in &ow.allow {
                if !perms.contains(a) {
                    perms.push(a.clone());
                }
            }
        }
    }

    // Step 2: Union of user's assigned role overwrites
    let member_role_ids = db::members::get_member_role_ids(pool, space_id, user_id).await?;
    let role_overwrites: Vec<&crate::models::permission::PermissionOverwrite> = overwrites
        .iter()
        .filter(|o| {
            o.overwrite_type == "role"
                && member_role_ids.contains(&o.id)
                && everyone_role_id.as_deref() != Some(&o.id)
        })
        .collect();

    if !role_overwrites.is_empty() {
        let mut role_allow: Vec<String> = Vec::new();
        let mut role_deny: Vec<String> = Vec::new();
        for ow in &role_overwrites {
            for a in &ow.allow {
                if !role_allow.contains(a) {
                    role_allow.push(a.clone());
                }
            }
            for d in &ow.deny {
                if !role_deny.contains(d) {
                    role_deny.push(d.clone());
                }
            }
        }
        // Allow wins over deny across roles
        role_deny.retain(|d| !role_allow.contains(d));

        for d in &role_deny {
            perms.retain(|p| p != d);
        }
        for a in &role_allow {
            if !perms.contains(a) {
                perms.push(a.clone());
            }
        }
    }

    // Step 3: Apply member-specific overwrite (highest precedence)
    if let Some(ow) = overwrites
        .iter()
        .find(|o| o.overwrite_type == "member" && o.id == user_id)
    {
        for d in &ow.deny {
            perms.retain(|p| p != d);
        }
        for a in &ow.allow {
            if !perms.contains(a) {
                perms.push(a.clone());
            }
        }
    }

    Ok(perms)
}

/// Check that a user has a specific permission for a channel.
/// Uses `resolve_channel_permissions` which accounts for overwrites.
/// Returns the space_id on success.
pub async fn require_channel_permission(
    pool: &SqlitePool,
    channel_id: &str,
    user_id: &str,
    perm: &str,
) -> Result<String, AppError> {
    let channel = db::channels::get_channel_row(pool, channel_id).await?;
    let space_id = channel
        .space_id
        .ok_or_else(|| AppError::BadRequest("channel has no space".to_string()))?;
    let perms = resolve_channel_permissions(pool, channel_id, &space_id, user_id).await?;
    if !has_permission(&perms, perm) {
        return Err(AppError::Forbidden(format!("missing permission: {perm}")));
    }
    Ok(space_id)
}

/// Shorthand: require that a user is a member of the channel's space.
/// Returns the space_id on success.
pub async fn require_channel_membership(
    pool: &SqlitePool,
    channel_id: &str,
    user_id: &str,
) -> Result<String, AppError> {
    require_channel_permission(pool, channel_id, user_id, "view_channel").await
}

/// Returns a user's highest role position in a space.
/// Space owner returns `i64::MAX`. A member with only @everyone returns 0.
pub async fn get_highest_role_position(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<i64, AppError> {
    // Owner outranks everyone
    let space = db::spaces::get_space_row(pool, space_id).await?;
    if space.owner_id == user_id {
        return Ok(i64::MAX);
    }

    let role_ids = db::members::get_member_role_ids(pool, space_id, user_id).await?;
    if role_ids.is_empty() {
        return Ok(0); // only @everyone
    }

    let roles = db::roles::list_roles(pool, space_id).await?;
    let max_pos = roles
        .iter()
        .filter(|r| role_ids.contains(&r.id))
        .map(|r| r.position)
        .max()
        .unwrap_or(0);

    Ok(max_pos)
}

/// Requires that the actor's highest role position is strictly greater than
/// the target user's highest role position. Prevents lateral or upward actions.
pub async fn require_hierarchy(
    pool: &SqlitePool,
    space_id: &str,
    actor_id: &str,
    target_id: &str,
) -> Result<(), AppError> {
    let actor_pos = get_highest_role_position(pool, space_id, actor_id).await?;
    let target_pos = get_highest_role_position(pool, space_id, target_id).await?;
    if actor_pos <= target_pos {
        return Err(AppError::Forbidden(
            "you cannot act on a member with an equal or higher role".into(),
        ));
    }
    Ok(())
}

/// Requires that the actor's highest role position is strictly greater than
/// the given role's position. Used for role management operations.
pub async fn require_role_hierarchy(
    pool: &SqlitePool,
    space_id: &str,
    actor_id: &str,
    role_position: i64,
) -> Result<(), AppError> {
    let actor_pos = get_highest_role_position(pool, space_id, actor_id).await?;
    if actor_pos <= role_position {
        return Err(AppError::Forbidden(
            "you cannot manage a role at or above your highest role".into(),
        ));
    }
    Ok(())
}
