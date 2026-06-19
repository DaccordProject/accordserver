//! Request-path forwards for **remote-homed** spaces, where this server is a
//! replica and the home server is authoritative.
//!
//! - `handle_send` (home side): a peer forwards one of its users' message
//!   actions; we re-run permissions locally (authority stays here), persist,
//!   broadcast, fan out, and return the authoritative message.
//! - `forward_message` / `initiate_join` (replica side): synchronous signed
//!   calls a local route makes to the home server.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::federation::mapping::RemoteUserRef;
use crate::federation::{authority, mapping, sender};
use crate::middleware::auth::AuthUser;
use crate::models::message::{CreateMessage, UpdateMessage};
use crate::state::AppState;

/// Path of the message-forward endpoint.
pub const SEND_PATH: &str = "/federation/v1/send";
/// Path of the reaction-forward endpoint.
pub const REACT_PATH: &str = "/federation/v1/react";
/// Path of the leave-forward endpoint.
pub const LEAVE_PATH: &str = "/federation/v1/leave";
/// Path of the message-edit-forward endpoint.
pub const EDIT_PATH: &str = "/federation/v1/edit";
/// Path of the message-delete-forward endpoint.
pub const DELETE_PATH: &str = "/federation/v1/delete";
/// Path of the typing-forward endpoint.
pub const TYPING_PATH: &str = "/federation/v1/typing";

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    /// The acting user (homed on the requesting peer).
    pub actor: RemoteUserRef,
    /// The home server's (bare) channel ID to post in.
    pub channel_id: String,
    pub content: String,
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// Some channel-permission helpers return an empty string for spaceless
/// channels; map that to `None` for gateway/fanout space scoping.
fn space_opt(space_id: &str) -> Option<String> {
    if space_id.is_empty() {
        None
    } else {
        Some(space_id.to_string())
    }
}

/// Synthetic `AuthUser` for a remote actor so the existing permission helpers
/// resolve their roles in our local space.
fn remote_actor_auth(user_id: &str) -> AuthUser {
    AuthUser {
        user_id: user_id.to_string(),
        is_bot: false,
        is_admin: false,
        is_guest: false,
        guest_space_id: None,
    }
}

