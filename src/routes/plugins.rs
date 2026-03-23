use axum::body::Bytes;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::io::Read;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_membership, require_permission};
use crate::models::plugin::{
    AssignRole, CreateSession, PluginAction, PluginManifest, UpdateSessionState,
};
use crate::state::AppState;

const MAX_BUNDLE_SIZE: usize = 50 * 1024 * 1024; // 50 MB

#[derive(Debug, Deserialize)]
pub struct ListPluginsQuery {
    #[serde(rename = "type")]
    pub plugin_type: Option<String>,
}

// ── Space-scoped plugin management ──────────────────────────────────────────

pub async fn list_plugins(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    Query(query): Query<ListPluginsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_membership(&state.db, &space_id, &auth.user_id).await?;
    let plugins =
        db::plugins::list_plugins(&state.db, &space_id, query.plugin_type.as_deref()).await?;
    Ok(Json(serde_json::json!({ "data": plugins })))
}

/// Upload a `.daccord-plugin` bundle (ZIP file). The server parses the ZIP,
/// extracts `plugin.json` as the manifest, stores the Lua source
/// (scripted), the full bundle (native), and the icon if present.
pub async fn install_plugin(
    state: State<AppState>,
    Path(space_id): Path<String>,
    auth: AuthUser,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_space").await?;

    let mut bundle_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("multipart error: {e}")))?
    {
        match field.name() {
            Some("bundle") | Some("file") => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("failed to read bundle: {e}")))?;
                if data.len() > MAX_BUNDLE_SIZE {
                    return Err(AppError::PayloadTooLarge(format!(
                        "bundle exceeds maximum size of {} MB",
                        MAX_BUNDLE_SIZE / (1024 * 1024)
                    )));
                }
                bundle_bytes = Some(data.to_vec());
            }
            _ => {}
        }
    }

    let zip_bytes = bundle_bytes
        .ok_or_else(|| AppError::BadRequest("missing bundle file in upload".to_string()))?;

    // Parse the ZIP and extract plugin.json and icon
    let parsed = parse_plugin_bundle(&zip_bytes)?;

    // Validate manifest
    validate_manifest(&parsed.manifest)?;

    // For native plugins, verify plugin.sig exists
    if parsed.manifest.runtime == "native" && !parsed.has_signature {
        return Err(AppError::BadRequest(
            "native plugins must include a plugin.sig signature file".to_string(),
        ));
    }

    // Store in DB — full bundle ZIP is stored for both scripted and native plugins
    let plugin = db::plugins::create_plugin(
        &state.db,
        &space_id,
        &auth.user_id,
        &parsed.manifest,
        Some(&zip_bytes),
        parsed.icon_blob.as_deref(),
    )
    .await?;

    // Broadcast plugin.installed
    broadcast_plugin_event(
        &state,
        Some(&space_id),
        None,
        "plugin.installed",
        serde_json::json!({
            "space_id": space_id,
            "manifest": plugin
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": plugin })))
}

pub async fn uninstall_plugin(
    state: State<AppState>,
    Path((space_id, plugin_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_permission(&state.db, &space_id, &auth, "manage_space").await?;
    db::plugins::require_plugin_in_space(&state.db, &plugin_id, &space_id).await?;

    db::plugins::delete_plugin(&state.db, &plugin_id).await?;

    // Broadcast plugin.uninstalled
    broadcast_plugin_event(
        &state,
        Some(&space_id),
        None,
        "plugin.uninstalled",
        serde_json::json!({
            "space_id": space_id,
            "plugin_id": plugin_id
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": null })))
}

// ── Plugin content serving ──────────────────────────────────────────────────

pub async fn get_plugin_source(
    state: State<AppState>,
    Path(plugin_id): Path<String>,
    auth: AuthUser,
) -> Result<impl IntoResponse, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    if plugin.runtime != "scripted" {
        return Err(AppError::BadRequest(
            "source is only available for scripted plugins".to_string(),
        ));
    }

    let bytes = db::plugins::get_bundle_blob(&state.db, &plugin_id).await?;
    if bytes.is_empty() {
        return Err(AppError::NotFound("plugin bundle not found".to_string()));
    }

    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"plugin.zip\"",
            ),
        ],
        Bytes::from(bytes),
    ))
}

pub async fn get_plugin_bundle(
    state: State<AppState>,
    Path(plugin_id): Path<String>,
    auth: AuthUser,
) -> Result<impl IntoResponse, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let bytes = db::plugins::get_bundle_blob(&state.db, &plugin_id).await?;
    if bytes.is_empty() {
        return Err(AppError::NotFound("plugin bundle not found".to_string()));
    }

    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"plugin.zip\"",
            ),
        ],
        Bytes::from(bytes),
    ))
}

