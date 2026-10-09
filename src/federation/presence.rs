//! Ephemeral, signed presence exchange between trusted user-home servers.
//! Updates never enter the durable content outbox. Shared-space membership
//! bounds visibility; periodic exchange supplies initial state and renews TTLs.
use super::{mapping, sender};
use crate::{error::AppError, models::presence::Presence, state::AppState};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use tokio::sync::{Mutex, Notify};

pub const PATH: &str = "/federation/v1/presence";
pub const TTL_MS: i64 = 90_000;
const PAGE_SIZE: usize = 500;

pub struct RemotePresence {
    pub presence: Presence,
    pub observed_at: i64,
    pub expires_at: i64,
}

#[derive(Default)]
pub struct PresenceState {
    pub remote: DashMap<String, RemotePresence>,
    pub changed: Notify,
    sync: Mutex<Option<tokio::time::Instant>>,
}

#[derive(Serialize, Deserialize)]
pub struct Exchange {
    pub observed_at: i64,
    #[serde(default)]
    pub presences: Vec<Presence>,
    #[serde(default)]
    pub after: Option<String>,
}

/// Local accounts visible to this peer, including aliases of our own users
/// that were mirrored back into a remote-homed space.
async fn shared_local_users(state: &AppState, peer: &str) -> Result<Vec<String>, AppError> {
    let own = state
        .federation
        .as_ref()
        .map(|f| f.domain.as_str())
        .unwrap_or("");
    let ids: Vec<String> = sqlx::query_scalar(&crate::db::q(
        "SELECT DISTINCT m.user_id FROM members m JOIN users u ON u.id = m.user_id \
         JOIN spaces s ON s.id = m.space_id \
         WHERE (u.origin IS NULL OR u.origin = ?) AND \
         (s.origin = ? OR EXISTS (SELECT 1 FROM members pm JOIN users pu ON pu.id = pm.user_id \
          WHERE pm.space_id = m.space_id AND COALESCE(pm.origin, pu.origin) = ?))",
    ))
    .bind(own)
    .bind(peer)
    .bind(peer)
    .fetch_all(&state.db)
    .await?;
    let mut ids: Vec<String> = ids
        .into_iter()
        .map(|id| mapping::local_part(&id).to_string())
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn local_snapshot(state: &AppState, peer: &str) -> Result<Vec<Presence>, AppError> {
    let Some(fed) = state.federation.as_ref() else {
        return Ok(vec![]);
    };
    let mut result = vec![];
    let ids = shared_local_users(state, peer).await?;
    let mut authoritative = HashSet::new();
    for batch in ids.chunks(PAGE_SIZE) {
        for user in crate::db::users::get_users_by_ids(&state.db, batch).await? {
            if user.origin.is_none() {
                authoritative.insert(user.id);
            }
        }
    }
    for id in ids {
        if !authoritative.contains(&id) {
            continue;
        }
        let mut p = crate::presence::visible_presence(
            crate::presence::current_presence(state, &id)
                .unwrap_or_else(|| crate::presence::offline_presence(&id)),
            None,
        );
        p.user_id = mapping::qualify(&id, &fed.domain);
        result.push(p);
    }
    result.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    Ok(result)
}

/// Verify the entire batch before mutating any state. A peer may only publish
/// users it homes and which already have membership on this server.
pub async fn apply(state: &AppState, peer: &str, update: &Exchange) -> Result<(), AppError> {
    if update.presences.len() > PAGE_SIZE {
        return Err(AppError::BadRequest("presence batch too large".into()));
    }
    let now = chrono::Utc::now().timestamp_millis();
    if update.observed_at > now + 5_000 {
        return Err(AppError::BadRequest(
            "presence timestamp is in the future".into(),
        ));
    }
    let own = state
        .federation
        .as_ref()
        .map(|f| f.domain.as_str())
        .unwrap_or("");
    if peer.eq_ignore_ascii_case(own) {
        return Err(AppError::Forbidden("cannot publish local presence".into()));
    }
    let ids: Vec<String> = update.presences.iter().map(|p| p.user_id.clone()).collect();
    let users: HashMap<_, _> = crate::db::users::get_users_by_ids(&state.db, &ids)
        .await?
        .into_iter()
        .map(|u| (u.id.clone(), u))
        .collect();
    let mut memberships: HashMap<String, Vec<String>> = HashMap::new();
    if !ids.is_empty() {
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = crate::db::q(&format!(
            "SELECT user_id, space_id FROM members WHERE user_id IN ({placeholders})"
        ));
        let mut query = sqlx::query(&sql);
        for id in &ids {
            query = query.bind(id);
        }
        for row in query.fetch_all(&state.db).await? {
            memberships
                .entry(row.get("user_id"))
                .or_default()
                .push(row.get("space_id"));
        }
    }
    let mut validated = vec![];
    let mut seen = HashSet::new();
    for p in &update.presences {
        super::authority::require_homed_on(&p.user_id, peer, "presence user")?;
        if !seen.insert(&p.user_id) {
            return Err(AppError::BadRequest("duplicate presence user".into()));
        }
        if !matches!(
            p.status.as_str(),
            "online" | "idle" | "dnd" | "offline" | "invisible"
        ) {
            return Err(AppError::BadRequest("invalid presence status".into()));
        }
        let user = users
            .get(&p.user_id)
            .ok_or_else(|| AppError::Forbidden("unknown presence user".into()))?;
        if !user
            .origin
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case(peer))
        {
            return Err(AppError::Forbidden("presence user origin mismatch".into()));
        }
        let spaces = memberships
            .get(&p.user_id)
            .ok_or_else(|| AppError::Forbidden("presence user is not a member".into()))?;
        validated.push((
            crate::presence::visible_presence(p.clone(), None),
            spaces.clone(),
        ));
    }
    // Expired deliveries cannot extend a lease or replay an old online status.
    if update.observed_at < now - TTL_MS {
        return Ok(());
    }
    for (p, spaces) in validated {
        let changed;
        {
            let mut entry = state
                .federated_presence
                .remote
                .entry(p.user_id.clone())
                .or_insert_with(|| RemotePresence {
                    presence: crate::presence::offline_presence(&p.user_id),
                    observed_at: -1,
                    expires_at: 0,
                });
            if update.observed_at <= entry.observed_at {
                continue;
            }
            changed = (entry.expires_at <= now && p.status != "offline")
                || serde_json::to_value(&entry.presence).ok() != serde_json::to_value(&p).ok();
            *entry = RemotePresence {
                presence: p.clone(),
                observed_at: update.observed_at,
                expires_at: update.observed_at + TTL_MS,
            };
        }
        if changed {
            for space in spaces {
                super::broadcast_space(
                    state,
                    Some(space),
                    "presence.update",
                    serde_json::to_value(&p).unwrap(),
                    "presences",
                )
                .await;
            }
        }
    }
    Ok(())
}