/// Authoritative author-or-manage check for an edit/delete: the actor must be
/// the message's author, or hold `manage_messages` in its channel. Re-derived
/// from our own DB — never trusts the forwarded request.
async fn require_author_or_manage(
    state: &AppState,
    channel_id: &str,
    author_id: &str,
    actor_id: &str,
) -> Result<(), AppError> {
    if author_id != actor_id {
        let auth = remote_actor_auth(actor_id);
        crate::middleware::permissions::require_channel_permission(
            &state.db,
            channel_id,
            &auth,
            "manage_messages",
        )
        .await?;
    } else {
        crate::middleware::permissions::require_channel_membership(&state.db, channel_id, actor_id)
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Home side
// ---------------------------------------------------------------------------

pub async fn handle_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, SendRequest) =
        match crate::federation::verify::prepare(&state, &headers, SEND_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_send(&state, &our_domain, &peer.domain, &req).await {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_send(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &SendRequest,
) -> Result<serde_json::Value, AppError> {
    // Authority (S1): the actor must be homed on the signing peer.
    authority::require_homed_on(&req.actor.id, peer, "actor")?;

    // Input cap (S6).
    if req.content.chars().count() > 4000 {
        return Err(AppError::BadRequest("message content too long".to_string()));
    }

    // Cache the actor's profile (so the FK + member checks resolve).
    crate::db::users::upsert_remote_user(
        &state.db,
        &req.actor.id,
        peer,
        &mapping::handle(req.actor.username_or_id(), peer),
        req.actor.display_name.as_deref(),
        req.actor.avatar.as_deref(),
    )
    .await?;

    // Authoritative permission check: re-derived from OUR DB, never trusting the
    // request (S1). Also fails if the actor is not a member of the channel.
    let auth = remote_actor_auth(&req.actor.id);
    let space_id = crate::middleware::permissions::require_channel_permission(
        &state.db,
        &req.channel_id,
        &auth,
        "send_messages",
    )
    .await?;

    // Persist as a normal local message (this server homes the space).
    let msg = crate::db::messages::create_message(
        &state.db,
        &req.channel_id,
        &req.actor.id,
        Some(&space_id),
        &CreateMessage {
            content: req.content.clone(),
            tts: None,
            embeds: None,
            reply_to: req.reply_to.clone(),
            thread_id: None,
            title: None,
        },
    )
    .await?;

    let author = crate::db::users::get_user(&state.db, &req.actor.id).await?;
    let payload = crate::federation::outbound::message_payload(our_domain, &msg, &author);

    // Broadcast to OUR local gateway sessions (they key on the bare space id).
    crate::federation::broadcast_space(
        state,
        Some(space_id.clone()),
        "message.create",
        payload.clone(),
        "messages",
    )
    .await;

    // Fan out to every interested peer (including the originator, which dedups).
    crate::federation::outbound::fanout_message_create(state, &msg).await?;

    Ok(payload)
}

// --- Reactions ---

#[derive(Debug, Deserialize)]
pub struct ReactRequest {
    pub actor: RemoteUserRef,
    pub channel_id: String,
    pub message_id: String,
    pub emoji: String,
    #[serde(default)]
    pub remove: bool,
}

pub async fn handle_react(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, ReactRequest) =
        match crate::federation::verify::prepare(&state, &headers, REACT_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_react(&state, &our_domain, &peer.domain, &req).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "data": null }))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_react(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &ReactRequest,
) -> Result<(), AppError> {
    authority::require_homed_on(&req.actor.id, peer, "actor")?;
    crate::db::users::upsert_remote_user(
        &state.db,
        &req.actor.id,
        peer,
        &mapping::handle(req.actor.username_or_id(), peer),
        req.actor.display_name.as_deref(),
        req.actor.avatar.as_deref(),
    )
    .await?;

    let auth = remote_actor_auth(&req.actor.id);
    // Authoritative permission check from our own DB.
    let space_id = if req.remove {
        crate::middleware::permissions::require_channel_membership(
            &state.db,
            &req.channel_id,
            &req.actor.id,
        )
        .await?
    } else {
        crate::middleware::permissions::require_channel_permission(
            &state.db,
            &req.channel_id,
            &auth,
            "add_reactions",
        )
        .await?
    };

    // The target message must exist and live in the channel the permission check
    // was scoped to — never trust the forwarded message_id/channel_id pairing.
    let message = crate::db::messages::get_message_row(&state.db, &req.message_id).await?;
    if message.channel_id != req.channel_id {
        return Err(AppError::NotFound("unknown_message".to_string()));
    }

    if req.remove {
        crate::db::messages::remove_reaction(&state.db, &req.message_id, &req.actor.id, &req.emoji)
            .await?;
    } else {
        crate::db::messages::add_reaction(&state.db, &req.message_id, &req.actor.id, &req.emoji)
            .await?;
    }

    let payload = crate::federation::outbound::reaction_payload(
        our_domain,
        &req.channel_id,
        &req.message_id,
        &req.actor.id,
        &req.emoji,
    );
    let event_type = if req.remove {
        "m.reaction.remove"
    } else {
        "m.reaction.add"
    };
    let local_type = if req.remove {
        "reaction.remove"
    } else {
        "reaction.add"
    };

    // Broadcast to our local sessions and fan out to interested peers.
    crate::federation::broadcast_space(
        state,
        space_opt(&space_id),
        local_type,
        payload.clone(),
        "message_reactions",
    )
    .await;
    if !space_id.is_empty() {
        crate::federation::outbound::fanout_to_space(state, &space_id, event_type, payload).await?;
    }
    Ok(())
}

/// Forward a local user's reaction to the home server of a remote-homed space.
pub async fn forward_reaction(
    state: &AppState,
    home_domain: &str,
    channel_id: &str,
    message_id: &str,
    actor: &crate::models::user::User,
    emoji: &str,
    remove: bool,
) -> Result<(), AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;
    let body = serde_json::to_vec(&json!({
        "actor": actor_ref(&fed.domain, actor),
        "channel_id": mapping::local_part(channel_id),
        "message_id": mapping::local_part(message_id),
        "emoji": emoji,
        "remove": remove,
    }))
    .map_err(|e| AppError::Internal(format!("serialize react: {e}")))?;

    let (status, _bytes) = sender::request_signed(state, home_domain, REACT_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected reaction ({status})"
        )));
    }
    Ok(())
}

// --- Member leave ---

#[derive(Debug, Deserialize)]
pub struct LeaveRequest {
    pub actor: RemoteUserRef,
    /// The home server's (bare) space ID to leave.
    pub space_id: String,
}

pub async fn handle_leave(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, LeaveRequest) =
        match crate::federation::verify::prepare(&state, &headers, LEAVE_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_leave(&state, &our_domain, &peer.domain, &req).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "data": null }))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_leave(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &LeaveRequest,
) -> Result<(), AppError> {
    authority::require_homed_on(&req.actor.id, peer, "actor")?;
    // The space must be homed here.
    crate::db::spaces::get_space_row(&state.db, &req.space_id).await?;

    // Capture interested peers before removal so the departing user's own home
    // server is still notified even if they were its last member here.
    let fanout_targets = crate::db::federation::interested_servers(&state.db, &req.space_id)
        .await
        .unwrap_or_default();

    crate::db::members::remove_member(&state.db, &req.space_id, &req.actor.id).await?;

    // Broadcast locally and fan the departure out to remaining interested peers.
    crate::federation::broadcast_space(
        state,
        Some(req.space_id.clone()),
        "member.leave",
        json!({ "space_id": req.space_id, "user_id": req.actor.id }),
        "members",
    )
    .await;
    let payload = crate::federation::outbound::member_leave_payload(our_domain, &req.actor.id);
    crate::federation::outbound::fanout_to_targets(
        state,
        &req.space_id,
        "m.member.leave",
        payload,
        &fanout_targets,
    )
    .await?;
    Ok(())
}

