pub mod policy;
pub mod routes;
pub mod scanner;
pub mod video;

use crate::{
    db,
    error::AppError,
    middleware::{auth::AuthUser, permissions},
    models::channel::ChannelRow,
    state::AppState,
};
use policy::{Action, Policy, Trigger};
use scanner::{ScanResult, Scanner};
use serde::Serialize;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub struct AutoMod {
    pub scanner: Arc<dyn Scanner>,
    pub video: Arc<dyn video::VideoSampler>,
    pub admission: tokio::sync::Mutex<()>,
    pub processing: tokio::sync::Mutex<()>,
    pub publication: tokio::sync::Mutex<()>,
    pub max_held: i64,
    pub max_held_bytes: i64,
    pub scan_timeout: Duration,
}
impl Default for AutoMod {
    fn default() -> Self {
        Self::new(Arc::new(scanner::Unavailable("scanner disabled".into())))
    }
}
impl AutoMod {
    pub fn new(scanner: Arc<dyn Scanner>) -> Self {
        Self {
            scanner,
            video: Arc::new(video::Ffmpeg::default()),
            admission: tokio::sync::Mutex::new(()),
            processing: tokio::sync::Mutex::new(()),
            publication: tokio::sync::Mutex::new(()),
            max_held: 1000,
            max_held_bytes: 1024 * 1024 * 1024,
            scan_timeout: Duration::from_secs(30),
        }
    }
    pub fn from_env() -> Self {
        let env = |key| std::env::var(key).unwrap_or_default();
        let backend = env("ACCORD_AUTOMOD_SCANNER");
        let scanner: Result<Arc<dyn Scanner>, String> = match backend.as_str() {
            "" | "none" => Ok(Arc::new(scanner::Unavailable("scanner disabled".into()))),
            "local" => scanner::Local::load(
                &PathBuf::from(env("ACCORD_AUTOMOD_MODEL_PATH")),
                &PathBuf::from(env("ACCORD_AUTOMOD_RUNTIME_PATH")),
                env("ACCORD_AUTOMOD_THREADS").parse().unwrap_or(2),
            )
            .map(|s| Arc::new(s) as Arc<dyn Scanner>),
            "http" => scanner::Http::new(
                env("ACCORD_AUTOMOD_SCANNER_URL"),
                env("ACCORD_AUTOMOD_SCANNER_SECRET"),
                env("ACCORD_AUTOMOD_MODEL_VERSION"),
            )
            .map(|s| Arc::new(s) as Arc<dyn Scanner>),
            _ => Err("unknown automod scanner backend".into()),
        };
        let mut this = Self::new(scanner.unwrap_or_else(|e| {
            tracing::error!("automod scanner unavailable: {e}");
            Arc::new(scanner::Unavailable(e))
        }));
        this.video = Arc::new(video::Ffmpeg::from_env());
        this.max_held = env("ACCORD_AUTOMOD_MAX_HELD")
            .parse::<i64>()
            .unwrap_or(1000)
            .clamp(1, 100_000);
        this.max_held_bytes = env("ACCORD_AUTOMOD_MAX_HELD_BYTES")
            .parse::<i64>()
            .unwrap_or(1024 * 1024 * 1024)
            .clamp(1024 * 1024, 100 * 1024 * 1024 * 1024);
        this
    }
}
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Upload {
    pub id: String,
    pub message_id: String,
    pub channel_id: String,
    pub space_id: Option<String>,
    pub author_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub hash: String,
    pub status: String,
    pub reason: String,
    pub result: Option<String>,
    pub rule_id: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub next_attempt: i64,
    pub attempts: i64,
}
pub async fn get(state: &AppState, id: &str) -> Result<Upload, AppError> {
    Ok(
        sqlx::query_as(&db::q("SELECT * FROM automod_uploads WHERE id = ?"))
            .bind(id)
            .fetch_one(&state.db)
            .await?,
    )
}
pub fn private_path(state: &AppState, id: &str) -> PathBuf {
    state.storage_path.join("automod").join(id)
}
pub use crate::storage::content_hash as hash;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn io(e: std::io::Error) -> AppError {
    AppError::Internal(format!("automod storage: {e}"))
}
/// Scope key for policies, hash blocks and events: the space ID, or `*` for
/// channels without a space (DMs) and instance-wide entries.
pub fn scope_key(space_id: Option<&str>) -> &str {
    space_id.unwrap_or("*")
}
impl Upload {
    pub fn scope(&self) -> &str {
        scope_key(self.space_id.as_deref())
    }
}

