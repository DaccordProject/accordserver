use super::{policy::Policy, Upload};
use crate::{
    db,
    error::AppError,
    middleware::{auth::AuthUser, permissions},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use sqlx::Row;

async fn authorize(
    state: &AppState,
    scope: &str,
    auth: &AuthUser,
    configure: bool,
) -> Result<(), AppError> {
    if scope == "*" {
        permissions::require_server_admin(auth)
    } else {
        permissions::require_permission(
            &state.db,
            scope,
            auth,
            if configure {
                "manage_space"
            } else {
                "moderate_members"
            },
        )
        .await
    }
}
pub async fn get_policy(
    State(state): State<AppState>,
    Path(scope): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, true).await?;
    let policy = super::policy::load(&state, Some(&scope)).await?;
    let (overridden,): (i64,) = sqlx::query_as(&db::q(
        "SELECT COUNT(*) FROM automod_policies WHERE scope_id=?",
    ))
    .bind(&scope)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(
        serde_json::json!({"data":{"policy":policy,"inherited":overridden==0 && scope!="*"}}),
    ))
}
pub async fn set_policy(
    State(state): State<AppState>,
    Path(scope): Path<String>,
    auth: AuthUser,
    Json(policy): Json<Policy>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, true).await?;
    policy.validate()?;
    let _guard = state.automod.processing.lock().await;
    sqlx::query(&db::q("INSERT INTO automod_policies (scope_id,policy) VALUES (?,?) ON CONFLICT(scope_id) DO UPDATE SET policy=excluded.policy"))
        .bind(&scope).bind(serde_json::to_string(&policy).map_err(|e|AppError::Internal(e.to_string()))?).execute(&state.db).await?;
    super::record_event(
        &state,
        None,
        &scope,
        Some(&auth.user_id),
        "policy_updated",
        serde_json::json!({"policy":policy}),
    )
    .await?;
    Ok(Json(serde_json::json!({"data":policy})))
}
pub async fn reset_policy(
    State(state): State<AppState>,
    Path(scope): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, true).await?;
    let _guard = state.automod.processing.lock().await;
    sqlx::query(&db::q("DELETE FROM automod_policies WHERE scope_id=?"))
        .bind(&scope)
        .execute(&state.db)
        .await?;
    super::record_event(
        &state,
        None,
        &scope,
        Some(&auth.user_id),
        "policy_reset",
        serde_json::json!({}),
    )
    .await?;
    Ok(Json(serde_json::json!({"data":null})))
}
#[derive(Deserialize)]
pub struct ListQuery {
    pub before: Option<String>,
    pub status: Option<String>,
}
pub async fn list_uploads(
    State(state): State<AppState>,
    Path(scope): Path<String>,
    auth: AuthUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, false).await?;
    let rows: Vec<Upload> = sqlx::query_as(&db::q("SELECT * FROM automod_uploads WHERE (?='*' OR space_id=?) AND (CAST(? AS TEXT) IS NULL OR id < ?) AND (CAST(? AS TEXT) IS NULL OR status=?) ORDER BY id DESC LIMIT 100"))
        .bind(&scope).bind(&scope).bind(&query.before).bind(&query.before).bind(&query.status).bind(&query.status).fetch_all(&state.db).await?;
    Ok(Json(serde_json::json!({"data":rows})))
}
pub async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let upload = super::get(&state, &id).await?;
    if auth.user_id != upload.author_id {
        authorize(&state, upload.scope(), &auth, false).await?;
    }
    Ok(Json(
        serde_json::json!({"data":{"id":upload.id,"message_id":upload.message_id,"status":upload.status,"reason":upload.reason,"rule_id":upload.rule_id,"expires_at":upload.expires_at}}),
    ))
}
pub async fn health(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    permissions::require_server_admin(&auth)?;
    let rows = sqlx::query("SELECT status,COUNT(*) AS count,CAST(COALESCE(SUM(size),0) AS BIGINT) AS bytes FROM automod_uploads GROUP BY status").fetch_all(&state.db).await?;
    let counts: Vec<_> = rows.iter().map(|r|serde_json::json!({"status":r.get::<String,_>("status"),"count":r.get::<i64,_>("count"),"bytes":r.get::<i64,_>("bytes")})).collect();
    Ok(Json(
        serde_json::json!({"data":{"scanner":state.automod.scanner.status(),"video_sampler":state.automod.video.status(),"model_version":state.automod.scanner.version(),"max_held":state.automod.max_held,"max_held_bytes":state.automod.max_held_bytes,"uploads":counts}}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub action: String,
    pub reason: String,
}
pub async fn review(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: AuthUser,
    Json(review): Json<Review>,
) -> Result<Json<serde_json::Value>, AppError> {
    if review.reason.trim().is_empty() || review.reason.len() > 2000 {
        return Err(AppError::BadRequest(
            "review reason must be 1–2000 bytes".into(),
        ));
    }
    let _guard = state.automod.processing.lock().await;
    let upload = super::get(&state, &id).await?;
    authorize(&state, upload.scope(), &auth, false).await?;
    if upload.status == "removed"
        || (upload.status != "published" && upload.expires_at <= super::now())
    {
        return Err(AppError::Conflict(
            "upload retention window has expired".into(),
        ));
    }
    let next = match review.action.as_str() {
        "release" => "published",
        "reject" => "rejected",
        "remove" => "removed",
        "quarantine" => "quarantined",
        "retry" => "pending",
        _ => {
            return Err(AppError::BadRequest(
                "action must be release, reject, remove, quarantine, or retry".into(),
            ))
        }
    };
    if upload.status == "published" && next == "published" {
        return Err(AppError::Conflict("attachment already published".into()));
    }
    if upload.status == "published" {
        // Recover the approved original before withdrawing its public DB row.
        let (url,): (String,) = sqlx::query_as(&db::q("SELECT url FROM attachments WHERE id=?"))
            .bind(&id)
            .fetch_one(&state.db)
            .await?;
        let relative = url
            .strip_prefix("/cdn/attachments/")
            .ok_or_else(|| AppError::BadRequest("not a local attachment".into()))?;
        let source = state.storage_path.join("attachments").join(relative);
        tokio::fs::copy(source, super::private_path(&state, &id))
            .await
            .map_err(super::io)?;
    }
    super::finish(
        &state,
        &upload,
        next,
        &review.reason,
        super::Decision {
            reviewer: Some(&auth.user_id),
            ..Default::default()
        },
    )
    .await?;
    if next == "pending" {
        sqlx::query(&db::q(
            "UPDATE automod_uploads SET next_attempt=? WHERE id=?",
        ))
        .bind(super::now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    }
    Ok(Json(
        serde_json::json!({"data":super::get(&state,&id).await?}),
    ))
}
pub async fn content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: AuthUser,
    req: axum::extract::Request,
) -> Result<axum::response::Response, AppError> {
    use tower::ServiceExt;
    let upload = super::get(&state, &id).await?;
    authorize(&state, upload.scope(), &auth, false).await?;
    if !matches!(
        upload.status.as_str(),
        "pending" | "quarantined" | "rejected"
    ) || upload.expires_at <= super::now()
    {
        return Err(AppError::NotFound("held attachment not found".into()));
    }
    let mut response =
        tower_http::services::ServeFile::new(super::private_path(&state, &upload.id))
            .oneshot(req)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .map(axum::body::Body::new);
    // Held originals are evidence: never cached, never rendered inline, and
    // always inert regardless of what the bytes actually are.
    for (header, value) in [
        (axum::http::header::CACHE_CONTROL, "no-store"),
        (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
        (axum::http::header::CONTENT_DISPOSITION, "attachment"),
        (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            axum::http::header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(header, value.parse().unwrap());
    }
    Ok(response)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashBody {
    pub reason: String,
}
fn validate_hash(hash: &str) -> Result<(), AppError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(AppError::BadRequest(
            "hash must be a lowercase SHA-256 hex digest".into(),
        ));
    }
    Ok(())
}
pub async fn block_hash(
    State(state): State<AppState>,
    Path((scope, hash)): Path<(String, String)>,
    auth: AuthUser,
    Json(body): Json<HashBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, true).await?;
    validate_hash(&hash)?;
    let _guard = state.automod.processing.lock().await;
    let _admission = state.automod.admission.lock().await;
    store_block(&state, &scope, &hash, &body.reason, &auth.user_id, None).await?;
    Ok(Json(serde_json::json!({"data":null})))
}

async fn store_block(
    state: &AppState,
    scope: &str,
    hash: &str,
    reason: &str,
    actor: &str,
    attachment: Option<&str>,
) -> Result<(), AppError> {
    if reason.trim().is_empty() || reason.len() > 2000 {
        return Err(AppError::BadRequest("reason must be 1–2000 bytes".into()));
    }
    let now = super::now();
    let mut tx = state.db.begin().await?;
    sqlx::query(&db::q("INSERT INTO automod_hashes (scope_id,hash,reason,added_by,created_at) VALUES (?,?,?,?,?) ON CONFLICT(scope_id,hash) DO UPDATE SET reason=excluded.reason,added_by=excluded.added_by,created_at=excluded.created_at"))
        .bind(scope).bind(hash).bind(reason).bind(actor).bind(now).execute(&mut *tx).await?;
    if let Some(id) = attachment {
        sqlx::query(&db::q("UPDATE attachments SET content_hash=? WHERE id=?"))
            .bind(hash)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let details = serde_json::json!({"hash":hash,"reason":reason,"attachment_id":attachment});
    super::record_event_in(&mut tx, None, scope, Some(actor), "hash_blocked", details).await?;
    tx.commit().await?;
    Ok(())
}

/// Block an existing attachment without requiring moderators to download it.
/// The normal message-delete action can then remove the original message.
pub async fn block_attachment(
    State(state): State<AppState>,
    Path((scope, id)): Path<(String, String)>,
    auth: AuthUser,
    Json(body): Json<HashBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(&db::q("SELECT a.content_hash,a.url,m.channel_id,m.space_id FROM attachments a JOIN messages m ON m.id=a.message_id WHERE a.id=?"))
        .bind(&id).fetch_one(&state.db).await?;
    let space: Option<String> = row.get("space_id");
    let channel: String = row.get("channel_id");
    if scope == "*" {
        permissions::require_server_admin(&auth)?;
    } else {
        if space.as_deref() != Some(scope.as_str()) {
            return Err(AppError::NotFound(
                "attachment not found in this space".into(),
            ));
        }
        permissions::require_channel_permission(&state.db, &channel, &auth, "manage_messages")
            .await?;
    }
    let hash = match row.get::<Option<String>, _>("content_hash") {
        Some(hash) => hash,
        None => {
            crate::storage::hash_local_attachment(&state.storage_path, &row.get::<String, _>("url"))
                .await?
        }
    };
    let _guard = state.automod.processing.lock().await;
    let _admission = state.automod.admission.lock().await;
    store_block(
        &state,
        &scope,
        &hash,
        &body.reason,
        &auth.user_id,
        Some(&id),
    )
    .await?;
    Ok(Json(
        serde_json::json!({"data":{"content_hash":hash,"scope_id":scope}}),
    ))
}

pub async fn unblock_hash(
    State(state): State<AppState>,
    Path((scope, hash)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, true).await?;
    validate_hash(&hash)?;
    let _guard = state.automod.processing.lock().await;
    sqlx::query(&db::q(
        "DELETE FROM automod_hashes WHERE scope_id=? AND hash=?",
    ))
    .bind(&scope)
    .bind(&hash)
    .execute(&state.db)
    .await?;
    super::record_event(
        &state,
        None,
        &scope,
        Some(&auth.user_id),
        "hash_unblocked",
        serde_json::json!({"hash":hash}),
    )
    .await?;
    Ok(Json(serde_json::json!({"data":null})))
}
pub async fn list_hashes(
    State(state): State<AppState>,
    Path(scope): Path<String>,
    auth: AuthUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, true).await?;
    let rows = sqlx::query(&db::q("SELECT hash,reason,added_by,created_at FROM automod_hashes WHERE scope_id=? AND (CAST(? AS TEXT) IS NULL OR hash<?) ORDER BY hash DESC LIMIT 100")).bind(&scope).bind(&query.before).bind(&query.before).fetch_all(&state.db).await?;
    Ok(Json(
        serde_json::json!({"data":rows.iter().map(|r|serde_json::json!({"hash":r.get::<String,_>("hash"),"reason":r.get::<String,_>("reason"),"added_by":r.get::<Option<String>,_>("added_by"),"created_at":r.get::<Option<i64>,_>("created_at")})).collect::<Vec<_>>()}),
    ))
}
pub async fn events(
    State(state): State<AppState>,
    Path(scope): Path<String>,
    auth: AuthUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    authorize(&state, &scope, &auth, false).await?;
    let rows = sqlx::query(&db::q("SELECT * FROM automod_events WHERE (?='*' OR scope_id=?) AND (CAST(? AS TEXT) IS NULL OR id<?) ORDER BY id DESC LIMIT 100")).bind(&scope).bind(&scope).bind(&query.before).bind(&query.before).fetch_all(&state.db).await?;
    Ok(Json(
        serde_json::json!({"data":rows.iter().map(|r|serde_json::json!({"id":r.get::<String,_>("id"),"upload_id":r.get::<Option<String>,_>("upload_id"),"actor_id":r.get::<Option<String>,_>("actor_id"),"action":r.get::<String,_>("action"),"details":r.get::<String,_>("details"),"created_at":r.get::<i64,_>("created_at")})).collect::<Vec<_>>()}),
    ))
}