/// Forward a local user's departure from a remote-homed space to the home server.
pub async fn forward_leave(
    state: &AppState,
    home_domain: &str,
    space_id: &str,
    actor: &crate::models::user::User,
) -> Result<(), AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;
    let body = serde_json::to_vec(&json!({
        "actor": actor_ref(&fed.domain, actor),
        "space_id": mapping::local_part(space_id),
    }))
    .map_err(|e| AppError::Internal(format!("serialize leave: {e}")))?;

    let (status, _bytes) = sender::request_signed(state, home_domain, LEAVE_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected leave ({status})"
        )));
    }
    Ok(())
}

// --- Message edit ---

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    pub actor: RemoteUserRef,
    /// The home server's (bare) message ID to edit.
    pub message_id: String,
    pub content: String,
}

pub async fn handle_edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, EditRequest) =
        match crate::federation::verify::prepare(&state, &headers, EDIT_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_edit(&state, &our_domain, &peer.domain, &req).await {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_edit(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &EditRequest,
) -> Result<serde_json::Value, AppError> {
    authority::require_homed_on(&req.actor.id, peer, "actor")?;
    if req.content.chars().count() > 4000 {
        return Err(AppError::BadRequest("message content too long".to_string()));
    }
    let existing = crate::db::messages::get_message_row(&state.db, &req.message_id).await?;
    // Authoritative author-or-manage check from our DB.
    require_author_or_manage(state, &existing.channel_id, &existing.author_id, &req.actor.id)
        .await?;

    let msg = crate::db::messages::update_message(
        &state.db,
        &req.message_id,
        &UpdateMessage {
            content: Some(req.content.clone()),
            embeds: None,
            title: None,
        },
        state.db_is_postgres,
    )
    .await?;
    let payload = crate::routes::messages::message_row_to_json_with_attachments(&msg, &[], None);

    crate::federation::broadcast_space(
        state,
        existing.space_id.clone(),
        "message.update",
        payload.clone(),
        "messages",
    )
    .await;
    if let Some(sid) = &existing.space_id {
        let fanout = json!({
            "id": mapping::qualify(&req.message_id, our_domain),
            "content": msg.content,
            "edited_at": msg.edited_at,
        });
        crate::federation::outbound::fanout_to_space(state, sid, "m.message.update", fanout)
            .await?;
    }
    Ok(payload)
}

/// Forward a local user's edit of their message in a remote-homed space.
pub async fn forward_edit(
    state: &AppState,
    home_domain: &str,
    message_id: &str,
    actor: &crate::models::user::User,
    content: &str,
) -> Result<serde_json::Value, AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;
    let body = serde_json::to_vec(&json!({
        "actor": actor_ref(&fed.domain, actor),
        "message_id": mapping::local_part(message_id),
        "content": content,
    }))
    .map_err(|e| AppError::Internal(format!("serialize edit: {e}")))?;
    let (status, bytes) = sender::request_signed(state, home_domain, EDIT_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected edit ({status})"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("invalid home response: {e}")))
}

// --- Message delete ---

#[derive(Debug, Deserialize)]
pub struct DeleteRequest {
    pub actor: RemoteUserRef,
    pub message_id: String,
}

pub async fn handle_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, DeleteRequest) =
        match crate::federation::verify::prepare(&state, &headers, DELETE_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_delete(&state, &our_domain, &peer.domain, &req).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "data": null }))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_delete(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &DeleteRequest,
) -> Result<(), AppError> {
    authority::require_homed_on(&req.actor.id, peer, "actor")?;
    let existing = crate::db::messages::get_message_row(&state.db, &req.message_id).await?;
    require_author_or_manage(state, &existing.channel_id, &existing.author_id, &req.actor.id)
        .await?;

    crate::db::messages::delete_message(&state.db, &req.message_id).await?;

    let data = json!({
        "id": mapping::qualify(&req.message_id, our_domain),
        "channel_id": mapping::qualify(&existing.channel_id, our_domain),
    });
    crate::federation::broadcast_space(
        state,
        existing.space_id.clone(),
        "message.delete",
        data.clone(),
        "messages",
    )
    .await;
    if let Some(sid) = &existing.space_id {
        crate::federation::outbound::fanout_to_space(state, sid, "m.message.delete", data).await?;
    }
    Ok(())
}

