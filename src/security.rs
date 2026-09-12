//! Shared, bounded admission controls and local operator utilities.
use crate::{error::AppError, models::voice::VoiceState, state::AppState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const MAX_TRACKERS: usize = 10_000;

pub struct Admission {
    global: Arc<Semaphore>,
    keys: Mutex<HashMap<String, Weak<Semaphore>>>,
    per_key: usize,
}

pub struct AdmissionPermit {
    _global: OwnedSemaphorePermit,
    _key: OwnedSemaphorePermit,
}

impl Admission {
    pub fn new(global: usize, per_key: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            keys: Mutex::new(HashMap::new()),
            per_key,
        }
    }

    pub fn enter(&self, key: &str) -> Result<AdmissionPermit, AppError> {
        let busy = || AppError::RateLimited { retry_after: 5 };
        let global = self
            .global
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy())?;
        let mut keys = self.keys.lock().unwrap();
        keys.retain(|_, v| v.strong_count() > 0);
        let semaphore = keys.get(key).and_then(Weak::upgrade).unwrap_or_else(|| {
            let semaphore = Arc::new(Semaphore::new(self.per_key));
            keys.insert(key.to_string(), Arc::downgrade(&semaphore));
            semaphore
        });
        let key = semaphore.try_acquire_owned().map_err(|_| busy())?;
        Ok(AdmissionPermit {
            _global: global,
            _key: key,
        })
    }
}

pub struct SecurityState {
    pub unfurls: Admission,
    pub sockets: Admission,
    pub gateway_users: Admission,
    pub tracker_lock: Mutex<()>,
    pub upload_limits: dashmap::DashMap<String, crate::middleware::rate_limit::UploadBucket>,
    voice_cleanup: tokio::sync::Mutex<()>,
}

impl Default for SecurityState {
    fn default() -> Self {
        Self {
            unfurls: Admission::new(32, 2),
            sockets: Admission::new(512, 32),
            gateway_users: Admission::new(512, 8),
            tracker_lock: Mutex::new(()),
            upload_limits: dashmap::DashMap::new(),
            voice_cleanup: tokio::sync::Mutex::new(()),
        }
    }
}

/// Reserve tracker space before doing expensive authentication work.
pub fn reserve_tracker<T>(
    state: &AppState,
    map: &dashmap::DashMap<String, T>,
    key: &str,
    expired: impl Fn(&T) -> bool,
    initial: impl FnOnce() -> T,
) -> Result<(), AppError> {
    let _guard = state.security.tracker_lock.lock().unwrap();
    map.retain(|_, value| !expired(value));
    if !map.contains_key(key) {
        if map.len() >= MAX_TRACKERS {
            return Err(AppError::RateLimited { retry_after: 60 });
        }
        map.insert(key.to_string(), initial());
    }
    Ok(())
}

pub fn redact_database_url(url: &str) -> String {
    if url.starts_with("sqlite:") {
        return url.to_string();
    }
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("REDACTED"));
            }
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => "[database URL redacted]".into(),
    }
}

/// Explicit local provisioning; never invoked by public registration.
pub async fn bootstrap_admin(
    pool: &sqlx::AnyPool,
    username: &str,
    password: &str,
) -> Result<(), AppError> {
    use argon2::{password_hash::SaltString, PasswordHasher};
    if username.trim() != username
        || username.is_empty()
        || username.len() > 32
        || password.len() < 16
        || password.len() > 128
    {
        return Err(AppError::BadRequest(
            "admin username must be 1–32 characters and password 16–128 characters".into(),
        ));
    }
    let password = password.to_string();
    let hash = tokio::task::spawn_blocking(move || {
        argon2::Argon2::default()
            .hash_password(
                password.as_bytes(),
                &SaltString::generate(&mut rand::rngs::OsRng),
            )
            .map(|h| h.to_string())
    })
    .await
    .map_err(|_| AppError::Internal("password hashing task failed".into()))?
    .map_err(|_| AppError::Internal("password hashing failed".into()))?;
    sqlx::query(&crate::db::q("INSERT INTO users (id, username, display_name, password_hash, is_admin) VALUES (?, ?, ?, ?, TRUE)"))
        .bind(crate::snowflake::generate()).bind(username).bind(username).bind(hash).execute(pool).await?;
    Ok(())
}

/// Drop a single voice session: evict the participant from LiveKit, clear the
/// in-memory voice state, and announce the departure.
///
/// `old` is the session as the caller observed it. The in-memory entry is only
/// cleared when it still matches, so a user who has since moved to a channel
/// they are still allowed in is not pulled out of it.
async fn drop_voice_session(state: &AppState, old: &VoiceState) {
    let Some(channel) = old.channel_id.as_deref() else {
        return;
    };
    if let Err(err) = evict_voice_participant(state, channel, &old.user_id).await {
        tracing::warn!("voice eviction could not be queued: {err}");
        return;
    }
    if state
        .voice_states
        .remove_if(&old.user_id, |_, current| {
            current.channel_id == old.channel_id && current.session_id == old.session_id
        })
        .is_some()
    {
        let mut left = old.clone();
        left.channel_id = None;
        crate::routes::voice::broadcast_voice_state_update(
            state,
            channel,
            old.space_id.as_deref(),
            &left,
        )
        .await;
    }
}

