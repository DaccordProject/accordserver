use axum::extract::{Path, State};
use axum::Json;

use crate::error::AppError;
use crate::middleware::auth::AuthUser;
use crate::middleware::permissions::{require_channel_membership, require_channel_permission};
use crate::state::AppState;

pub async fn add_reaction(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "add_reactions").await?;
    sqlx::query(
        "INSERT OR IGNORE INTO reactions (message_id, user_id, emoji_name) VALUES (?, ?, ?)",
    )
    .bind(&message_id)
    .bind(&auth.user_id)
    .bind(&emoji)
    .execute(&state.db)
    .await
    .map_err(crate::error::AppError::from)?;

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_own_reaction(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    sqlx::query("DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji_name = ?")
        .bind(&message_id)
        .bind(&auth.user_id)
        .bind(&emoji)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_user_reaction(
    state: State<AppState>,
    Path((channel_id, message_id, emoji, user_id)): Path<(String, String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    sqlx::query("DELETE FROM reactions WHERE message_id = ? AND user_id = ? AND emoji_name = ?")
        .bind(&message_id)
        .bind(&user_id)
        .bind(&emoji)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn list_reactions(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let users = sqlx::query_as::<_, (String,)>(
        "SELECT user_id FROM reactions WHERE message_id = ? AND emoji_name = ?",
    )
    .bind(&message_id)
    .bind(&emoji)
    .fetch_all(&state.db)
    .await?;

    let user_ids: Vec<String> = users.into_iter().map(|r| r.0).collect();
    Ok(Json(serde_json::json!({ "data": user_ids })))
}

pub async fn remove_all_reactions(
    state: State<AppState>,
    Path((channel_id, message_id)): Path<(String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    sqlx::query("DELETE FROM reactions WHERE message_id = ?")
        .bind(&message_id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "data": null })))
}

pub async fn remove_all_reactions_emoji(
    state: State<AppState>,
    Path((channel_id, message_id, emoji)): Path<(String, String, String)>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_permission(&state.db, &channel_id, &auth, "manage_messages").await?;
    sqlx::query("DELETE FROM reactions WHERE message_id = ? AND emoji_name = ?")
        .bind(&message_id)
        .bind(&emoji)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "data": null })))
}
