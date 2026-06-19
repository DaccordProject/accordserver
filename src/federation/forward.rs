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
use crate::federation::handshake::RemoteUserRef;
use crate::federation::{authority, mapping, sender};
use crate::middleware::auth::AuthUser;
use crate::models::message::CreateMessage;
use crate::state::AppState;

/// Path of the message-forward endpoint.
pub const SEND_PATH: &str = "/federation/v1/send";

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

fn err(status: StatusCode, msg: &str) -> axum::response::Response {
    (status, Json(json!({ "error": msg }))).into_response()
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

// ---------------------------------------------------------------------------
// Home side
// ---------------------------------------------------------------------------

pub async fn handle_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let Some(fed) = state.federation.clone() else {
        return err(StatusCode::NOT_FOUND, "federation disabled");
    };

    let peer = match crate::federation::verify::verify_signed(
        &state,
        &fed.domain,
        &headers,
        SEND_PATH,
        &body,
    )
    .await
    {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let req: SendRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return err(StatusCode::BAD_REQUEST, "invalid send request"),
    };

    match serve_send(&state, &fed.domain, &peer.domain, &req).await {
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
        &mapping::handle(&req.actor.username, peer),
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
    if let Some(dispatcher) = state.gateway_tx.read().await.as_ref() {
        let event = json!({ "op": 0, "type": "message.create", "data": payload });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: Some(space_id.clone()),
            target_user_ids: None,
            event,
            intent: "messages".to_string(),
        });
    }

    // Fan out to every interested peer (including the originator, which dedups).
    crate::federation::outbound::fanout_message_create(state, &msg).await?;

    Ok(payload)
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
        "actor": {
            "id": mapping::qualify(&author.id, &fed.domain),
            "username": author.username,
            "display_name": author.display_name,
            "avatar": author.avatar,
        },
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
        "user": {
            "id": mapping::qualify(&user.id, &fed.domain),
            "username": user.username,
            "display_name": user.display_name,
            "avatar": user.avatar,
        },
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