/// Revoke voice access for every live session matching `matches`.
///
/// Access-revocation paths call this synchronously rather than leaving the work
/// to the periodic [`reconcile_voice_access`] sweep: until the sweep ran, a
/// removed participant would still be publishing and subscribing media in a
/// room they no longer have access to.
async fn revoke_voice_sessions(state: &AppState, matches: impl Fn(&VoiceState) -> bool) {
    let targets: Vec<VoiceState> = state
        .voice_states
        .iter()
        .filter(|entry| matches(entry.value()))
        .map(|entry| entry.value().clone())
        .collect();
    for old in targets {
        drop_voice_session(state, &old).await;
    }
}

fn targets_user(user_id: Option<&str>, candidate: &str) -> bool {
    match user_id {
        Some(id) => candidate == id,
        None => true,
    }
}

/// Revoke voice access within a space: one member for a kick, ban or leave, or
/// every member when the space itself is deleted.
pub async fn revoke_space_voice_access(state: &AppState, space_id: &str, user_id: Option<&str>) {
    revoke_voice_sessions(state, |vs| {
        vs.space_id.as_deref() == Some(space_id) && targets_user(user_id, &vs.user_id)
    })
    .await;
}

/// Revoke voice access within a single channel: one participant for a DM
/// removal, or everyone when the channel is deleted.
pub async fn revoke_channel_voice_access(
    state: &AppState,
    channel_id: &str,
    user_id: Option<&str>,
) {
    revoke_voice_sessions(state, |vs| {
        vs.channel_id.as_deref() == Some(channel_id) && targets_user(user_id, &vs.user_id)
    })
    .await;
}

/// Revoke every voice session a user holds, wherever it is — for account
/// disable and account deletion.
pub async fn revoke_user_voice_access(state: &AppState, user_id: &str) {
    revoke_voice_sessions(state, |vs| vs.user_id == user_id).await;
}

/// Reconcile REST, gateway and cascaded membership changes against current voice access.
/// Failed evictions remain in a durable queue for the periodic worker.
pub async fn reconcile_voice_access(state: &AppState) {
    let Ok(_guard) = state.security.voice_cleanup.try_lock() else {
        return;
    };
    if let Err(err) = drain_voice_evictions(state).await {
        tracing::warn!("voice eviction retry failed: {err}");
    }
    let voices: Vec<_> = state
        .voice_states
        .iter()
        .map(|v| v.value().clone())
        .collect();
    for old in voices {
        let Some(channel) = old.channel_id.as_deref() else {
            continue;
        };
        let allowed = match crate::db::users::get_user(&state.db, &old.user_id).await {
            Ok(user) if !user.disabled => {
                let auth = crate::middleware::auth::AuthUser {
                    user_id: old.user_id.clone(),
                    is_admin: false,
                    is_bot: false,
                    is_guest: false,
                    guest_space_id: None,
                };
                crate::middleware::permissions::require_channel_permission(
                    &state.db, channel, &auth, "connect",
                )
                .await
                .is_ok()
            }
            _ => false,
        };
        if allowed {
            continue;
        }
        drop_voice_session(state, &old).await;
    }
}

/// Persist eviction before contacting LiveKit so failures survive disconnects and restarts.
pub async fn evict_voice_participant(
    state: &AppState,
    channel: &str,
    user: &str,
) -> Result<(), AppError> {
    sqlx::query(&crate::db::q("INSERT INTO voice_evictions (channel_id, user_id) VALUES (?, ?) ON CONFLICT(channel_id, user_id) DO NOTHING"))
        .bind(channel).bind(user).execute(&state.db).await?;
    attempt_voice_eviction(state, channel, user).await
}

async fn attempt_voice_eviction(
    state: &AppState,
    channel: &str,
    user: &str,
) -> Result<(), AppError> {
    let removed = if state.test_mode {
        true
    } else if let Some(lk) = state.livekit_client.as_ref() {
        lk.try_remove_participant(channel, user).await
    } else {
        false
    };
    if removed {
        sqlx::query(&crate::db::q(
            "DELETE FROM voice_evictions WHERE channel_id = ? AND user_id = ?",
        ))
        .bind(channel)
        .bind(user)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

async fn drain_voice_evictions(state: &AppState) -> Result<(), AppError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT channel_id, user_id FROM voice_evictions LIMIT 100")
            .fetch_all(&state.db)
            .await?;
    for (channel, user) in rows {
        attempt_voice_eviction(state, &channel, &user).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_releases_capacity_and_bounds_keys() {
        let gate = Admission::new(2, 1);
        let a = gate.enter("a").unwrap();
        assert!(gate.enter("a").is_err());
        let b = gate.enter("b").unwrap();
        assert!(gate.enter("c").is_err());
        drop(a);
        drop(b);
        for i in 0..100 {
            drop(gate.enter(&i.to_string()).unwrap());
        }
        assert!(gate.keys.lock().unwrap().len() <= 1);
    }
    #[test]
    fn database_credentials_never_survive_redaction() {
        for url in [
            "postgres://user:SENTINEL@localhost/db?password=SENTINEL#SENTINEL",
            "invalid SENTINEL",
        ] {
            assert!(!redact_database_url(url).contains("SENTINEL"));
        }
    }
}
