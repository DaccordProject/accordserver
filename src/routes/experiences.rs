use crate::{
    db::{
        self,
        experiences::{Participant, Session},
    },
    error::AppError,
    gateway::events::GatewayBroadcast,
    middleware::{
        auth::AuthUser,
        permissions::{require_channel_permission, require_membership, require_permission},
    },
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
use experience_contract::{verify_release, Release};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::BTreeMap;

fn keys() -> Result<BTreeMap<String, String>, AppError> {
    serde_json::from_str(&std::env::var("EXPERIENCE_TRUSTED_KEYS").unwrap_or_else(|_| "{}".into()))
        .map_err(|_| AppError::Internal("Invalid experience trust configuration".into()))
}

fn identifier(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
    {
        return Err(AppError::BadRequest(
            "Invalid game/version identifier".into(),
        ));
    }
    Ok(())
}

async fn fetch(path: &str) -> Result<Value, AppError> {
    if std::env::var("EXPERIENCES_ENABLED").as_deref() != Ok("true") {
        return Err(AppError::Forbidden(
            "Experiences are disabled by this server's operator".into(),
        ));
    }
    let base = std::env::var("EXPERIENCE_DIRECTORY_URL")
        .unwrap_or_else(|_| "https://master.daccord.gg".into());
    let url = reqwest::Url::parse(&base)
        .map_err(|_| AppError::Internal("Invalid directory URL".into()))?;
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && [Some("localhost"), Some("127.0.0.1"), Some("[::1]")].contains(&url.host_str()))
    {
        return Err(AppError::Internal("Directory requires HTTPS".into()));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| AppError::Internal("Directory client unavailable".into()))?;
    let mut response = client
        .get(format!(
            "{}/api/v1/experiences{path}",
            base.trim_end_matches('/')
        ))
        .send()
        .await
        .map_err(|_| {
            AppError::Forbidden("Directory unavailable; fresh approval required".into())
        })?;
    if !response.status().is_success() {
        return Err(AppError::Forbidden(
            "Release unavailable from directory".into(),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::Forbidden("Directory response failed".into()))?
    {
        if bytes.len() + chunk.len() > 3_000_000 {
            return Err(AppError::PayloadTooLarge(
                "Directory response too large".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::Forbidden("Invalid directory response".into()))
}

async fn fresh(id: &str, version: &str) -> Result<Release, AppError> {
    identifier(id)?;
    identifier(version)?;
    let data = fetch(&format!("/{id}/{version}")).await?;
    let release: Release = serde_json::from_value(data["data"].clone())
        .map_err(|_| AppError::Forbidden("Invalid release metadata".into()))?;
    verify_release(&release, &keys()?).map_err(AppError::Forbidden)?;
    if release.manifest.id != id || release.manifest.version != version {
        return Err(AppError::Forbidden("Directory identity mismatch".into()));
    }
    Ok(release)
}

fn member(auth: &AuthUser) -> Result<(), AppError> {
    if auth.is_guest || auth.is_bot {
        return Err(AppError::Forbidden("A member account is required".into()));
    }
    Ok(())
}

async fn require_arcade_access(
    state: &AppState,
    space: &str,
    auth: &AuthUser,
) -> Result<(), AppError> {
    require_membership(&state.db, space, &auth.user_id).await?;
    let channel: Option<String> = sqlx::query_scalar(&db::q(
        "SELECT id FROM channels WHERE space_id = ? AND type = 'arcade'",
    ))
    .bind(space)
    .fetch_optional(&state.db)
    .await?;
    if let Some(channel) = channel {
        require_channel_permission(&state.db, &channel, auth, "view_channel").await?;
    }
    Ok(())
}

async fn installed_with_disabled(
    state: &AppState,
    space: &str,
    game: &str,
    allow_disabled: bool,
) -> Result<(Release, Value, i64), AppError> {
    let row = sqlx::query(&db::q("SELECT release_json, config_json, enabled, generation FROM space_experiences WHERE space_id = ? AND game_id = ?")).bind(space).bind(game).fetch_one(&state.db).await?;
    if row.get::<i64, _>("enabled") == 0 && !allow_disabled {
        return Err(AppError::Forbidden("Experience disabled".into()));
    }
    let pinned: Release = serde_json::from_str(row.get("release_json"))
        .map_err(|_| AppError::Internal("Invalid installed release".into()))?;
    let release = match fresh(game, &pinned.manifest.version).await {
        Ok(r) => r,
        Err(e) => {
            end_sessions(state, space, Some(game), "approval_unavailable").await?;
            return Err(e);
        }
    };
    if release.digest != pinned.digest {
        return Err(AppError::Forbidden("Pinned release digest changed".into()));
    }
    let config = serde_json::from_str(row.get("config_json"))
        .map_err(|_| AppError::Internal("Invalid configuration".into()))?;
    Ok((release, config, row.get("generation")))
}

async fn installed(
    state: &AppState,
    space: &str,
    game: &str,
) -> Result<(Release, Value, i64), AppError> {
    installed_with_disabled(state, space, game, false).await
}

async fn arcade_enabled(state: &AppState, space: &str) -> Result<bool, AppError> {
    let row = sqlx::query(&db::q(
        "SELECT enabled FROM space_arcades WHERE space_id = ?",
    ))
    .bind(space)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| r.get::<i64, _>("enabled") != 0).unwrap_or(true))
}

pub async fn broadcast(state: &AppState, session: &Session) {
    if let Some(tx) = state.gateway_tx.read().await.as_ref() {
        let _ = tx.send(GatewayBroadcast {
            space_id: Some(session.space_id.clone()),
            target_user_ids: Some(
                session
                    .participants
                    .iter()
                    .map(|p| p.user_id.clone())
                    .collect(),
            ),
            event: json!({"op":0,"type":"experience.session","data":session}),
            intent: "experiences".into(),
            required_permission: None,
        });
    }
}

async fn end_sessions(
    state: &AppState,
    space: &str,
    game: Option<&str>,
    reason: &str,
) -> Result<(), AppError> {
    for listed in db::experiences::list(&state.db, space).await? {
        if game.is_some_and(|g| g != listed.game_id) {
            continue;
        }
        // A policy mutation is already committed. No active save can pass the
        // installation-generation guard; retry a final in-flight revision race.
        for _ in 0..3 {
            let mut session = db::experiences::load(&state.db, space, &listed.id).await?;
            if session.state == "ended" {
                break;
            }
            session.end(reason, None);
            if db::experiences::save(&state.db, &mut session).await.is_ok() {
                broadcast(state, &session).await;
                break;
            }
        }
    }
    Ok(())
}

async fn ensure_arcade_channel(state: &AppState, space: &str) -> Result<(), AppError> {
    if let Some(channel) = db::channels::ensure_arcade_channel(&state.db, space).await? {
        let json = super::spaces::channel_row_to_json_pub(&state.db, &channel).await;
        if let Some(tx) = state.gateway_tx.read().await.as_ref() {
            let _ = tx.send(GatewayBroadcast {
                space_id: Some(space.into()),
                target_user_ids: None,
                event: json!({"op":0,"type":"channel.create","data":json}),
                intent: "channels".into(),
                required_permission: None,
            });
        }
    }
    Ok(())
}

pub(super) async fn disable_arcade(state: &AppState, space: &str) -> Result<(), AppError> {
    sqlx::query(&db::q("INSERT INTO space_arcades (space_id, enabled) VALUES (?, 0) ON CONFLICT(space_id) DO UPDATE SET enabled = 0"))
        .bind(space).execute(&state.db).await?;
    end_sessions(state, space, None, "arcade_disabled").await
}

/// End and remove an expired game only if no player action has changed the
/// snapshot. Broadcast the final result before deleting so open clients stop.
async fn remove_idle_session(state: &AppState, session: &mut Session) -> Result<(), AppError> {
    session.end("idle_timeout", None);
    db::experiences::save(&state.db, session).await?;
    broadcast(state, session).await;
    sqlx::query(&db::q("DELETE FROM experience_sessions WHERE id = ? AND space_id = ? AND revision = ? AND state = 'ended'"))
        .bind(&session.id).bind(&session.space_id).bind(session.revision).execute(&state.db).await?;
    Ok(())
}

pub async fn directory(
    State(state): State<AppState>,
    Path(space): Path<String>,
    auth: AuthUser,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_permission(&state.db, &space, &auth, "manage_space").await?;
    let data = fetch("").await?;
    let releases: Vec<Release> = serde_json::from_value(data["data"].clone())
        .map_err(|_| AppError::Forbidden("Invalid catalogue".into()))?;
    let trust = keys()?;
    let releases: Vec<_> = releases
        .into_iter()
        .filter(|r| verify_release(r, &trust).is_ok())
        .map(|r| r.manifest)
        .collect();
    Ok(Json(json!({"data":releases})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enable {
    pub version: String,
}

pub async fn enable(
    State(state): State<AppState>,
    Path((space, game)): Path<(String, String)>,
    auth: AuthUser,
    Json(request): Json<Enable>,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_permission(&state.db, &space, &auth, "manage_space").await?;
    let release = fresh(&game, &request.version).await?;
    // Enabling and every update require an explicit owner operation. v1 has a
    // fixed capability set, so no update can silently gain new permissions.
    sqlx::query(&db::q("INSERT INTO space_experiences (space_id, game_id, release_json) VALUES (?, ?, ?) ON CONFLICT(space_id, game_id) DO UPDATE SET release_json = excluded.release_json, enabled = 1, generation = space_experiences.generation + 1"))
        .bind(&space).bind(&game).bind(serde_json::to_string(&release).unwrap()).execute(&state.db).await?;
    end_sessions(&state, &space, Some(&game), "release_updated").await?;
    ensure_arcade_channel(&state, &space).await?;
    Ok(Json(json!({"data":release.manifest})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    pub enabled: bool,
    #[serde(default)]
    pub turn_timeout_seconds: i64,
}

pub async fn configure(
    State(state): State<AppState>,
    Path((space, game)): Path<(String, String)>,
    auth: AuthUser,
    Json(request): Json<Configure>,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_permission(&state.db, &space, &auth, "manage_space").await?;
    if request.turn_timeout_seconds != 0 && !(60..=604800).contains(&request.turn_timeout_seconds) {
        return Err(AppError::BadRequest(
            "Turn timeout must be zero or 60–604800 seconds".into(),
        ));
    }
    if request.enabled {
        installed_with_disabled(&state, &space, &game, true).await?;
    }
    let result = sqlx::query(&db::q("UPDATE space_experiences SET enabled = ?, config_json = ? WHERE space_id = ? AND game_id = ?"))
        .bind(if request.enabled {1i64} else {0}).bind(json!({"turn_timeout_seconds":request.turn_timeout_seconds}).to_string()).bind(&space).bind(&game).execute(&state.db).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Experience not installed".into()));
    }
    if !request.enabled {
        end_sessions(&state, &space, Some(&game), "disabled").await?;
    }
    Ok(Json(json!({"data":null})))
}

pub async fn remove(
    State(state): State<AppState>,
    Path((space, game)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_permission(&state.db, &space, &auth, "manage_space").await?;
    sqlx::query(&db::q(
        "DELETE FROM space_experiences WHERE space_id = ? AND game_id = ?",
    ))
    .bind(&space)
    .bind(&game)
    .execute(&state.db)
    .await?;
    end_sessions(&state, &space, Some(&game), "removed").await?;
    Ok(Json(json!({"data":null})))
}

pub async fn configure_arcade(
    State(state): State<AppState>,
    Path(space): Path<String>,
    auth: AuthUser,
    Json(request): Json<Configure>,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_permission(&state.db, &space, &auth, "manage_space").await?;
    sqlx::query(&db::q("INSERT INTO space_arcades (space_id, enabled) VALUES (?, ?) ON CONFLICT(space_id) DO UPDATE SET enabled = excluded.enabled")).bind(&space).bind(if request.enabled {1i64} else {0}).execute(&state.db).await?;
    if !request.enabled {
        end_sessions(&state, &space, None, "arcade_disabled").await?;
    } else {
        ensure_arcade_channel(&state, &space).await?;
    }
    Ok(Json(json!({"data":{"enabled":request.enabled}})))
}

pub async fn arcade(
    State(state): State<AppState>,
    Path(space): Path<String>,
    auth: AuthUser,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_arcade_access(&state, &space, &auth).await?;
    let enabled = arcade_enabled(&state, &space).await?;
    let rows = sqlx::query(&db::q("SELECT release_json, enabled, config_json FROM space_experiences WHERE space_id = ? ORDER BY game_id")).bind(&space).fetch_all(&state.db).await?;
    let mut games = Vec::new();
    for row in rows {
        let release: Release = serde_json::from_str(row.get("release_json"))
            .map_err(|_| AppError::Internal("Invalid installed release".into()))?;
        games.push(json!({"manifest":release.manifest,"enabled":row.get::<i64,_>("enabled") != 0,"config":serde_json::from_str::<Value>(row.get("config_json")).unwrap_or(json!({}))}));
    }
    let visible = enabled && games.iter().any(|g| g["enabled"] == true);
    let active_sessions = if visible {
        let now = chrono::Utc::now().timestamp();
        db::experiences::list(&state.db, &space)
            .await?
            .iter()
            .filter(|s| s.state != "ended" && !s.idle_expired(now) && s.can_observe(&auth.user_id))
            .count()
    } else {
        0
    };
    Ok(Json(
        json!({"data":{"enabled":enabled,"visible":visible,"experiences":games,"active_sessions":active_sessions}}),
    ))
}

pub async fn package(
    State(state): State<AppState>,
    Path((space, game)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_arcade_access(&state, &space, &auth).await?;
    if !arcade_enabled(&state, &space).await? {
        return Err(AppError::Forbidden("Arcade disabled".into()));
    }
    let (release, _, _) = installed(&state, &space, &game).await?;
    Ok(Json(
        json!({"data":{ "manifest":release.manifest,"payload":release.payload,"digest":release.digest,"key_id":release.key_id,"signature":release.signature,"status":release.status,"reviewed_at":release.reviewed_at,"verification_key":keys()?.get(&release.key_id) }}),
    ))
}

pub(super) async fn checked_session(
    state: &AppState,
    space: &str,
    id: &str,
    auth: &AuthUser,
) -> Result<Session, AppError> {
    Ok(checked_session_release(state, space, id, auth).await?.0)
}

async fn checked_session_release(
    state: &AppState,
    space: &str,
    id: &str,
    auth: &AuthUser,
) -> Result<(Session, Option<Release>), AppError> {
    member(auth)?;
    require_arcade_access(state, space, auth).await?;
    let mut session = db::experiences::load(&state.db, space, id).await?;
    if !session.can_observe(&auth.user_id) {
        return Err(AppError::Forbidden("Invite-only session".into()));
    }
    let mut approved = None;
    if session.idle_expired(chrono::Utc::now().timestamp()) {
        remove_idle_session(state, &mut session).await?;
        return Ok((session, None));
    }
    if session.state != "ended" {
        if !arcade_enabled(state, space).await? {
            return Err(AppError::Forbidden("Arcade disabled".into()));
        }
        let (release, _, generation) = installed(state, space, &session.game_id).await?;
        if release.digest != session.digest || generation != session.installation_generation {
            return Err(AppError::Forbidden(
                "Session release no longer enabled".into(),
            ));
        }
        approved = Some(release);
        // Approval checks can involve a remote directory. Live ticks must not
        // make the returned snapshot obsolete while those checks are in flight.
        if session.mode == "real_time" {
            session = db::experiences::load(&state.db, space, id).await?;
        }
        let now = chrono::Utc::now().timestamp();
        if session.deadline.is_some_and(|deadline| now >= deadline) {
            let loser = session.turn_user_id.as_deref();
            let winner = session
                .participants
                .iter()
                .find(|p| p.role == "player" && Some(p.user_id.as_str()) != loser)
                .map(|p| p.user_id.clone());
            session.end("turn_timeout", winner);
            db::experiences::save(&state.db, &mut session).await?;
            broadcast(state, &session).await;
        }
    }
    Ok((session, approved))
}

pub async fn sessions(
    State(state): State<AppState>,
    Path(space): Path<String>,
    auth: AuthUser,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_arcade_access(&state, &space, &auth).await?;
    let mut result = Vec::new();
    for session in db::experiences::list(&state.db, &space).await? {
        if session.can_observe(&auth.user_id) {
            match checked_session(&state, &space, &session.id, &auth).await {
                Ok(session)
                    if session.result.as_ref().and_then(|r| r["reason"].as_str())
                        != Some("idle_timeout") =>
                {
                    result.push(session)
                }
                Ok(_) | Err(AppError::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(Json(json!({"data":result})))
}

pub async fn session(
    State(state): State<AppState>,
    Path((space, id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<Value>, AppError> {
    Ok(Json(
        json!({"data":checked_session(&state, &space, &id, &auth).await?}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub game_id: String,
    #[serde(default)]
    pub invite_only: bool,
    #[serde(default)]
    pub invited: Vec<String>,
}

pub async fn create_session(
    State(state): State<AppState>,
    Path(space): Path<String>,
    auth: AuthUser,
    Json(request): Json<Create>,
) -> Result<Json<Value>, AppError> {
    member(&auth)?;
    require_arcade_access(&state, &space, &auth).await?;
    if !arcade_enabled(&state, &space).await? {
        return Err(AppError::Forbidden("Arcade disabled".into()));
    }
    if request.invited.len() > 18 {
        return Err(AppError::BadRequest("Too many invitations".into()));
    }
    for user in &request.invited {
        require_membership(&state.db, &space, user).await?;
    }
    if db::experiences::list(&state.db, &space)
        .await?
        .iter()
        .filter(|s| s.state != "ended")
        .count()
        >= 32
    {
        return Err(AppError::Conflict("Space session limit reached".into()));
    }
    let (release, config, generation) = installed(&state, &space, &request.game_id).await?;
    let now = chrono::Utc::now().timestamp();
    let session = Session {
        id: crate::snowflake::generate().to_string(),
        space_id: space,
        game_id: request.game_id,
        version: release.manifest.version,
        digest: release.digest,
        installation_generation: generation,
        mode: release.manifest.session_mode,
        state: "lobby".into(),
        host_user_id: auth.user_id.clone(),
        invite_only: request.invite_only,
        invited: {
            let mut invited = request.invited;
            if !invited.contains(&auth.user_id) {
                invited.push(auth.user_id.clone());
            }
            invited
        },
        participants: vec![Participant {
            user_id: auth.user_id,
            role: "player".into(),
            slot: Some(0),
            ready: false,
            last_seen: now,
        }],
        revision: 0,
        game: json!({}),
        result: None,
        turn_user_id: None,
        turn_timeout_seconds: config["turn_timeout_seconds"].as_i64().unwrap_or(0),
        deadline: None,
        created_at: now,
        updated_at: now,
        last_activity_at: now,
        idle_expires_at: Some(now + db::experiences::IDLE_TIMEOUT_SECONDS),
    };
    db::experiences::insert(&state.db, &session).await?;
    broadcast(&state, &session).await;
    Ok(Json(json!({"data":session})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub operation: String,
    pub revision: i64,
    #[serde(default)]
    pub spectator: bool,
    #[serde(default)]
    pub ready: bool,
}

pub async fn membership(
    State(state): State<AppState>,
    Path((space, id)): Path<(String, String)>,
    auth: AuthUser,
    Json(request): Json<Membership>,
) -> Result<Json<Value>, AppError> {
    for attempt in 0..3 {
        let result = membership_once(&state, &space, &id, &auth, &request).await;
        if attempt < 2
            && matches!(result, Err(AppError::SessionChanged))
            && matches!(request.operation.as_str(), "join" | "leave")
        {
            continue;
        }
        return result;
    }
    unreachable!()
}

async fn membership_once(
    state: &AppState,
    space: &str,
    id: &str,
    auth: &AuthUser,
    request: &Membership,
) -> Result<Json<Value>, AppError> {
    let (mut session, approved) = checked_session_release(state, space, id, auth).await?;
    let live_membership = session.mode == "real_time"
        && session.state == "running"
        && matches!(request.operation.as_str(), "join" | "leave");
    let mut locked = if live_membership {
        Some(db::experiences::lock_live(&state.db, space, id).await?)
    } else {
        None
    };
    if let Some(tx) = &mut locked {
        session = db::experiences::load(&mut **tx, space, id).await?;
    }
    if request.revision < 0
        || request.revision > session.revision
        || (!live_membership && request.revision != session.revision)
    {
        return Err(AppError::Conflict("Stale session revision".into()));
    }
    if session.state == "ended" {
        return Err(AppError::Conflict("Session ended".into()));
    }
    let now = chrono::Utc::now().timestamp();
    let position = session
        .participants
        .iter()
        .position(|p| p.user_id == auth.user_id);
    let player_activity = match request.operation.as_str() {
        "ready" | "start" => true,
        "join" => position.is_none() && !request.spectator,
        "leave" => position.is_some_and(|i| session.participants[i].role == "player"),
        _ => false,
    };
    match request.operation.as_str() {
        "join" => {
            if let Some(index) = position {
                session.participants[index].last_seen = now;
            } else {
                if session.state != "lobby" && !request.spectator {
                    return Err(AppError::Conflict(
                        "Only spectators can join a started session".into(),
                    ));
                }
                let release = approved
                    .as_ref()
                    .ok_or_else(|| AppError::Conflict("Session ended".into()))?;
                let slot = if request.spectator {
                    if session
                        .participants
                        .iter()
                        .filter(|p| p.role == "spectator")
                        .count()
                        >= release.manifest.max_spectators as usize
                    {
                        return Err(AppError::Conflict("Spectator slots full".into()));
                    }
                    None
                } else {
                    Some(
                        (0..release.manifest.max_players as usize)
                            .find(|slot| {
                                !session.participants.iter().any(|p| p.slot == Some(*slot))
                            })
                            .ok_or_else(|| AppError::Conflict("Player slots full".into()))?,
                    )
                };
                session.participants.push(Participant {
                    user_id: auth.user_id.clone(),
                    role: if request.spectator {
                        "spectator"
                    } else {
                        "player"
                    }
                    .into(),
                    slot,
                    ready: false,
                    last_seen: now,
                });
            }
        }
        "ready" => {
            let player = position
                .and_then(|i| session.participants.get_mut(i))
                .filter(|p| p.role == "player")
                .ok_or_else(|| AppError::Forbidden("Player membership required".into()))?;
            if session.state != "lobby" {
                return Err(AppError::Conflict("Session already started".into()));
            }
            player.ready = request.ready;
        }
        "start" => {
            if session.host_user_id != auth.user_id {
                return Err(AppError::Forbidden("Only the lobby host may start".into()));
            }
            let players: Vec<_> = session
                .participants
                .iter()
                .filter(|p| p.role == "player")
                .collect();
            if session.state != "lobby" || players.len() != 2 || players.iter().any(|p| !p.ready) {
                return Err(AppError::Conflict("Two ready players required".into()));
            }
            for player in &players {
                require_membership(&state.db, space, &player.user_id).await?;
            }
            session.state = "running".into();
            for p in &mut session.participants {
                p.last_seen = now;
            }
            session.game = if session.mode == "turn_based" {
                super::experiences_rules::initial_chess()
            } else {
                super::experiences_rules::initial_pong()
            };
            if session.mode == "turn_based" {
                session.turn_user_id = session
                    .participants
                    .iter()
                    .find(|p| p.slot == Some(0))
                    .map(|p| p.user_id.clone());
                if session.turn_timeout_seconds > 0 {
                    session.deadline = Some(now + session.turn_timeout_seconds);
                }
            }
        }
        "leave" => {
            let index = position
                .ok_or_else(|| AppError::Forbidden("Session membership required".into()))?;
            let leaving = session.participants.remove(index);
            if session.state == "running" && leaving.role == "player" {
                let winner = session
                    .participants
                    .iter()
                    .find(|p| p.role == "player")
                    .map(|p| p.user_id.clone());
                session.end("player_left", winner);
            }
            if session.host_user_id == auth.user_id {
                if let Some(next) = session
                    .participants
                    .iter()
                    .find(|p| p.role == "player")
                    .or(session.participants.first())
                {
                    session.host_user_id = next.user_id.clone();
                } else {
                    session.end("empty", None);
                }
            }
        }
        _ => return Err(AppError::BadRequest("Unknown membership operation".into())),
    }
    if player_activity {
        session.record_player_activity();
    }
    if let Some(mut tx) = locked {
        db::experiences::save(&mut *tx, &mut session).await?;
        tx.commit().await?;
    } else {
        db::experiences::save(&state.db, &mut session).await?;
    }
    broadcast(state, &session).await;
    Ok(Json(json!({"data":session})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub revision: i64,
    pub kind: String,
    #[serde(default)]
    pub a: i32,
    #[serde(default)]
    pub b: i32,
    #[serde(default)]
    pub promotion: Option<String>,
}

pub async fn action(
    State(state): State<AppState>,
    Path((space, id)): Path<(String, String)>,
    auth: AuthUser,
    Json(request): Json<Action>,
) -> Result<Json<Value>, AppError> {
    for attempt in 0..3 {
        let result = action_once(&state, &space, &id, &auth, &request).await;
        if attempt < 2
            && request.kind == "resign"
            && matches!(result, Err(AppError::SessionChanged))
        {
            continue;
        }
        return result;
    }
    unreachable!()
}

async fn action_once(
    state: &AppState,
    space: &str,
    id: &str,
    auth: &AuthUser,
    request: &Action,
) -> Result<Json<Value>, AppError> {
    let mut session = checked_session(state, space, id, auth).await?;
    let live_resignation = session.mode == "real_time" && request.kind == "resign";
    let mut locked = if live_resignation && session.state == "running" {
        Some(db::experiences::lock_live(&state.db, space, id).await?)
    } else {
        None
    };
    if let Some(tx) = &mut locked {
        session = db::experiences::load(&mut **tx, space, id).await?;
    }
    if request.revision < 0
        || request.revision > session.revision
        || (!live_resignation && request.revision != session.revision)
        || session.state != "running"
    {
        return Err(AppError::Conflict("Stale or inactive session".into()));
    }
    let player = session
        .participants
        .iter()
        .find(|p| p.user_id == auth.user_id && p.role == "player")
        .ok_or_else(|| AppError::Forbidden("Only players may submit actions".into()))?;
    if request.kind == "resign" {
        let winner = session
            .participants
            .iter()
            .find(|p| p.role == "player" && p.user_id != auth.user_id)
            .map(|p| p.user_id.clone());
        session.end("resigned", winner);
    } else if session.mode == "turn_based" {
        if session.turn_user_id.as_deref() != Some(&auth.user_id) {
            return Err(AppError::Forbidden("It is not your turn".into()));
        }
        super::experiences_rules::chess_move(&mut session, request, &auth.user_id)?;
    } else {
        if request.kind != "input" || request.b != 0 || request.promotion.is_some() {
            return Err(AppError::BadRequest("Invalid live input action".into()));
        }
        let slot = player.slot.unwrap();
        super::experiences_rules::pong_input(&mut session, slot, request.a)?;
    }
    session.record_player_activity();
    if let Some(mut tx) = locked {
        db::experiences::save(&mut *tx, &mut session).await?;
        tx.commit().await?;
    } else {
        db::experiences::save(&state.db, &mut session).await?;
    }
    broadcast(state, &session).await;
    Ok(Json(json!({"data":session})))
}

/// Bounded maintenance runs independently of connected clients. Timeout results
/// and abandonment therefore do not depend on somebody opening the Arcade.
pub async fn maintenance(state: &AppState) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    let rows=sqlx::query(&db::q("SELECT session_json FROM experience_sessions WHERE (state != 'ended' AND last_activity_at <= ?) OR (state = 'running' AND deadline IS NOT NULL AND deadline <= ?) OR (state = 'ended' AND updated_at < ?) LIMIT 128"))
        .bind(now-db::experiences::IDLE_TIMEOUT_SECONDS).bind(now).bind(now-30*86400).fetch_all(&state.db).await?;
    for row in rows {
        let mut session: Session = serde_json::from_str(row.get("session_json"))
            .map_err(|_| AppError::Internal("Invalid session".into()))?;
        session.normalize_activity();
        if session.state == "ended" {
            sqlx::query(&db::q(
                "DELETE FROM experience_sessions WHERE id = ? AND revision = ?",
            ))
            .bind(&session.id)
            .bind(session.revision)
            .execute(&state.db)
            .await?;
        } else if session.idle_expired(now) {
            match remove_idle_session(state, &mut session).await {
                Ok(()) | Err(AppError::SessionChanged) => {}
                Err(error) => return Err(error),
            }
        } else {
            if session.deadline.is_some_and(|d| d <= now) {
                let winner = session
                    .participants
                    .iter()
                    .find(|p| {
                        p.role == "player" && Some(&p.user_id) != session.turn_user_id.as_ref()
                    })
                    .map(|p| p.user_id.clone());
                session.end("turn_timeout", winner);
            } else {
                session.end("abandoned", None);
            }
            if db::experiences::save(&state.db, &mut session).await.is_ok() {
                broadcast(state, &session).await;
            }
        }
    }
    Ok(())
}

pub fn spawn_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(e) = maintenance(&state).await {
                tracing::warn!("Experience maintenance failed: {e}");
            }
        }
    });
}