/// Forward a local user's deletion of their message in a remote-homed space.
pub async fn forward_delete(
    state: &AppState,
    home_domain: &str,
    message_id: &str,
    actor: &crate::models::user::User,
) -> Result<(), AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;
    let body = serde_json::to_vec(&json!({
        "actor": actor_ref(&fed.domain, actor),
        "message_id": mapping::local_part(message_id),
    }))
    .map_err(|e| AppError::Internal(format!("serialize delete: {e}")))?;
    let (status, _bytes) = sender::request_signed(state, home_domain, DELETE_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected delete ({status})"
        )));
    }
    Ok(())
}

// --- Typing ---

#[derive(Debug, Deserialize)]
pub struct TypingRequest {
    pub actor: RemoteUserRef,
    pub channel_id: String,
}

pub async fn handle_typing(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, TypingRequest) =
        match crate::federation::verify::prepare(&state, &headers, TYPING_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_typing(&state, &our_domain, &peer.domain, &req).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "data": null }))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_typing(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &TypingRequest,
) -> Result<(), AppError> {
    authority::require_homed_on(&req.actor.id, peer, "actor")?;
    let space_id = crate::middleware::permissions::require_channel_membership(
        &state.db,
        &req.channel_id,
        &req.actor.id,
    )
    .await?;
    let data = json!({
        "channel_id": mapping::qualify(&req.channel_id, our_domain),
        "user_id": req.actor.id,
    });
    crate::federation::broadcast_space(
        state,
        space_opt(&space_id),
        "typing.start",
        data.clone(),
        "message_typing",
    )
    .await;
    if !space_id.is_empty() {
        crate::federation::outbound::fanout_to_space(state, &space_id, "m.typing", data).await?;
    }
    Ok(())
}

/// Forward a typing indicator for a remote-homed space (best-effort).
pub async fn forward_typing(
    state: &AppState,
    home_domain: &str,
    channel_id: &str,
    actor: &crate::models::user::User,
) -> Result<(), AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;
    let body = serde_json::to_vec(&json!({
        "actor": actor_ref(&fed.domain, actor),
        "channel_id": mapping::local_part(channel_id),
    }))
    .map_err(|e| AppError::Internal(format!("serialize typing: {e}")))?;
    let _ = sender::request_signed(state, home_domain, TYPING_PATH, &body).await?;
    Ok(())
}

/// Build the qualified `actor` object sent in forward requests.
fn actor_ref(domain: &str, user: &crate::models::user::User) -> serde_json::Value {
    json!({
        "id": mapping::qualify(&user.id, domain),
        "username": user.username,
        "display_name": user.display_name,
        "avatar": user.avatar,
    })
}

// ---------------------------------------------------------------------------
// Replica side (called from local REST routes)
// ---------------------------------------------------------------------------

/// Forward a local user's message to the home server of a remote-homed space
/// and return the authoritative message object. `home_domain` is the space's
/// home; `channel_id`/`reply_to` are the qualified replica IDs (their local
/// parts are sent to the home server).
pub async fn forward_message(
    state: &AppState,
    home_domain: &str,
    channel_id: &str,
    author: &crate::models::user::User,
    content: &str,
    reply_to: Option<&str>,
) -> Result<serde_json::Value, AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;

    let body = serde_json::to_vec(&json!({
        "actor": actor_ref(&fed.domain, author),
        "channel_id": mapping::local_part(channel_id),
        "content": content,
        "reply_to": reply_to.map(mapping::local_part),
    }))
    .map_err(|e| AppError::Internal(format!("serialize send: {e}")))?;

    let (status, bytes) = sender::request_signed(state, home_domain, SEND_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected message ({status})"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("invalid home response: {e}")))
}

/// Initiate a federated join: ask `home_domain` to add `user` to its `space_id`
/// (the home server's bare ID), then apply the returned snapshot locally.
/// Returns the mirrored (qualified) space ID.
pub async fn initiate_join(
    state: &AppState,
    home_domain: &str,
    home_space_id: &str,
    user: &crate::models::user::User,
) -> Result<String, AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::Internal("federation disabled".to_string()))?;

    let body = serde_json::to_vec(&json!({
        "user": actor_ref(&fed.domain, user),
        "space_id": home_space_id,
    }))
    .map_err(|e| AppError::Internal(format!("serialize join: {e}")))?;

    let (status, bytes) = sender::request_signed(
        state,
        home_domain,
        crate::federation::handshake::JOIN_PATH,
        &body,
    )
    .await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected join ({status})"
        )));
    }
    let snapshot: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("invalid snapshot: {e}")))?;

    crate::federation::handshake::apply_snapshot(state, home_domain, &user.id, snapshot).await
}
