//! Cross-server direct messages (Phase 1: 1:1 DMs).
//!
//! DMs have no space, so the space-federation model (authority = home server of
//! a space) does not apply directly. Instead each DM is anchored on a
//! deterministic **home server**, chosen from the two participants' qualified
//! IDs so both servers independently agree on the same home. After that a DM
//! behaves like a space:
//!   - the home server holds the authoritative channel (`origin IS NULL`),
//!   - the other server mirrors it as a replica (`origin = <home>`),
//!   - sends from the replica are forwarded to the home, which fans the message
//!     back out to the other participant's server.
//!
//! Consent (block lists) is always enforced by the *recipient's* own server,
//! which is whichever side hosts the non-initiating participant.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::AppError;
use crate::federation::mapping::RemoteUserRef;
use crate::federation::{authority, mapping, sender};
use crate::models::channel::ChannelRow;
use crate::models::message::CreateMessage;
use crate::models::user::User;
use crate::state::AppState;

/// Home side: a remote server asks us (the home) to open a DM with one of our
/// local users.
pub const DM_OPEN_PATH: &str = "/federation/v1/dm/open";
/// Replica side: the home tells the other participant's server to mirror a DM
/// it just created.
pub const DM_ANNOUNCE_PATH: &str = "/federation/v1/dm/announce";
/// Home side: a replica forwards one of its users' DM messages to us (the home).
pub const DM_SEND_PATH: &str = "/federation/v1/dm/send";

const MAX_CONTENT_CHARS: usize = 4000;

/// Pick the deterministic home domain for a DM between two qualified user IDs.
/// Both servers compute the same value, so the DM is created exactly once.
fn home_domain_for(a_qualified: &str, b_qualified: &str) -> String {
    let (a, b) = (a_qualified.to_ascii_lowercase(), b_qualified.to_ascii_lowercase());
    let lower = if a <= b { &a } else { &b };
    mapping::domain_of(lower).unwrap_or(lower).to_string()
}

/// The wire form of a DM, exchanged on open/announce so the non-home side can
/// mirror it. All IDs are fully qualified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmSnapshot {
    /// Qualified DM channel ID, homed on `home`.
    pub channel_id: String,
    pub home: String,
    pub channel_type: String,
    /// Qualified ID of the participant who initiated the DM (never needs consent).
    pub opener_id: String,
    pub owner_id: String,
    pub participants: Vec<RemoteUserRef>,
}

// ---------------------------------------------------------------------------
// Replica/opener side: open a DM whose other participant is remote
// ---------------------------------------------------------------------------

/// Open (or reuse) a DM between a local `opener` and a remote `recipient_id`
/// (a qualified `<snowflake>@<domain>`). Returns the local `ChannelRow` the
/// caller should surface — either the authoritative home channel (if we are the
/// home) or the mirrored replica.
pub async fn open_dm(
    state: &AppState,
    opener: &User,
    recipient_id: &str,
) -> Result<ChannelRow, AppError> {
    let fed = state
        .federation
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("federation is not enabled".to_string()))?;
    let our_domain = fed.domain.clone();

    let opener_qualified = mapping::qualify(&opener.id, &our_domain);
    authority::require_remote_target(recipient_id)?; // must be a qualified remote ID
    let recipient_domain = mapping::domain_of(recipient_id)
        .ok_or_else(|| AppError::BadRequest("recipient must be a qualified id".to_string()))?;

    let home = home_domain_for(&opener_qualified, recipient_id);

    if home.eq_ignore_ascii_case(&our_domain) {
        // We are the home: create locally, then announce to the recipient's
        // server so it can mirror and enforce its user's consent.
        open_dm_as_home(state, &our_domain, opener, recipient_id, recipient_domain).await
    } else {
        // Home is the recipient's server: forward the open and mirror the
        // snapshot it returns.
        open_dm_as_replica(state, &our_domain, opener, recipient_id, &home).await
    }
}

