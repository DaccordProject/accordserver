//! Federated join handshake (Phase 3).
//!
//! When a user on server A joins a space homed on server B:
//!   1. A sends a signed `POST /federation/v1/join` to B with the joining user.
//!   2. B (home/authoritative) validates federation opt-in + bans, adds the
//!      remote user as a member of its local space, and returns a **snapshot**
//!      (space, channels, roles, members, recent messages) — all IDs qualified
//!      to B's domain.
//!   3. A applies the snapshot as a local replica (`origin = B`) and records the
//!      joining local user as a member so the space appears in their gateway/READY.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::AppError;
use crate::federation::{authority, mapping};
use crate::state::AppState;

/// Path of the join endpoint, also the signed `(request-target)`.
pub const JOIN_PATH: &str = "/federation/v1/join";

/// Recent messages included per channel in a join snapshot.
const SNAPSHOT_MESSAGES_PER_CHANNEL: i64 = 50;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteUserRef {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    /// The joining user (homed on the requesting peer).
    pub user: RemoteUserRef,
    /// The home server's (bare) space ID to join.
    pub space_id: String,
}

fn err(status: StatusCode, msg: &str) -> axum::response::Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

// ---------------------------------------------------------------------------
// Home side: serve a join request
// ---------------------------------------------------------------------------

pub async fn handle_join(
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
        JOIN_PATH,
        &body,
    )
    .await
    {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let join: JoinRequest = match serde_json::from_slice(&body) {
        Ok(j) => j,
        Err(_) => return err(StatusCode::BAD_REQUEST, "invalid join request"),
    };

    // Authority (S1): the joining user must be homed on the signing peer.
    if let Err(e) = authority::require_homed_on(&join.user.id, &peer.domain, "user") {
        tracing::warn!("join authority check failed from {}: {e}", peer.domain);
        return err(StatusCode::FORBIDDEN, "authority check failed");
    }

    match serve_join(&state, &fed.domain, &peer.domain, &join).await {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Validate and record the join on the home server, returning the snapshot.
async fn serve_join(
    state: &AppState,
    our_domain: &str,
    peer: &str,
    join: &JoinRequest,
) -> Result<serde_json::Value, AppError> {
    // The space must exist locally and be opted in to federation (S9).
    let space = crate::db::spaces::get_space_row(&state.db, &join.space_id).await?;
    if !crate::db::federation::space_federation_enabled(&state.db, &join.space_id).await? {
        return Err(AppError::Forbidden("space is not federated".to_string()));
    }

    // Reject banned users (reuses local ban state).
    if crate::db::bans::get_ban(&state.db, &join.space_id, &join.user.id)
        .await
        .is_ok()
    {
        return Err(AppError::Forbidden(
            "user is banned from this space".to_string(),
        ));
    }

    // Mirror the remote user and add them to the space (origin = their home).
    crate::db::users::upsert_remote_user(
        &state.db,
        &join.user.id,
        peer,
        &mapping::handle(&join.user.username, peer),
        join.user.display_name.as_deref(),
        join.user.avatar.as_deref(),
    )
    .await?;
    crate::db::federation::add_member_with_origin(
        &state.db,
        &join.space_id,
        &join.user.id,
        Some(peer),
    )
    .await?;

    build_snapshot(state, our_domain, &space).await
}

/// Build a join snapshot with every ID qualified to `our_domain`.
async fn build_snapshot(
    state: &AppState,
    our_domain: &str,
    space: &crate::models::space::SpaceRow,
) -> Result<serde_json::Value, AppError> {
    let q = |id: &str| mapping::qualify(id, our_domain);

    let channels = crate::db::channels::list_channels_in_space(&state.db, &space.id).await?;
    let roles = crate::db::roles::list_roles(&state.db, &space.id).await?;

    let channels_json: Vec<serde_json::Value> = channels
        .iter()
        .map(|c| {
            json!({
                "id": q(&c.id),
                "space_id": c.space_id.as_deref().map(q),
                "name": c.name,
                "type": c.channel_type,
                "position": c.position,
            })
        })
        .collect();

    let roles_json: Vec<serde_json::Value> = roles
        .iter()
        .map(|r| {
            json!({
                "id": q(&r.id),
                "space_id": q(&r.space_id),
                "name": r.name,
                "position": r.position,
                "permissions": r.permissions,
            })
        })
        .collect();

    // Members (id qualified; username sent as a fully-qualified handle).
    let member_rows = sqlx::query(&crate::db::q(
        "SELECT u.id, u.username, u.display_name, u.avatar, u.origin FROM members m \
         JOIN users u ON m.user_id = u.id WHERE m.space_id = ? AND u.system = FALSE",
    ))
    .bind(&space.id)
    .fetch_all(&state.db)
    .await?;
    let members_json: Vec<serde_json::Value> = member_rows
        .iter()
        .map(|row| {
            use sqlx::Row;
            let id: String = row.get("id");
            let username: String = row.get("username");
            let origin: Option<String> = row.try_get("origin").ok().flatten();
            // Local members are qualified to us; already-remote members keep theirs.
            let domain = origin.as_deref().unwrap_or(our_domain);
            json!({
                "id": q(&id),
                "username": mapping::handle(&username, domain),
                "display_name": row.try_get::<Option<String>, _>("display_name").ok().flatten(),
                "avatar": row.try_get::<Option<String>, _>("avatar").ok().flatten(),
            })
        })
        .collect();

    // Recent messages per text channel, with author profiles.
    let mut messages_json = Vec::new();
    for c in &channels {
        let rows = crate::db::messages::list_messages(
            &state.db,
            &c.id,
            None,
            SNAPSHOT_MESSAGES_PER_CHANNEL,
            None,
        )
        .await
        .unwrap_or_default();
        for m in rows {
            let author = crate::db::users::get_user(&state.db, &m.author_id)
                .await
                .ok();
            let author_domain = author
                .as_ref()
                .and_then(|a| a.origin.clone())
                .unwrap_or_else(|| our_domain.to_string());
            messages_json.push(json!({
                "id": q(&m.id),
                "channel_id": q(&m.channel_id),
                "space_id": m.space_id.as_deref().map(q),
                "author": {
                    "id": q(&m.author_id),
                    "username": author.as_ref().map(|a| mapping::handle(&a.username, &author_domain)),
                    "display_name": author.as_ref().and_then(|a| a.display_name.clone()),
                    "avatar": author.as_ref().and_then(|a| a.avatar.clone()),
                },
                "content": m.content,
                "mention_everyone": m.mention_everyone,
                "mentions": serde_json::from_str::<Vec<String>>(&m.mentions).unwrap_or_default(),
                "embeds": serde_json::from_str::<serde_json::Value>(&m.embeds).unwrap_or(json!([])),
                "reply_to": m.reply_to.as_deref().map(q),
                "created_at": m.created_at,
            }));
        }
    }

    Ok(json!({
        "space": {
            "id": q(&space.id),
            "name": space.name,
            "slug": space.slug,
            "owner_id": q(&space.owner_id),
        },
        "channels": channels_json,
        "roles": roles_json,
        "members": members_json,
        "messages": messages_json,
    }))
}

// ---------------------------------------------------------------------------
// Joining side: apply a snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SnapshotSpace {
    id: String,
    name: String,
    slug: String,
    owner_id: String,
}

#[derive(Debug, Deserialize)]
struct SnapshotChannel {
    id: String,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "type")]
    channel_type: String,
    #[serde(default)]
    position: i64,
}