/// One file from a multipart message upload. The digest is computed once, while
/// the body is being read, and reused by the denylist check, the queue and the
/// stored attachment row.
pub struct UploadFile {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub hash: String,
}

/// Admission for direct uploads that bypass the message pipeline: emoji,
/// avatars, icons, banners and sounds. Charges the uploader's upload budget and
/// rejects bytes whose digest is blocked instance-wide or in `space_id`, so a
/// file a moderator blocked cannot be republished through another route.
/// Returns the digest.
pub async fn admit_direct_upload(
    state: &AppState,
    uploader: &str,
    space_id: Option<&str>,
    bytes: &[u8],
) -> Result<String, AppError> {
    crate::middleware::rate_limit::charge_upload(state, uploader, 1, bytes.len())?;
    let digest = hash(bytes);
    if policy::hash_blocked_in_scope(state, space_id, &digest).await? {
        return Err(AppError::BadRequest("attachment hash is blocked".into()));
    }
    Ok(digest)
}

/// Called under the admission lock, before creating the message or any files.
pub async fn preflight(
    state: &AppState,
    channel: &ChannelRow,
    auth: &AuthUser,
    files: &[UploadFile],
) -> Result<Option<Policy>, AppError> {
    if files.is_empty() {
        return Ok(None);
    }
    let policy = policy::load(state, channel.space_id.as_deref()).await?;
    let enabled = policy.enabled && !policy::exempt(state, &policy, channel, auth).await?;
    // Explicit blocks are useful without a model or an enabled scanning policy.
    // A matching enabled hash rule may instead quarantine/timeout the upload.
    if !(enabled && policy.has_hash_rule(channel)) {
        for file in files {
            if policy::hash_blocked(state, channel, &file.hash).await? {
                return Err(AppError::BadRequest("attachment hash is blocked".into()));
            }
        }
    }
    if !enabled {
        return Ok(None);
    }
    let (count,size): (i64,i64) = sqlx::query_as("SELECT COUNT(*), CAST(COALESCE(SUM(size),0) AS BIGINT) FROM automod_uploads WHERE status IN ('pending','quarantined','rejected')").fetch_one(&state.db).await?;
    if count + files.len() as i64 > state.automod.max_held
        || size + files.iter().map(|f| f.bytes.len() as i64).sum::<i64>()
            > state.automod.max_held_bytes
    {
        return Err(AppError::RateLimited { retry_after: 60 });
    }
    // Deterministic rejects can give an immediate error; model-based decisions
    // are asynchronous and returned via the upload status endpoint/event.
    for file in files {
        for rule in &policy.rules {
            if matches!(rule.action, Action::Reject)
                && policy::non_media_match(state, rule, channel, &auth.user_id, &file.hash).await?
            {
                record_event(
                    state,
                    None,
                    scope_key(channel.space_id.as_deref()),
                    None,
                    "reject",
                    serde_json::json!({"rule_id":rule.id,"author_id":auth.user_id,"hash":file.hash}),
                )
                .await?;
                return Err(AppError::BadRequest(format!(
                    "attachment rejected by automod rule '{}'",
                    rule.id
                )));
            }
        }
    }
    Ok(Some(policy))
}
/// Persist the complete upload batch in one transaction. A worker can only see
/// the batch after all files exist. The IDs are generated by the server.
pub async fn enqueue(
    state: &AppState,
    channel: &ChannelRow,
    message: &str,
    author: &str,
    policy: &Policy,
    files: &[UploadFile],
) -> Result<Vec<String>, AppError> {
    tokio::fs::create_dir_all(state.storage_path.join("automod"))
        .await
        .map_err(io)?;
    let mut ids = Vec::new();
    let outcome = async {
        let mut tx = state.db.begin().await?;
        for file in files {
            let id = crate::snowflake::generate(); ids.push(id.clone());
            tokio::fs::write(private_path(state,&id), &file.bytes).await.map_err(io)?;
            sqlx::query(&db::q("INSERT INTO automod_uploads (id,message_id,channel_id,space_id,author_id,filename,content_type,size,hash,status,created_at,expires_at,next_attempt) VALUES (?,?,?,?,?,?,?,?,?,'pending',?,?,?)"))
                .bind(&id).bind(message).bind(&channel.id).bind(&channel.space_id).bind(author).bind(&file.filename).bind(&file.content_type).bind(file.bytes.len() as i64).bind(&file.hash).bind(now()).bind(now()+i64::from(policy.retention_days)*86400).bind(now()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok::<_,AppError>(())
    }.await;
    if let Err(e) = outcome {
        for id in &ids {
            let _ = tokio::fs::remove_file(private_path(state, id)).await;
        }
        return Err(e);
    }
    Ok(ids)
}

pub async fn record_event(
    state: &AppState,
    upload: Option<&str>,
    scope: &str,
    actor: Option<&str>,
    action: &str,
    details: serde_json::Value,
) -> Result<(), AppError> {
    let mut conn = state.db.acquire().await?;
    record_event_in(&mut conn, upload, scope, actor, action, details).await
}
/// [`record_event`] on an explicit connection, so an event can commit together
/// with the change it describes (pass `&mut *tx`).
pub async fn record_event_in(
    conn: &mut sqlx::AnyConnection,
    upload: Option<&str>,
    scope: &str,
    actor: Option<&str>,
    action: &str,
    details: serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(&db::q("INSERT INTO automod_events (id,upload_id,scope_id,actor_id,action,details,created_at) VALUES (?,?,?,?,?,?,?)"))
        .bind(crate::snowflake::generate()).bind(upload).bind(scope).bind(actor).bind(action).bind(details.to_string()).bind(now()).execute(&mut *conn).await?;
    Ok(())
}
async fn scan(state: &AppState, upload: &Upload) -> Result<ScanResult, String> {
    let version = format!(
        "{}:{}",
        state.automod.scanner.version(),
        video::CACHE_VERSION
    );
    let cached: Option<(String,)> = sqlx::query_as(&db::q("SELECT result FROM automod_cache WHERE hash = ? AND scanner_version = ? AND expires_at > ?"))
        .bind(&upload.hash).bind(&version).bind(now()).fetch_optional(&state.db).await.map_err(|e|e.to_string())?;
    if let Some((cached,)) = cached {
        let result: ScanResult = serde_json::from_str(&cached).map_err(|e| e.to_string())?;
        result.validate()?;
        return Ok(result);
    }
    let bytes = tokio::fs::read(private_path(state, &upload.id))
        .await
        .map_err(|e| e.to_string())?;
    if hash(&bytes) != upload.hash {
        return Err("stored upload hash mismatch".into());
    }
    let result = tokio::time::timeout(state.automod.scan_timeout, async {
        if let Some(container) = video::container(&bytes) {
            let samples = state
                .automod
                .video
                .sample(&private_path(state, &upload.id), container)
                .await?;
            if samples.frames.len() != video::SAMPLE_COUNT
                || samples.timestamps_ms.len() != video::SAMPLE_COUNT
            {
                return Err(format!(
                    "video decoder did not return {} samples",
                    video::SAMPLE_COUNT
                ));
            }
            let mut combined: Option<ScanResult> = None;
            for frame in samples.frames {
                let next = state.automod.scanner.scan(frame).await?;
                next.validate()?;
                if let Some(result) = combined.as_mut() {
                    if next.model_version != result.model_version
                        || next.scores.keys().ne(result.scores.keys())
                    {
                        return Err("scanner response changed between video frames".into());
                    }
                    for (category, score) in next.scores {
                        let current = result.scores.get_mut(&category).unwrap();
                        *current = current.max(score);
                    }
                } else {
                    combined = Some(next);
                }
            }
            let mut result = combined.ok_or("video samples unavailable")?;
            result.sampled_timestamps_ms = samples.timestamps_ms;
            Ok(result)
        } else {
            state.automod.scanner.scan(bytes).await
        }
    })
    .await
    .map_err(|_| "scanner timed out")??;
    result.validate()?;
    sqlx::query(&db::q("INSERT INTO automod_cache (hash,scanner_version,result,expires_at) VALUES (?,?,?,?) ON CONFLICT(hash,scanner_version) DO UPDATE SET result=excluded.result, expires_at=excluded.expires_at"))
        .bind(&upload.hash).bind(&version).bind(serde_json::to_string(&result).map_err(|e|e.to_string())?).bind(now()+CACHE_TTL_SECS).execute(&state.db).await.map_err(|e|e.to_string())?;
    Ok(result)
}

/// Seconds a failed scan waits before the worker retries it.
const RETRY_DELAY_SECS: i64 = 60;
/// Lifetime of a cached scan result.
const CACHE_TTL_SECS: i64 = 86400;
/// Upper bound on cached scan results, independent of the upload queue.
const CACHE_MAX_ROWS: i64 = 10000;
/// Age before a private file with no queue row is treated as a crash leftover.
const ORPHAN_GRACE: Duration = Duration::from_secs(3600);

/// Why an upload is held without consulting the model. `None` means the
/// uploader still has standing and the policy rules decide.
async fn author_hold(
    state: &AppState,
    upload: &Upload,
    channel: &ChannelRow,
) -> Result<Option<&'static str>, AppError> {
    let author = match db::users::get_user(&state.db, &upload.author_id).await {
        Ok(u) if !u.disabled => AuthUser {
            user_id: u.id,
            is_admin: u.is_admin,
            is_bot: u.bot,
            is_guest: false,
            guest_space_id: None,
        },
        Ok(_) | Err(AppError::NotFound(_)) => return Ok(Some("uploader is unavailable")),
        Err(e) => return Err(e),
    };
    let mut allowed =
        permissions::require_channel_permission(&state.db, &channel.id, &author, "send_messages")
            .await
            .is_ok();
    if let Some(space) = channel.space_id.as_deref().filter(|s| !s.is_empty()) {
        allowed &= permissions::require_not_timed_out(&state.db, space, &author)
            .await
            .is_ok();
    }
    Ok((!allowed).then_some("uploader no longer has permission to post"))
}

/// One bounded worker per server; retries and held uploads survive restarts.
///
/// `processing` is held while a pending upload is claimed and while its
/// decision is committed, but never during inference: policy updates, reviews
/// and the per-second cleanup must not queue behind a video scan. After a scan
/// the row and policy are reloaded under the lock and the decision re-derived,
/// so a review or policy change made while the scanner was busy always wins.
pub async fn process_one(state: &AppState) -> Result<bool, AppError> {
    let candidate: Option<(String,)> = {
        let _guard = state.automod.processing.lock().await;
        let _admission = state.automod.admission.lock().await;
        sqlx::query_as(&db::q("SELECT id FROM automod_uploads WHERE status = 'pending' AND next_attempt <= ? ORDER BY next_attempt,id LIMIT 1"))
            .bind(now()).fetch_optional(&state.db).await?
    };
    let Some((id,)) = candidate else {
        return Ok(false);
    };
    let mut result: Option<ScanResult> = None;
    'claim: loop {
        let guard = state.automod.processing.lock().await;
        let upload: Option<Upload> = sqlx::query_as(&db::q(
            "SELECT * FROM automod_uploads WHERE id = ? AND status = 'pending'",
        ))
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
        let Some(upload) = upload else {
            // Reviewed, expired or retried by someone else while we were scanning.
            return Ok(true);
        };
        let channel = match db::channels::get_channel_row(&state.db, &upload.channel_id).await {
            Ok(c) => Some(c),
            Err(AppError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let policy = policy::load(state, upload.space_id.as_deref()).await?;
        let hold: Option<(&str, &str)> = if upload.expires_at <= now() {
            Some(("removed", "retention window expired"))
        } else if channel.is_none() {
            Some(("quarantined", "original channel was deleted"))
        } else if !policy.enabled {
            Some((
                "quarantined",
                "policy disabled while upload was pending; review required",
            ))
        } else {
            let channel = channel.as_ref().unwrap();
            if policy::hash_blocked(state, channel, &upload.hash).await?
                && !policy.has_hash_rule(channel)
            {
                Some(("quarantined", "attachment hash is blocked"))
            } else {
                author_hold(state, &upload, channel)
                    .await?
                    .map(|reason| ("quarantined", reason))
            }
        };
        if let Some((status, reason)) = hold {
            finish(state, &upload, status, reason, Decision::default()).await?;
            return Ok(true);
        }
        let channel = channel.unwrap();
        let mut matched = None;
        for rule in policy.rules.iter().filter(|r| r.applies(&channel)) {
            let fires = if let Trigger::Media {
                categories,
                threshold,
            } = &rule.trigger
            {
                if result.is_none() {
                    // Inference runs without the lock; re-evaluate from scratch afterwards.
                    drop(guard);
                    match scan(state, &upload).await {
                        Ok(r) => result = Some(r),
                        Err(e) => {
                            sqlx::query(&db::q("UPDATE automod_uploads SET reason = ?, attempts = attempts + 1, next_attempt = ? WHERE id = ? AND status = 'pending'"))
                                .bind(&e).bind(now()+RETRY_DELAY_SECS).bind(&upload.id).execute(&state.db).await?;
                            if upload.reason != e {
                                record_event(
                                    state,
                                    Some(&upload.id),
                                    upload.scope(),
                                    None,
                                    "scan_failed",
                                    serde_json::json!({"reason":e}),
                                )
                                .await?;
                                notify(state, &upload, "pending").await;
                            }
                            return Ok(true);
                        }
                    }
                    continue 'claim;
                }
                let scores = &result.as_ref().unwrap().scores;
                if categories.iter().any(|c| !scores.contains_key(c)) {
                    finish(
                        state,
                        &upload,
                        "quarantined",
                        "scanner does not support a configured category",
                        Decision {
                            rule: Some(rule),
                            result: result.as_ref(),
                            reviewer: None,
                        },
                    )
                    .await?;
                    return Ok(true);
                }
                categories.iter().any(|c| scores[c] >= *threshold)
            } else {
                policy::non_media_match(state, rule, &channel, &upload.author_id, &upload.hash)
                    .await?
            };
            if fires {
                matched = Some(rule);
                break;
            }
        }
        let status = match matched.map(|r| &r.action) {
            None | Some(Action::Flag) => "published",
            Some(Action::Reject) => "rejected",
            Some(Action::Quarantine | Action::Timeout { .. }) => "quarantined",
        };
        finish(
            state,
            &upload,
            status,
            if matched.is_some() {
                "automod rule matched"
            } else {
                "no rule matched"
            },
            Decision {
                rule: matched,
                result: result.as_ref(),
                reviewer: None,
            },
        )
        .await?;
        return Ok(true);
    }
}

/// What a decision was based on. All fields are optional: automatic holds carry
/// nothing, worker decisions carry the rule and scan, reviews carry the reviewer.
#[derive(Clone, Copy, Default)]
pub struct Decision<'a> {
    pub rule: Option<&'a policy::Rule>,
    pub result: Option<&'a ScanResult>,
    pub reviewer: Option<&'a str>,
}

/// Caller holds processing. DB visibility, decision, report and audit log commit
/// together. The private original is retained for held decisions, even if the
/// author later deletes the message. Public files have no route until commit.
pub async fn finish(
    state: &AppState,
    upload: &Upload,
    requested_status: &str,
    reason: &str,
    decision: Decision<'_>,
) -> Result<(), AppError> {
    let Decision {
        rule,
        result,
        reviewer,
    } = decision;
    // Serialize file publication with deletion cleanup, without holding this
    // lock during inference (API mutation middleware also drains deletions).
    let _publication = state.automod.publication.lock().await;
    let actor = match reviewer {
        Some(id) => id.to_string(),
        None => db::users::get_or_create_system_user(&state.db).await?,
    };
    let mut status = requested_status;
    let mut reason = reason.to_string();
    let mut public_file = None;
    let mut dimensions: (Option<i64>, Option<i64>) = (None, None);
    if status == "published" {
        match db::messages::get_message_row(&state.db, &upload.message_id).await {
            Ok(message) if message.channel_id == upload.channel_id => {
                let bytes = tokio::fs::read(private_path(state, &upload.id))
                    .await
                    .map_err(io)?;
                if hash(&bytes) != upload.hash {
                    return Err(AppError::Internal("stored upload hash mismatch".into()));
                }
                let (url, _) = crate::storage::save_attachment(
                    &state.storage_path,
                    &upload.channel_id,
                    &upload.id,
                    &upload.filename,
                    &bytes,
                    bytes.len(),
                )
                .await?;
                // Same detection as a directly published attachment, so a
                // released file gets identical metadata.
                if upload.content_type.starts_with("image/") {
                    dimensions = crate::routes::messages::detect_image_dimensions(&bytes);
                }
                public_file = Some(url);
            }
            Ok(_) | Err(AppError::NotFound(_)) => {
                status = "quarantined";
                reason = "original message was deleted; cannot release".into();
            }
            Err(e) => return Err(e),
        }
    }
    let result_json = result
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let details = serde_json::json!({"reason":reason,"rule":rule,"scan":result,"author_id":upload.author_id,"reviewer_id":reviewer});
    let expires_at = if upload.status == "published" {
        now()
            + i64::from(
                policy::load(state, upload.space_id.as_deref())
                    .await?
                    .retention_days,
            ) * 86400
    } else {
        upload.expires_at
    };
    let mut tx = state.db.begin().await?;
    let changed = sqlx::query(&db::q("UPDATE automod_uploads SET status=?,reason=?,expires_at=?,file_removed=0,rule_id=COALESCE(?,rule_id),result=COALESCE(?,result) WHERE id=? AND status=?"))
        .bind(status).bind(&reason).bind(expires_at).bind(rule.map(|r|r.id.as_str())).bind(result_json).bind(&upload.id).bind(&upload.status).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(AppError::Conflict(
            "upload decision changed; reload before reviewing".into(),
        ));
    }
    if upload.status == "published" && status != "published" {
        sqlx::query(&db::q("DELETE FROM attachments WHERE id = ?"))
            .bind(&upload.id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(url) = &public_file {
        sqlx::query(&db::q("INSERT INTO attachments (id,message_id,filename,content_type,size,url,width,height,content_hash) SELECT ?,id,?,?,?,?,?,?,? FROM messages WHERE id=? AND channel_id=?"))
            .bind(&upload.id).bind(&upload.filename).bind(&upload.content_type).bind(upload.size).bind(url).bind(dimensions.0).bind(dimensions.1).bind(&upload.hash).bind(&upload.message_id).bind(&upload.channel_id).execute(&mut *tx).await?.rows_affected().eq(&1).then_some(()).ok_or_else(||AppError::Conflict("original message was deleted".into()))?;
    }
    record_event_in(
        &mut tx,
        Some(&upload.id),
        upload.scope(),
        Some(&actor),
        status,
        details.clone(),
    )
    .await?;
    let mut audit_entry = None;
    if let Some(space) = &upload.space_id {
        // Deleted spaces retain the independent automod event ledger.
        let (exists,): (i64,) = sqlx::query_as(&db::q("SELECT COUNT(*) FROM spaces WHERE id=?"))
            .bind(space)
            .fetch_one(&mut *tx)
            .await?;
        if exists > 0 {
            audit_entry = Some(
                db::audit_log::create_entry_in(
                    &mut tx,
                    space,
                    &actor,
                    &format!("automod.{status}"),
                    Some(&upload.id),
                    Some("attachment"),
                    Some(&reason),
                    Some(&details.to_string()),
                )
                .await?,
            );
        }
    }
    if let Some(rule) = rule {
        if matches!(rule.action, Action::Flag) && status == "published" {
            db::reports::create_report_in(
                &mut tx,
                upload.space_id.as_deref(),
                &actor,
                "message",
                &upload.message_id,
                Some(&upload.channel_id),
                "nsfw",
                Some(&format!(
                    "AutoMod rule {}: attachment {}",
                    rule.id, upload.id
                )),
            )
            .await?;
        }
        if let (Action::Timeout { seconds }, Some(space)) = (&rule.action, &upload.space_id) {
            let until = (chrono::Utc::now() + chrono::Duration::seconds(i64::from(*seconds)))
                .format("%Y-%m-%dT%H:%M:%S+00:00")
                .to_string();
            // Never punish the owner or an instance admin. Existing longer
            // timeouts are preserved. Timeout only comes from explicit trust/hash rules.
            sqlx::query(&db::q("UPDATE members SET timed_out_until=? WHERE space_id=? AND user_id=? AND (timed_out_until IS NULL OR timed_out_until < ?) AND user_id NOT IN (SELECT owner_id FROM spaces WHERE id=?) AND user_id NOT IN (SELECT id FROM users WHERE is_admin=TRUE)"))
                .bind(&until).bind(space).bind(&upload.author_id).bind(&until).bind(space).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    if status == "published" || status == "removed" {
        let _ = tokio::fs::remove_file(private_path(state, &upload.id)).await;
    }
    if let Some(entry) = &audit_entry {
        crate::routes::audit_log::broadcast_entry(state, entry).await;
    }
    if status == "published" || upload.status == "published" {
        if let Ok(message) = db::messages::get_message_row(&state.db, &upload.message_id).await {
            let attachments =
                db::attachments::get_attachments_for_message(&state.db, &upload.message_id).await?;
            let data = crate::routes::messages::message_row_to_json_with_attachments(
                &message,
                &attachments,
                None,
            );
            broadcast(
                state,
                upload,
                "message.update",
                data,
                "messages",
                None,
                None,
            )
            .await;
        }
    }
    notify(state, upload, status).await;
    Ok(())
}
async fn broadcast(
    state: &AppState,
    upload: &Upload,
    kind: &str,
    data: serde_json::Value,
    intent: &str,
    targets: Option<Vec<String>>,
    required_permission: Option<&'static str>,
) {
    if let Some(tx) = &*state.gateway_tx.read().await {
        let _ = tx.send(crate::gateway::events::GatewayBroadcast {
            space_id: upload.space_id.clone(),
            target_user_ids: targets,
            intent: intent.into(),
            event: serde_json::json!({"op":0,"type":kind,"data":data}),
            required_permission,
        });
    }
}
async fn notify(state: &AppState, upload: &Upload, status: &str) {
    // Payload contains IDs/status only. A fresh permission check at gateway
    // delivery gates moderator notifications; the status endpoint gates details.
    let data = serde_json::json!({"id":upload.id,"message_id":upload.message_id,"channel_id":upload.channel_id,"space_id":upload.space_id,"status":status});
    broadcast(
        state,
        upload,
        "automod.upload_update",
        data.clone(),
        "moderation",
        None,
        Some("moderate_members"),
    )
    .await;
    broadcast(
        state,
        upload,
        "automod.upload_status",
        data,
        "messages",
        Some(vec![upload.author_id.clone()]),
        None,
    )
    .await;
}
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut last_hourly = tokio::time::Instant::now() - ORPHAN_GRACE;
    loop {
        interval.tick().await;
        if last_hourly.elapsed() >= ORPHAN_GRACE {
            if let Err(e) = reconcile_orphans(&state).await {
                tracing::warn!("automod orphan cleanup: {e}");
            }
            if let Err(e) = prune_cache(&state).await {
                tracing::warn!("automod cache pruning: {e}");
            }
            last_hourly = tokio::time::Instant::now();
        }
        if let Err(e) = cleanup(&state).await {
            tracing::warn!("automod cleanup will retry: {e}");
        }
        for _ in 0..8 {
            match process_one(&state).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    tracing::warn!("automod worker will retry: {e}");
                    break;
                }
            }
        }
    }
}
/// Per-second housekeeping. Both queries are index-backed, so holding
/// `processing` here is cheap.
pub async fn cleanup(state: &AppState) -> Result<(), AppError> {
    let _guard = state.automod.processing.lock().await;
    let expired: Vec<Upload> = sqlx::query_as(&db::q("SELECT * FROM automod_uploads WHERE status IN ('pending','quarantined','rejected') AND expires_at <= ? LIMIT 100")).bind(now()).fetch_all(&state.db).await?;
    for upload in expired {
        finish(
            state,
            &upload,
            "removed",
            "retention window expired",
            Decision::default(),
        )
        .await?;
    }
    // Retry failed unlinks, including a crash after a terminal DB commit.
    let terminal: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM automod_uploads WHERE status IN ('published','removed') AND file_removed=0 LIMIT 100")
            .fetch_all(&state.db)
            .await?;
    for (id,) in terminal {
        match tokio::fs::remove_file(private_path(state, &id)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        sqlx::query(&db::q(
            "UPDATE automod_uploads SET file_removed=1 WHERE id=?",
        ))
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}
/// Hourly: drop expired cache rows and bound the cache size. The size bound
/// sorts the whole table, which is why this is not part of `cleanup`.
pub async fn prune_cache(state: &AppState) -> Result<(), AppError> {
    sqlx::query(&db::q("DELETE FROM automod_cache WHERE expires_at <= ?"))
        .bind(now())
        .execute(&state.db)
        .await?;
    sqlx::query(&db::q("DELETE FROM automod_cache WHERE (hash,scanner_version) NOT IN (SELECT hash,scanner_version FROM automod_cache ORDER BY expires_at DESC LIMIT ?)"))
        .bind(CACHE_MAX_ROWS)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Reap files left by a crash before the enqueue transaction committed. The
/// grace period and admission lock protect uploads still being written.
pub async fn reconcile_orphans(state: &AppState) -> Result<(), AppError> {
    let _admission = state.automod.admission.lock().await;
    let mut entries = match tokio::fs::read_dir(state.storage_path.join("automod")).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    while let Some(entry) = entries.next_entry().await.map_err(io)? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let meta = entry.metadata().await.map_err(io)?;
        if !meta.is_file()
            || meta.modified().map_err(io)?.elapsed().unwrap_or_default() < ORPHAN_GRACE
        {
            continue;
        }
        let (exists,): (i64,) =
            sqlx::query_as(&db::q("SELECT COUNT(*) FROM automod_uploads WHERE id=?"))
                .bind(&name)
                .fetch_one(&state.db)
                .await?;
        if exists == 0 {
            tokio::fs::remove_file(entry.path()).await.map_err(io)?;
        }
    }
    Ok(())
}