async fn open_dm_as_home(
    state: &AppState,
    our_domain: &str,
    opener: &User,
    recipient_id: &str,
    recipient_domain: &str,
) -> Result<ChannelRow, AppError> {
    // Cache a minimal profile for the remote recipient so the FK + participant
    // rows resolve.
    crate::db::users::upsert_remote_user(
        &state.db,
        recipient_id,
        recipient_domain,
        recipient_id,
        None,
        None,
    )
    .await?;

    let channel = crate::db::dm_participants::create_dm_channel(
        &state.db,
        &opener.id,
        &[recipient_id.to_string()],
        state.db_is_postgres,
    )
    .await?;

    let snapshot = DmSnapshot {
        channel_id: mapping::qualify(&channel.id, our_domain),
        home: our_domain.to_string(),
        channel_type: channel.channel_type.clone(),
        opener_id: mapping::qualify(&opener.id, our_domain),
        owner_id: mapping::qualify(channel.owner_id.as_deref().unwrap_or(&opener.id), our_domain),
        participants: vec![
            actor_ref(our_domain, opener),
            RemoteUserRef {
                id: recipient_id.to_string(),
                username: Some(recipient_id.to_string()),
                display_name: None,
                avatar: None,
            },
        ],
    };

    // Announce to the recipient's server; it enforces the recipient's consent.
    // If it rejects, roll back so we don't leave a half-open DM.
    let body = serde_json::to_vec(&snapshot)
        .map_err(|e| AppError::Internal(format!("serialize dm announce: {e}")))?;
    let (status, bytes) =
        sender::request_signed(state, recipient_domain, DM_ANNOUNCE_PATH, &body).await?;
    if !status.is_success() {
        let _ = crate::db::channels::delete_channel(&state.db, &channel.id).await;
        let reason = String::from_utf8_lossy(&bytes);
        return Err(AppError::Forbidden(format!(
            "recipient server rejected the DM: {reason}"
        )));
    }

    broadcast_channel_create(state, &channel).await;
    Ok(channel)
}

async fn open_dm_as_replica(
    state: &AppState,
    our_domain: &str,
    opener: &User,
    recipient_id: &str,
    home: &str,
) -> Result<ChannelRow, AppError> {
    let body = serde_json::to_vec(&json!({
        "opener": actor_ref(our_domain, opener),
        "recipient_id": recipient_id,
    }))
    .map_err(|e| AppError::Internal(format!("serialize dm open: {e}")))?;

    let (status, bytes) = sender::request_signed(state, home, DM_OPEN_PATH, &body).await?;
    if !status.is_success() {
        let reason = String::from_utf8_lossy(&bytes);
        return Err(AppError::BadRequest(format!(
            "home server rejected the DM open: {reason}"
        )));
    }
    let snapshot: DmSnapshot = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("invalid dm snapshot: {e}")))?;

    mirror_dm(state, our_domain, &snapshot).await?;
    crate::db::channels::get_channel_row(&state.db, &snapshot.channel_id).await
}

// ---------------------------------------------------------------------------
// Home side: serve an open request from a remote opener
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DmOpenRequest {
    opener: RemoteUserRef,
    /// Qualified ID of the recipient — must be one of our local users.
    recipient_id: String,
}