#[derive(Debug, Deserialize)]
struct SnapshotRole {
    id: String,
    space_id: String,
    name: String,
    #[serde(default)]
    position: i64,
    permissions: String,
}

#[derive(Debug, Deserialize)]
struct SnapshotMessageAuthor {
    id: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SnapshotMessage {
    id: String,
    channel_id: String,
    #[serde(default)]
    space_id: Option<String>,
    author: SnapshotMessageAuthor,
    content: String,
    #[serde(default)]
    mention_everyone: bool,
    #[serde(default)]
    mentions: Vec<String>,
    #[serde(default)]
    embeds: serde_json::Value,
    #[serde(default)]
    reply_to: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    space: SnapshotSpace,
    #[serde(default)]
    channels: Vec<SnapshotChannel>,
    #[serde(default)]
    roles: Vec<SnapshotRole>,
    #[serde(default)]
    members: Vec<RemoteUserRef>,
    #[serde(default)]
    messages: Vec<SnapshotMessage>,
}

/// Apply a join snapshot received from `home_domain` as a local replica, and
/// record `local_user_id` (the joining local user) as a member. Returns the
/// mirrored (qualified) space ID.
///
/// Every mirrored entity must be homed on `home_domain` (S1/S2): the snapshot
/// cannot create or overwrite local rows.
pub async fn apply_snapshot(
    state: &AppState,
    home_domain: &str,
    local_user_id: &str,
    snapshot: serde_json::Value,
) -> Result<String, AppError> {
    let snap: Snapshot = serde_json::from_value(snapshot)
        .map_err(|e| AppError::BadRequest(format!("invalid snapshot: {e}")))?;

    // S2: refuse a snapshot that tries to touch local rows.
    authority::require_homed_on(&snap.space.id, home_domain, "space")?;

    // Owner must exist before the space (FK). Upsert from members if present,
    // else a minimal placeholder.
    authority::require_homed_on(&snap.space.owner_id, home_domain, "owner")?;
    let owner = owner_ref(&snap);
    upsert_member_user(state, home_domain, &owner).await?;

    // Members. A space's membership can include users from several servers, so
    // members are not required to be homed on the home server — only the space's
    // own authoritative state (space/channels/roles/messages) is.
    for m in &snap.members {
        // Members may be homed anywhere, but must be qualified remote IDs so a
        // snapshot can never create or overwrite a local (bare-ID) user row (S2).
        authority::require_remote_target(&m.id)?;
        let domain = mapping::domain_of(&m.id).unwrap_or(home_domain);
        crate::db::users::upsert_remote_user(
            &state.db,
            &m.id,
            domain,
            &mapping::handle(&m.username, domain),
            m.display_name.as_deref(),
            m.avatar.as_deref(),
        )
        .await?;
    }

    // Space.
    crate::db::federation::upsert_remote_space(
        &state.db,
        &snap.space.id,
        home_domain,
        &snap.space.name,
        &snap.space.slug,
        &snap.space.owner_id,
    )
    .await?;

    // Channels.
    for c in &snap.channels {
        authority::require_homed_on(&c.id, home_domain, "channel")?;
        crate::db::federation::upsert_remote_channel(
            &state.db,
            &c.id,
            home_domain,
            &snap.space.id,
            c.name.as_deref().unwrap_or(""),
            &c.channel_type,
            c.position,
        )
        .await?;
        let _ = &c.space_id;
    }

    // Roles.
    for r in &snap.roles {
        authority::require_homed_on(&r.id, home_domain, "role")?;
        crate::db::federation::upsert_remote_role(
            &state.db,
            &r.id,
            home_domain,
            &snap.space.id,
            &r.name,
            r.position,
            &r.permissions,
        )
        .await?;
        let _ = &r.space_id;
    }

    // Recent messages. The message itself must be homed on the home server (its
    // authoritative space); the author may be a member from another server.
    for m in &snap.messages {
        // The message must be homed on the home server; the author may be remote
        // but must be qualified (never a bare local ID — S2).
        if authority::require_homed_on(&m.id, home_domain, "message").is_err()
            || authority::require_remote_target(&m.author.id).is_err()
        {
            continue;
        }
        let domain = mapping::domain_of(&m.author.id).unwrap_or(home_domain);
        let handle = match &m.author.username {
            Some(u) => mapping::handle(u, domain),
            None => m.author.id.clone(),
        };
        crate::db::users::upsert_remote_user(
            &state.db,
            &m.author.id,
            domain,
            &handle,
            m.author.display_name.as_deref(),
            m.author.avatar.as_deref(),
        )
        .await?;
        let mentions_json = serde_json::to_string(&m.mentions).unwrap_or_else(|_| "[]".to_string());
        let embeds_json = if m.embeds.is_null() {
            "[]".to_string()
        } else {
            m.embeds.to_string()
        };
        let insert = crate::db::messages::RemoteMessageInsert {
            id: &m.id,
            channel_id: &m.channel_id,
            space_id: m.space_id.as_deref(),
            author_id: &m.author.id,
            content: &m.content,
            created_at: &m.created_at,
            mention_everyone: m.mention_everyone,
            mentions_json: &mentions_json,
            embeds_json: &embeds_json,
            reply_to: m.reply_to.as_deref(),
            origin: home_domain,
        };
        let _ = crate::db::messages::insert_remote_message(&state.db, &insert).await?;
    }

    // The joining local user becomes a member of the mirrored space so it shows
    // up in their space list and the gateway delivers its events to them.
    crate::db::federation::add_member_with_origin(&state.db, &snap.space.id, local_user_id, None)
        .await?;

    Ok(snap.space.id)
}

fn owner_ref(snap: &Snapshot) -> RemoteUserRef {
    // Prefer full owner info from the member list; fall back to a placeholder.
    snap.members
        .iter()
        .find(|m| m.id == snap.space.owner_id)
        .cloned()
        .unwrap_or_else(|| RemoteUserRef {
            id: snap.space.owner_id.clone(),
            username: snap.space.owner_id.clone(),
            display_name: None,
            avatar: None,
        })
}

/// Upsert a single member user, deriving its origin from its qualified ID.
async fn upsert_member_user(
    state: &AppState,
    home_domain: &str,
    user: &RemoteUserRef,
) -> Result<(), AppError> {
    let domain = mapping::domain_of(&user.id).unwrap_or(home_domain);
    crate::db::users::upsert_remote_user(
        &state.db,
        &user.id,
        domain,
        &mapping::handle(&user.username, domain),
        user.display_name.as_deref(),
        user.avatar.as_deref(),
    )
    .await?;
    Ok(())
}
