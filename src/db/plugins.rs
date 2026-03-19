use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::plugin::{Plugin, PluginManifest, PluginSession, PluginSessionParticipant};
use crate::snowflake;

const SELECT_PLUGINS: &str = "SELECT id, space_id, name, plugin_type, runtime, description, version, manifest_json, bundle_hash, signed, creator_id, created_at, updated_at, bundle_blob IS NOT NULL AS has_bundle, icon_blob IS NOT NULL AS has_icon FROM plugins";

fn row_to_plugin(row: sqlx::any::AnyRow) -> Plugin {
    let manifest_str: String = row.get("manifest_json");
    let manifest: PluginManifest = serde_json::from_str(&manifest_str).unwrap_or_default();
    let has_icon = crate::db::get_bool(&row, "has_icon");
    let id: String = row.get("id");
    let space_id: String = row.get("space_id");

    let icon_url = if has_icon {
        Some(format!("/api/v1/plugins/{id}/icon"))
    } else {
        None
    };

    Plugin {
        id,
        space_id,
        name: row.get("name"),
        plugin_type: row.get("plugin_type"),
        runtime: row.get("runtime"),
        description: row.get("description"),
        version: row.get("version"),
        bundle_hash: row.get("bundle_hash"),
        signed: crate::db::get_bool(&row, "signed"),
        icon_url,
        has_bundle: crate::db::get_bool(&row, "has_bundle"),
        creator_id: row.get("creator_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        // Flattened manifest fields
        entry_point: manifest.entry_point.clone(),
        max_participants: manifest.max_participants,
        max_spectators: manifest.max_spectators,
        max_file_size: manifest.max_file_size,
        lobby: manifest.lobby,
        permissions: manifest.permissions.clone(),
        data_topics: manifest.data_topics.clone(),
        canvas_size: manifest.canvas_size,
        signature: manifest.signature.clone(),
        manifest,
    }
}

pub async fn get_plugin(pool: &AnyPool, plugin_id: &str) -> Result<Plugin, AppError> {
    let row = sqlx::query(&super::q(&format!("{SELECT_PLUGINS} WHERE id = ?")))
        .bind(plugin_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_plugin".to_string()))?;
    Ok(row_to_plugin(row))
}

pub async fn list_plugins(
    pool: &AnyPool,
    space_id: &str,
    plugin_type: Option<&str>,
) -> Result<Vec<Plugin>, AppError> {
    let rows = if let Some(pt) = plugin_type {
        sqlx::query(&super::q(&format!(
            "{SELECT_PLUGINS} WHERE space_id = ? AND plugin_type = ? ORDER BY created_at"
        )))
        .bind(space_id)
        .bind(pt)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(&super::q(&format!(
            "{SELECT_PLUGINS} WHERE space_id = ? ORDER BY created_at"
        )))
        .bind(space_id)
        .fetch_all(pool)
        .await?
    };

    Ok(rows.into_iter().map(row_to_plugin).collect())
}

pub async fn require_plugin_in_space(
    pool: &AnyPool,
    plugin_id: &str,
    space_id: &str,
) -> Result<(), AppError> {
    let row: Option<(String,)> =
        sqlx::query_as(&super::q("SELECT space_id FROM plugins WHERE id = ?"))
            .bind(plugin_id)
            .fetch_optional(pool)
            .await?;
    match row {
        Some((sid,)) if sid == space_id => Ok(()),
        Some(_) => Err(AppError::NotFound(
            "plugin not found in this space".to_string(),
        )),
        None => Err(AppError::NotFound("unknown_plugin".to_string())),
    }
}

/// Insert a plugin with its manifest, bundle ZIP, and optional icon.
#[allow(clippy::too_many_arguments)]
pub async fn create_plugin(
    pool: &AnyPool,
    space_id: &str,
    creator_id: &str,
    manifest: &PluginManifest,
    bundle_blob: Option<&[u8]>,
    icon_blob: Option<&[u8]>,
) -> Result<Plugin, AppError> {
    let id = snowflake::generate();
    let manifest_json = serde_json::to_string(manifest).unwrap_or_default();
    let bundle_hash = &manifest.bundle_hash;
    let signed = manifest.signed;

    sqlx::query(&super::q(
        "INSERT INTO plugins (id, space_id, name, plugin_type, runtime, description, version, manifest_json, bundle_blob, icon_blob, bundle_hash, signed, creator_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(space_id)
    .bind(&manifest.name)
    .bind(&manifest.plugin_type)
    .bind(&manifest.runtime)
    .bind(&manifest.description)
    .bind(&manifest.version)
    .bind(&manifest_json)
    .bind(bundle_blob)
    .bind(icon_blob)
    .bind(bundle_hash)
    .bind(signed)
    .bind(creator_id)
    .execute(pool)
    .await?;

    get_plugin(pool, &id).await
}

pub async fn delete_plugin(pool: &AnyPool, plugin_id: &str) -> Result<(), AppError> {
    sqlx::query(&super::q("DELETE FROM plugins WHERE id = ?"))
        .bind(plugin_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Fetch the bundle BLOB for a plugin.
pub async fn get_bundle_blob(pool: &AnyPool, plugin_id: &str) -> Result<Vec<u8>, AppError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as(&super::q("SELECT bundle_blob FROM plugins WHERE id = ?"))
            .bind(plugin_id)
            .fetch_optional(pool)
            .await?;
    match row {
        Some((blob,)) => Ok(blob),
        None => Err(AppError::NotFound("unknown_plugin".to_string())),
    }
}

/// Fetch the icon BLOB for a plugin.
pub async fn get_icon_blob(pool: &AnyPool, plugin_id: &str) -> Result<Vec<u8>, AppError> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as(&super::q("SELECT icon_blob FROM plugins WHERE id = ?"))
            .bind(plugin_id)
            .fetch_optional(pool)
            .await?;
    match row {
        Some((blob,)) => Ok(blob),
        None => Err(AppError::NotFound("unknown_plugin".to_string())),
    }
}

/// Get the space_id for a plugin.
pub async fn get_plugin_space_id(pool: &AnyPool, plugin_id: &str) -> Result<String, AppError> {
    let row: (String,) = sqlx::query_as(&super::q("SELECT space_id FROM plugins WHERE id = ?"))
        .bind(plugin_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("unknown_plugin".to_string()))?;
    Ok(row.0)
}

// --- Sessions ---

fn row_to_session(row: sqlx::any::AnyRow) -> PluginSession {
    PluginSession {
        id: row.get("id"),
        plugin_id: row.get("plugin_id"),
        channel_id: row.get("channel_id"),
        host_user_id: row.get("host_user_id"),
        state: row.get("state"),
        participants: Vec::new(), // loaded separately
        created_at: row.get("created_at"),
    }
}

pub async fn get_session(pool: &AnyPool, session_id: &str) -> Result<PluginSession, AppError> {
    let row = sqlx::query(&super::q(
        "SELECT id, plugin_id, channel_id, host_user_id, state, created_at FROM plugin_sessions WHERE id = ?",
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown_session".to_string()))?;

    let mut session = row_to_session(row);
    session.participants = list_participants(pool, session_id).await?;
    Ok(session)
}

pub async fn create_session(
    pool: &AnyPool,
    plugin_id: &str,
    channel_id: &str,
    host_user_id: &str,
    use_lobby: bool,
) -> Result<PluginSession, AppError> {
    let id = snowflake::generate();
    let initial_state = if use_lobby { "lobby" } else { "running" };

    sqlx::query(&super::q(
        "INSERT INTO plugin_sessions (id, plugin_id, channel_id, host_user_id, state) VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(plugin_id)
    .bind(channel_id)
    .bind(host_user_id)
    .bind(initial_state)
    .execute(pool)
    .await?;

    // Add host as a participant (player by default, slot 0)
    add_participant(pool, &id, host_user_id, "player", Some(0)).await?;

    get_session(pool, &id).await
}

pub async fn update_session_state(
    pool: &AnyPool,
    session_id: &str,
    state: &str,
    is_postgres: bool,
) -> Result<PluginSession, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
    let sql = format!("UPDATE plugin_sessions SET state = ?, updated_at = {now_fn} WHERE id = ?");
    sqlx::query(&super::q(&sql))
        .bind(state)
        .bind(session_id)
        .execute(pool)
        .await?;

    get_session(pool, session_id).await
}

pub async fn delete_session(pool: &AnyPool, session_id: &str) -> Result<(), AppError> {
    sqlx::query(&super::q("DELETE FROM plugin_sessions WHERE id = ?"))
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

// --- Participants ---

pub async fn list_participants(
    pool: &AnyPool,
    session_id: &str,
) -> Result<Vec<PluginSessionParticipant>, AppError> {
    let rows = sqlx::query(&super::q(
        "SELECT user_id, role, slot_index, joined_at FROM plugin_session_participants WHERE session_id = ? ORDER BY joined_at",
    ))
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let slot_index: Option<i64> = row.get("slot_index");
            PluginSessionParticipant {
                user_id: row.get("user_id"),
                role: row.get("role"),
                slot_index,
                joined_at: row.get("joined_at"),
            }
        })
        .collect())
}

pub async fn add_participant(
    pool: &AnyPool,
    session_id: &str,
    user_id: &str,
    role: &str,
    slot_index: Option<i64>,
) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "INSERT INTO plugin_session_participants (session_id, user_id, role, slot_index) VALUES (?, ?, ?, ?)",
    ))
    .bind(session_id)
    .bind(user_id)
    .bind(role)
    .bind(slot_index)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_participant_role(
    pool: &AnyPool,
    session_id: &str,
    user_id: &str,
    role: &str,
    slot_index: Option<i64>,
) -> Result<(), AppError> {
    let result = sqlx::query(&super::q(
        "UPDATE plugin_session_participants SET role = ?, slot_index = ? WHERE session_id = ? AND user_id = ?",
    ))
    .bind(role)
    .bind(slot_index)
    .bind(session_id)
    .bind(user_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "participant not found in session".to_string(),
        ));
    }
    Ok(())
}

pub async fn remove_participant(
    pool: &AnyPool,
    session_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "DELETE FROM plugin_session_participants WHERE session_id = ? AND user_id = ?",
    ))
    .bind(session_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Count current players in a session.
pub async fn count_players(pool: &AnyPool, session_id: &str) -> Result<i64, AppError> {
    let count: (i64,) = sqlx::query_as(&super::q(
        "SELECT COUNT(*) FROM plugin_session_participants WHERE session_id = ? AND role = 'player'",
    ))
    .bind(session_id)
    .fetch_one(pool)
    .await?;
    Ok(count.0)
}

/// Find the next available slot index for a session.
pub async fn next_slot_index(pool: &AnyPool, session_id: &str) -> Result<i64, AppError> {
    let max: Option<i64> = sqlx::query_scalar(&super::q(
        "SELECT MAX(slot_index) FROM plugin_session_participants WHERE session_id = ? AND role = 'player'",
    ))
    .bind(session_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(max.map(|m| m + 1).unwrap_or(0))
}

/// Get active (non-ended) sessions for a channel.
pub async fn get_active_sessions_for_channel(
    pool: &AnyPool,
    channel_id: &str,
) -> Result<Vec<PluginSession>, AppError> {
    let rows = sqlx::query(&super::q(
        "SELECT id, plugin_id, channel_id, host_user_id, state, created_at FROM plugin_sessions WHERE channel_id = ? AND state != 'ended' ORDER BY created_at",
    ))
    .bind(channel_id)
    .fetch_all(pool)
    .await?;

    let mut sessions = Vec::new();
    for row in rows {
        let mut session = row_to_session(row);
        session.participants = list_participants(pool, &session.id).await?;
        sessions.push(session);
    }
    Ok(sessions)
}

/// Get participant user IDs for a session (for targeted gateway broadcasts).
pub async fn get_session_user_ids(
    pool: &AnyPool,
    session_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows: Vec<(String,)> = sqlx::query_as(&super::q(
        "SELECT user_id FROM plugin_session_participants WHERE session_id = ?",
    ))
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}