pub async fn handle_open(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, DmOpenRequest) =
        match crate::federation::verify::prepare(&state, &headers, DM_OPEN_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    match serve_open(&state, &our_domain, &peer.domain, &req).await {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn serve_open(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    req: &DmOpenRequest,
) -> Result<DmSnapshot, AppError> {
    // Authority (S1): the opener must be homed on the signing peer.
    authority::require_homed_on(&req.opener.id, peer, "opener")?;

    // The recipient must be one of our local users.
    if !mapping::is_local(&req.recipient_id, our_domain) {
        return Err(AppError::BadRequest(
            "recipient is not homed on this server".to_string(),
        ));
    }
    let recipient_local = mapping::local_part(&req.recipient_id).to_string();
    let recipient = crate::db::users::get_user(&state.db, &recipient_local).await?;

    // Consent: the recipient must not have blocked the opener.
    if crate::db::relationships::is_blocked_by(&state.db, &recipient.id, &req.opener.id).await? {
        return Err(AppError::Forbidden(
            "recipient is not accepting DMs from this user".to_string(),
        ));
    }

    // Cache the opener and open/reuse the authoritative DM.
    crate::db::users::upsert_remote_user(
        &state.db,
        &req.opener.id,
        peer,
        &mapping::handle(req.opener.username_or_id(), peer),
        req.opener.display_name.as_deref(),
        req.opener.avatar.as_deref(),
    )
    .await?;
    let channel = crate::db::dm_participants::create_dm_channel(
        &state.db,
        &req.opener.id,
        std::slice::from_ref(&recipient.id),
        state.db_is_postgres,
    )
    .await?;

    broadcast_channel_create(state, &channel).await;

    Ok(DmSnapshot {
        channel_id: mapping::qualify(&channel.id, our_domain),
        home: our_domain.to_string(),
        channel_type: channel.channel_type.clone(),
        opener_id: req.opener.id.clone(),
        owner_id: mapping::qualify(
            channel.owner_id.as_deref().unwrap_or(&req.opener.id),
            our_domain,
        ),
        participants: vec![
            req.opener.clone(),
            actor_ref(our_domain, &recipient),
        ],
    })
}

// ---------------------------------------------------------------------------
// Replica side: mirror an announced DM
// ---------------------------------------------------------------------------

pub async fn handle_announce(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, snapshot): (_, _, DmSnapshot) =
        match crate::federation::verify::prepare(&state, &headers, DM_ANNOUNCE_PATH, &body).await {
            Ok(t) => t,
            Err(resp) => return resp,
        };
    // Authority: the channel must be homed on the signing peer.
    if let Err(e) = authority::require_homed_on(&snapshot.channel_id, &peer.domain, "dm channel") {
        return e.into_response();
    }
    match mirror_dm(&state, &our_domain, &snapshot).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "data": null }))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Mirror an announced/returned DM snapshot as a local replica. Enforces consent
/// for any local participant that is not the opener, upserts remote participants,
/// stores the replica channel + membership, and notifies local sessions.
async fn mirror_dm(state: &AppState, our_domain: &str, snap: &DmSnapshot) -> Result<(), AppError> {
    // First pass: validate consent and ensure every participant user row exists.
    for p in &snap.participants {
        if mapping::is_local(&p.id, our_domain) {
            let local_id = mapping::local_part(&p.id).to_string();
            if !p.id.eq_ignore_ascii_case(&snap.opener_id) {
                // A local, non-initiating participant must consent to the DM.
                if crate::db::relationships::is_blocked_by(&state.db, &local_id, &snap.opener_id)
                    .await?
                {
                    return Err(AppError::Forbidden(
                        "recipient is not accepting DMs from this user".to_string(),
                    ));
                }
            }
        } else {
            let domain = mapping::domain_of(&p.id).unwrap_or(&snap.home);
            crate::db::users::upsert_remote_user(
                &state.db,
                &p.id,
                domain,
                &mapping::handle(p.username_or_id(), domain),
                p.display_name.as_deref(),
                p.avatar.as_deref(),
            )
            .await?;
        }
    }

    // Owner row must exist before the channel (FK). Localise so it matches a
    // stored user id (bare for local, qualified for remote).
    let owner_storage_id = participant_storage_id(&snap.owner_id, our_domain);
    crate::db::federation::upsert_remote_dm_channel(
        &state.db,
        &snap.channel_id,
        &snap.home,
        &snap.channel_type,
        &owner_storage_id,
    )
    .await?;

    // Membership uses the locally-stored id form for each participant.
    for p in &snap.participants {
        let storage_id = participant_storage_id(&p.id, our_domain);
        crate::db::dm_participants::add_participant(
            &state.db,
            &snap.channel_id,
            &storage_id,
            state.db_is_postgres,
        )
        .await?;
    }

    if let Ok(channel) = crate::db::channels::get_channel_row(&state.db, &snap.channel_id).await {
        broadcast_channel_create(state, &channel).await;
    }
    Ok(())
}

/// The id under which a (possibly qualified) participant is stored locally:
/// the bare snowflake for our own users, the qualified id for remote users.
fn participant_storage_id(qualified_id: &str, our_domain: &str) -> String {
    if mapping::is_local(qualified_id, our_domain) {
        mapping::local_part(qualified_id).to_string()
    } else {
        qualified_id.to_string()
    }
}