pub async fn handle_exchange(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (_, peer, req): (_, _, Exchange) =
        match super::verify::prepare(&state, &headers, PATH, &body).await {
            Ok(v) => v,
            Err(e) => return *e,
        };
    if let Err(e) = apply(&state, &peer.domain, &req).await {
        return e.into_response();
    }
    let mut snapshot = match local_snapshot(&state, &peer.domain).await {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    snapshot.retain(|p| req.after.as_ref().is_none_or(|id| &p.user_id > id));
    let more = snapshot.len() > PAGE_SIZE;
    snapshot.truncate(PAGE_SIZE);
    let after = if more {
        snapshot.last().map(|p| p.user_id.clone())
    } else {
        None
    };
    (
        StatusCode::OK,
        Json(Exchange {
            observed_at: chrono::Utc::now().timestamp_millis(),
            presences: snapshot,
            after,
        }),
    )
        .into_response()
}

async fn exchange_peer(state: &AppState, peer: &str) -> Result<(), AppError> {
    let local = local_snapshot(state, peer).await?;
    let observed_at = chrono::Utc::now().timestamp_millis();
    let mut offset = 0;
    let mut after = None;
    let mut cursors = HashSet::new();
    loop {
        let end = (offset + PAGE_SIZE).min(local.len());
        let request = Exchange {
            observed_at,
            presences: local[offset..end].to_vec(),
            after: after.clone(),
        };
        let body = serde_json::to_vec(&request).map_err(|e| AppError::Internal(e.to_string()))?;
        let (status, body) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            sender::request_signed(state, peer, PATH, &body),
        )
        .await
        .map_err(|_| AppError::Internal("presence exchange timed out".into()))??;
        if !status.is_success() {
            return Err(AppError::Internal(format!(
                "presence peer returned {status}"
            )));
        }
        let response: Exchange =
            serde_json::from_slice(&body).map_err(|e| AppError::Internal(e.to_string()))?;
        apply(state, peer, &response).await?;
        offset = end;
        after = response.after;
        if let Some(cursor) = &after {
            if !cursors.insert(cursor.clone()) {
                return Err(AppError::Internal("presence cursor did not advance".into()));
            }
        }
        if offset == local.len() && after.is_none() {
            break;
        }
    }
    Ok(())
}