pub async fn get_plugin_icon(
    state: State<AppState>,
    Path(plugin_id): Path<String>,
    auth: AuthUser,
) -> Result<impl IntoResponse, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let bytes = db::plugins::get_icon_blob(&state.db, &plugin_id).await?;
    if bytes.is_empty() {
        return Err(AppError::NotFound("plugin icon not found".to_string()));
    }

    Ok(([(header::CONTENT_TYPE, "image/png")], Bytes::from(bytes)))
}

// ── Sessions ────────────────────────────────────────────────────────────────

pub async fn get_channel_active_sessions(
    state: State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    // Look up the channel's space to verify membership
    let channel = db::channels::get_channel_row(&state.db, &channel_id).await?;
    let space_id = channel
        .space_id
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("channel has no space".to_string()))?;
    require_membership(&state.db, space_id, &auth.user_id).await?;

    let sessions = db::plugins::get_active_sessions_for_channel(&state.db, &channel_id).await?;
    Ok(Json(serde_json::json!({ "data": sessions })))
}

pub async fn create_session(
    state: State<AppState>,
    Path(plugin_id): Path<String>,
    auth: AuthUser,
    Json(input): Json<CreateSession>,
) -> Result<Json<serde_json::Value>, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let session = db::plugins::create_session(
        &state.db,
        &plugin_id,
        &input.channel_id,
        &auth.user_id,
        plugin.lobby,
    )
    .await?;

    // Broadcast session creation to space
    broadcast_plugin_event(
        &state,
        Some(&plugin.space_id),
        None,
        "plugin.session_state",
        serde_json::json!({
            "plugin_id": plugin_id,
            "session_id": session.id,
            "state": session.state,
            "channel_id": session.channel_id,
            "host_user_id": session.host_user_id,
            "participants": session.participants,
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": session })))
}

pub async fn delete_session(
    state: State<AppState>,
    Path((plugin_id, session_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let session = db::plugins::get_session(&state.db, &session_id).await?;
    if session.plugin_id != plugin_id {
        return Err(AppError::NotFound(
            "session not found for this plugin".to_string(),
        ));
    }

    // Only host or members with manage_space can end a session
    if session.host_user_id != auth.user_id {
        require_permission(&state.db, &plugin.space_id, &auth, "manage_space").await?;
    }

    let participant_ids = db::plugins::get_session_user_ids(&state.db, &session_id).await?;

    db::plugins::delete_session(&state.db, &session_id).await?;

    // Broadcast session ended to participants
    broadcast_plugin_event(
        &state,
        Some(&plugin.space_id),
        Some(participant_ids),
        "plugin.session_state",
        serde_json::json!({
            "plugin_id": plugin_id,
            "session_id": session_id,
            "state": "ended",
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn update_session_state(
    state: State<AppState>,
    Path((plugin_id, session_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<UpdateSessionState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let session = db::plugins::get_session(&state.db, &session_id).await?;
    if session.plugin_id != plugin_id {
        return Err(AppError::NotFound(
            "session not found for this plugin".to_string(),
        ));
    }

    // Only host can change session state
    if session.host_user_id != auth.user_id {
        return Err(AppError::Forbidden(
            "only the session host can change state".to_string(),
        ));
    }

    // Validate state transition
    match (session.state.as_str(), input.state.as_str()) {
        ("lobby", "running") | ("running", "ended") | ("lobby", "ended") => {}
        _ => {
            return Err(AppError::BadRequest(format!(
                "invalid state transition: {} -> {}",
                session.state, input.state
            )));
        }
    }

    let session = db::plugins::update_session_state(
        &state.db,
        &session_id,
        &input.state,
        state.db_is_postgres,
    )
    .await?;

    let participant_ids = db::plugins::get_session_user_ids(&state.db, &session_id).await?;

    // Broadcast state change to participants
    broadcast_plugin_event(
        &state,
        Some(&plugin.space_id),
        Some(participant_ids),
        "plugin.session_state",
        serde_json::json!({
            "plugin_id": plugin_id,
            "session_id": session_id,
            "state": session.state,
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": session })))
}

// ── Leave session ───────────────────────────────────────────────────────────

pub async fn leave_session(
    state: State<AppState>,
    Path((plugin_id, session_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let session = db::plugins::get_session(&state.db, &session_id).await?;
    if session.plugin_id != plugin_id {
        return Err(AppError::NotFound(
            "session not found for this plugin".to_string(),
        ));
    }

    // Host cannot leave — they must delete the session instead
    if session.host_user_id == auth.user_id {
        return Err(AppError::BadRequest(
            "host must delete the session, not leave it".to_string(),
        ));
    }

    db::plugins::remove_participant(&state.db, &session_id, &auth.user_id).await?;

    let participant_ids = db::plugins::get_session_user_ids(&state.db, &session_id).await?;
    let updated_session = db::plugins::get_session(&state.db, &session_id).await?;

    // Broadcast participant removal to remaining participants
    broadcast_plugin_event(
        &state,
        Some(&plugin.space_id),
        Some(participant_ids),
        "plugin.role_changed",
        serde_json::json!({
            "plugin_id": plugin_id,
            "session_id": session_id,
            "user_id": auth.user_id,
            "role": "left",
            "participants": updated_session.participants,
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": null })))
}

// ── Roles ───────────────────────────────────────────────────────────────────

pub async fn assign_role(
    state: State<AppState>,
    Path((plugin_id, session_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<AssignRole>,
) -> Result<Json<serde_json::Value>, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let session = db::plugins::get_session(&state.db, &session_id).await?;
    if session.plugin_id != plugin_id {
        return Err(AppError::NotFound(
            "session not found for this plugin".to_string(),
        ));
    }

    if input.role != "player" && input.role != "spectator" {
        return Err(AppError::BadRequest(
            "role must be 'player' or 'spectator'".to_string(),
        ));
    }

    // Check max_participants if claiming a player slot
    let slot_index = if input.role == "player" {
        if plugin.max_participants > 0 {
            let current_players = db::plugins::count_players(&state.db, &session_id).await?;
            let is_already_player = session
                .participants
                .iter()
                .any(|p| p.user_id == input.user_id && p.role == "player");
            if !is_already_player && current_players >= plugin.max_participants {
                return Err(AppError::BadRequest(
                    "all player slots are full".to_string(),
                ));
            }
        }
        // Use provided slot_index or auto-assign
        match input.slot_index {
            Some(idx) => Some(idx),
            None => Some(db::plugins::next_slot_index(&state.db, &session_id).await?),
        }
    } else {
        None // spectators have no slot
    };

    // Users can change their own role, or the host can change anyone's role
    if input.user_id != auth.user_id && session.host_user_id != auth.user_id {
        return Err(AppError::Forbidden(
            "only the host or the user themselves can change roles".to_string(),
        ));
    }

    // Ensure user is a participant; if not, add them
    let is_participant = session
        .participants
        .iter()
        .any(|p| p.user_id == input.user_id);
    if !is_participant {
        db::plugins::add_participant(
            &state.db,
            &session_id,
            &input.user_id,
            &input.role,
            slot_index,
        )
        .await?;
    } else {
        db::plugins::update_participant_role(
            &state.db,
            &session_id,
            &input.user_id,
            &input.role,
            slot_index,
        )
        .await?;
    }

    let participant_ids = db::plugins::get_session_user_ids(&state.db, &session_id).await?;

    let updated_session = db::plugins::get_session(&state.db, &session_id).await?;

    // Broadcast role change to participants (includes full participant list for lobby)
    broadcast_plugin_event(
        &state,
        Some(&plugin.space_id),
        Some(participant_ids),
        "plugin.role_changed",
        serde_json::json!({
            "plugin_id": plugin_id,
            "session_id": session_id,
            "user_id": input.user_id,
            "role": input.role,
            "participants": updated_session.participants,
        }),
    )
    .await;

    let session = updated_session;
    Ok(Json(serde_json::json!({ "data": session })))
}

// ── Actions (scripted plugins) ──────────────────────────────────────────────

pub async fn send_action(
    state: State<AppState>,
    Path((plugin_id, session_id)): Path<(String, String)>,
    auth: AuthUser,
    Json(input): Json<PluginAction>,
) -> Result<Json<serde_json::Value>, AppError> {
    let plugin = db::plugins::get_plugin(&state.db, &plugin_id).await?;
    require_membership(&state.db, &plugin.space_id, &auth.user_id).await?;

    let session = db::plugins::get_session(&state.db, &session_id).await?;
    if session.plugin_id != plugin_id {
        return Err(AppError::NotFound(
            "session not found for this plugin".to_string(),
        ));
    }

    // Verify user is a participant
    let is_participant = session
        .participants
        .iter()
        .any(|p| p.user_id == auth.user_id);
    if !is_participant {
        return Err(AppError::Forbidden(
            "you are not a participant in this session".to_string(),
        ));
    }

    if session.state != "running" {
        return Err(AppError::BadRequest(
            "actions can only be sent in a running session".to_string(),
        ));
    }

    let participant_ids = db::plugins::get_session_user_ids(&state.db, &session_id).await?;

    // Broadcast the action as a plugin.event to all session participants
    // (simple relay — no server-side game logic validation)
    broadcast_plugin_event(
        &state,
        Some(&plugin.space_id),
        Some(participant_ids),
        "plugin.event",
        serde_json::json!({
            "plugin_id": plugin_id,
            "session_id": session_id,
            "type": "action",
            "sender_id": auth.user_id,
            "data": input.data,
        }),
    )
    .await;

    Ok(Json(serde_json::json!({ "data": { "ok": true } })))
}

// ── Bundle parsing ──────────────────────────────────────────────────────────

struct ParsedBundle {
    manifest: PluginManifest,
    icon_blob: Option<Vec<u8>>,
    has_signature: bool,
}

/// Parse a `.daccord-plugin` ZIP bundle, extracting the manifest and icon.
/// The full bundle ZIP is stored as-is for both scripted and native plugins.
fn parse_plugin_bundle(zip_bytes: &[u8]) -> Result<ParsedBundle, AppError> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| AppError::BadRequest(format!("invalid ZIP bundle: {e}")))?;

    // 1. Extract and parse plugin.json
    let manifest: PluginManifest = {
        let mut file = archive.by_name("plugin.json").map_err(|_| {
            AppError::BadRequest("bundle must contain a plugin.json manifest".to_string())
        })?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| AppError::BadRequest(format!("failed to read plugin.json: {e}")))?;
        serde_json::from_slice(&buf)
            .map_err(|e| AppError::BadRequest(format!("invalid plugin.json: {e}")))?
    };

    // 2. For scripted plugins: verify the entry file exists in the bundle
    if manifest.runtime == "scripted" {
        let entry = if manifest.entry_point.is_empty() {
            "src/main.lua"
        } else {
            &manifest.entry_point
        };
        if archive.by_name(entry).is_err() {
            return Err(AppError::BadRequest(format!(
                "scripted plugins must include the entry file: {entry}"
            )));
        }
    }

    // 3. Check for plugin.sig (required for native plugins)
    let has_signature = archive.by_name("plugin.sig").is_ok();

    // 4. Extract icon if present (assets/icon.png)
    let icon_blob = match archive.by_name("assets/icon.png") {
        Ok(mut file) => {
            let mut buf = Vec::new();
            let _ = file.read_to_end(&mut buf);
            if buf.is_empty() {
                None
            } else {
                Some(buf)
            }
        }
        Err(_) => None,
    };

    Ok(ParsedBundle {
        manifest,
        icon_blob,
        has_signature,
    })
}

fn validate_manifest(manifest: &PluginManifest) -> Result<(), AppError> {
    if manifest.name.is_empty() {
        return Err(AppError::BadRequest(
            "manifest name is required".to_string(),
        ));
    }
    if manifest.name.len() > 100 {
        return Err(AppError::BadRequest(
            "manifest name must be 100 characters or less".to_string(),
        ));
    }

    if manifest.runtime != "scripted" && manifest.runtime != "native" {
        return Err(AppError::BadRequest(
            "runtime must be 'scripted' or 'native'".to_string(),
        ));
    }

    let valid_types = ["activity", "bot", "theme", "command"];
    if !valid_types.contains(&manifest.plugin_type.as_str()) {
        return Err(AppError::BadRequest(
            "type must be one of: activity, bot, theme, command".to_string(),
        ));
    }

    if let Some([w, h]) = manifest.canvas_size {
        if !(1..=1280).contains(&w) {
            return Err(AppError::BadRequest(
                "canvas_size width must be between 1 and 1280".to_string(),
            ));
        }
        if !(1..=720).contains(&h) {
            return Err(AppError::BadRequest(
                "canvas_size height must be between 1 and 720".to_string(),
            ));
        }
    }

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

async fn broadcast_plugin_event(
    state: &AppState,
    space_id: Option<&str>,
    target_user_ids: Option<Vec<String>>,
    event_type: &str,
    data: serde_json::Value,
) {
    if let Some(ref dispatcher) = *state.gateway_tx.read().await {
        let event = serde_json::json!({
            "op": 0,
            "type": event_type,
            "data": data,
        });
        let _ = dispatcher.send(GatewayBroadcast {
            space_id: space_id.map(|s| s.to_string()),
            target_user_ids,
            event,
            intent: "plugins".to_string(),
        });
    }
}