// ---------------------------------------------------------------------------
// Messaging: send on the home, forward from a replica, apply inbound
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DmSendRequest {
    actor: RemoteUserRef,
    /// The home server's (bare) DM channel ID.
    channel_id: String,
    content: String,
    #[serde(default)]
    reply_to: Option<String>,
}

pub async fn handle_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let (our_domain, peer, req): (_, _, DmSendRequest) =
        match crate::federation::verify::prepare(&state, &headers, DM_SEND_PATH, &body).await {
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
    req: &DmSendRequest,
) -> Result<serde_json::Value, AppError> {
    authority::require_homed_on(&req.actor.id, peer, "actor")?;
    if req.content.chars().count() > MAX_CONTENT_CHARS {
        return Err(AppError::BadRequest("message content too long".to_string()));
    }

    // The channel must be a DM we home, and the actor must be a participant.
    let channel = crate::db::channels::get_channel_row(&state.db, &req.channel_id).await?;
    if !is_dm(&channel.channel_type) {
        return Err(AppError::BadRequest("not a dm channel".to_string()));
    }
    if !crate::db::dm_participants::is_participant(&state.db, &req.channel_id, &req.actor.id).await? {
        return Err(AppError::Forbidden(
            "actor is not a participant in this dm".to_string(),
        ));
    }

    crate::db::users::upsert_remote_user(
        &state.db,
        &req.actor.id,
        peer,
        &mapping::handle(req.actor.username_or_id(), peer),
        req.actor.display_name.as_deref(),
        req.actor.avatar.as_deref(),
    )
    .await?;

    let msg = crate::db::messages::create_message(
        &state.db,
        &req.channel_id,
        &req.actor.id,
        None,
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
    // Qualified payload for the originating replica + peer fanout; bare-ID JSON
    // for our own local sessions (which know this DM by its bare home ID).
    let payload = crate::federation::outbound::message_payload(our_domain, &msg, &author);
    let local_json =
        crate::routes::messages::message_row_to_json_with_attachments(&msg, &[], None);

    broadcast_message(state, &req.channel_id, "message.create", local_json).await;
    fanout_dm_message(state, &channel, &payload).await?;
    Ok(payload)
}

/// Forward a local user's DM message to the home server of a remote-homed DM,
/// returning the authoritative message object.
pub async fn forward_dm_message(
    state: &AppState,
    home: &str,
    channel_id: &str,
    author: &User,
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
    .map_err(|e| AppError::Internal(format!("serialize dm send: {e}")))?;
    let (status, bytes) = sender::request_signed(state, home, DM_SEND_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "home server rejected dm message ({status})"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("invalid home response: {e}")))
}

/// Fan a locally-homed DM message out to the servers of its remote participants.
pub async fn fanout_dm_message(
    state: &AppState,
    channel: &ChannelRow,
    payload: &serde_json::Value,
) -> Result<(), AppError> {
    let Some(fed) = state.federation.as_ref() else {
        return Ok(());
    };
    let targets = remote_participant_domains(state, &channel.id, &fed.domain).await?;
    if targets.is_empty() {
        return Ok(());
    }
    let envelope = mapping::FederationEnvelope::new(
        crate::snowflake::generate(),
        fed.domain.clone(),
        None, // DMs have no space
        "m.dm.message.create",
        payload.clone(),
    );
    sender::enqueue(state, &envelope, &targets).await
}