async fn sync(state: &AppState, force: bool) {
    if state.federation.is_none() {
        return;
    }
    let mut guard = if force {
        state.federated_presence.sync.lock().await
    } else {
        match state.federated_presence.sync.try_lock() {
            Ok(g) => g,
            Err(_) => return,
        }
    };
    if !force && guard.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
        return;
    }
    let peers = match crate::db::federation::list_peers(&state.db).await {
        Ok(p) => p,
        Err(_) => return,
    };
    stream::iter(peers.into_iter().filter(|p| p.is_trusted()))
        .for_each_concurrent(4, |peer| async move {
            if let Err(e) = exchange_peer(state, &peer.domain).await {
                tracing::debug!("presence exchange with {}: {e}", peer.domain);
            }
        })
        .await;
    *guard = Some(tokio::time::Instant::now());
}

/// Refresh initial state without multiplying requests from concurrent rosters.
pub async fn sync_once(state: &AppState) {
    sync(state, false).await;
}

pub async fn expire(state: &AppState) {
    let now = chrono::Utc::now().timestamp_millis();
    let ids: Vec<String> = state
        .federated_presence
        .remote
        .iter()
        .filter(|p| p.expires_at <= now && p.presence.status != "offline")
        .map(|p| p.key().clone())
        .collect();
    for id in ids {
        let p = {
            let Some(mut entry) = state.federated_presence.remote.get_mut(&id) else {
                continue;
            };
            if entry.expires_at > now || entry.presence.status == "offline" {
                continue;
            }
            entry.presence = crate::presence::offline_presence(&id);
            entry.presence.clone()
        };
        if let Ok(spaces) = crate::db::spaces::list_space_ids_for_user(&state.db, &id).await {
            for space in spaces {
                super::broadcast_space(
                    state,
                    Some(space),
                    "presence.update",
                    serde_json::to_value(&p).unwrap(),
                    "presences",
                )
                .await;
            }
        }
    }
    state
        .federated_presence
        .remote
        .retain(|_, p| p.expires_at > now - 300_000);
}

pub async fn run(state: AppState) {
    let exchange = async {
        let mut refresh = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = refresh.tick() => sync(&state, true).await,
                _ = state.federated_presence.changed.notified() => {
                    // Coalesce bursts without exceeding the peer request budget.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    sync(&state, true).await;
                }
            }
        }
    };
    let expiry = async {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            expire(&state).await;
        }
    };
    // A slow peer must not block lease expiry and local offline broadcasts.
    tokio::join!(exchange, expiry);
}