/// Apply an inbound `m.dm.message.create` (called from the inbox applier). The
/// message is stored as a replica and delivered to local participants.
pub async fn apply_message_create(
    state: &AppState,
    peer: &str,
    env: &mapping::FederationEnvelope,
) -> Result<(), AppError> {
    let payload: crate::federation::apply::RemoteMessagePayload =
        serde_json::from_value(env.payload.clone())
            .map_err(|e| AppError::BadRequest(format!("invalid dm message payload: {e}")))?;

    // Authority: channel + message are homed on the signing peer; the author may
    // be remote but must be a qualified id (never a bare local row — S2).
    authority::require_homed_on(&payload.id, peer, "message")?;
    authority::require_homed_on(&payload.channel_id, peer, "dm channel")?;
    authority::require_remote_target(&payload.author.id)?;

    if payload.content.chars().count() > MAX_CONTENT_CHARS {
        return Err(AppError::BadRequest("dm message too long".to_string()));
    }

    // We must already mirror this DM channel; otherwise we are not a participant.
    if crate::db::channels::get_channel_row(&state.db, &payload.channel_id)
        .await
        .is_err()
    {
        return Ok(());
    }

    let author_domain = mapping::domain_of(&payload.author.id).unwrap_or(peer);
    let handle = mapping::handle(payload.author.username_or_id(), author_domain);
    crate::db::users::upsert_remote_user(
        &state.db,
        &payload.author.id,
        author_domain,
        &handle,
        payload.author.display_name.as_deref(),
        payload.author.avatar.as_deref(),
    )
    .await?;

    let mentions_json =
        serde_json::to_string(&payload.mentions).unwrap_or_else(|_| "[]".to_string());
    let embeds_json = serde_json::to_string(&payload.embeds).unwrap_or_else(|_| "[]".to_string());
    let insert = crate::db::messages::RemoteMessageInsert {
        id: &payload.id,
        channel_id: &payload.channel_id,
        space_id: None,
        author_id: &payload.author.id,
        content: &payload.content,
        created_at: &payload.created_at,
        mention_everyone: payload.mention_everyone,
        mentions_json: &mentions_json,
        embeds_json: &embeds_json,
        reply_to: payload.reply_to.as_deref(),
        origin: peer,
    };
    let Some(row) = crate::db::messages::insert_remote_message(&state.db, &insert).await? else {
        return Ok(()); // duplicate delivery
    };

    let json = crate::routes::messages::message_row_to_json_with_attachments(&row, &[], None);
    broadcast_message(state, &payload.channel_id, "message.create", json).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// True for 1:1 and group DM channel types.
pub fn is_dm(channel_type: &str) -> bool {
    channel_type == "dm" || channel_type == "group_dm"
}

fn actor_ref(domain: &str, user: &User) -> RemoteUserRef {
    RemoteUserRef {
        id: mapping::qualify(&user.id, domain),
        username: Some(user.username.clone()),
        display_name: user.display_name.clone(),
        avatar: user.avatar.clone(),
    }
}

/// The distinct domains of a DM's participants other than our own — the fanout
/// targets for a locally-homed DM.
async fn remote_participant_domains(
    state: &AppState,
    channel_id: &str,
    our_domain: &str,
) -> Result<Vec<String>, AppError> {
    let ids = crate::db::dm_participants::list_participant_ids(&state.db, channel_id).await?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if let Some(domain) = mapping::domain_of(&id) {
            if !domain.eq_ignore_ascii_case(our_domain) && seen.insert(domain.to_ascii_lowercase()) {
                out.push(domain.to_string());
            }
        }
    }
    Ok(out)
}

/// Broadcast a DM channel event to its local participants (DMs have no space, so
/// delivery targets participant user IDs).
async fn broadcast_channel_create(state: &AppState, channel: &ChannelRow) {
    let json = crate::routes::spaces::channel_row_to_json_pub(&state.db, channel).await;
    broadcast_to_participants(state, &channel.id, "channel.create", json, "channels").await;
}

/// Broadcast a DM message event to the channel's local participants.
async fn broadcast_message(
    state: &AppState,
    channel_id: &str,
    event_type: &str,
    data: serde_json::Value,
) {
    broadcast_to_participants(state, channel_id, event_type, data, "messages").await;
}

async fn broadcast_to_participants(
    state: &AppState,
    channel_id: &str,
    event_type: &str,
    data: serde_json::Value,
    intent: &str,
) {
    let participant_ids = crate::db::dm_participants::list_participant_ids(&state.db, channel_id)
        .await
        .unwrap_or_default();
    if participant_ids.is_empty() {
        return;
    }
    if let Some(dispatcher) = state.gateway_tx.read().await.as_ref() {
        let event = json!({ "op": 0, "type": event_type, "data": data });
        let _ = dispatcher.send(crate::gateway::events::GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(participant_ids),
            event,
            intent: intent.to_string(),
        });
    }
}
