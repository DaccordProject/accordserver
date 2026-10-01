use crate::{
    e2ee::Identity,
    error::AppError,
    middleware::{auth::AuthUser, permissions::require_channel_membership},
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};

/// Immutable account identity. A second device must import the existing secret.
pub async fn register(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(key): Json<Identity>,
) -> Result<Json<serde_json::Value>, AppError> {
    key.validate()?;
    if let Some(existing) = crate::e2ee::identity(&state.db, &auth.user_id).await? {
        if existing != key {
            return Err(AppError::Conflict("This account already has an encryption identity. Import its encrypted backup on this device.".into()));
        }
    }
    let wire = state
        .federation
        .as_ref()
        .map(|fed| crate::federation::mapping::qualify(&auth.user_id, &fed.domain))
        .unwrap_or_else(|| auth.user_id.clone());
    crate::federation::e2ee::cache_identity(&state, &auth.user_id, &wire, &key).await?;
    Ok(Json(serde_json::json!({"data":key})))
}

pub async fn participants(
    State(state): State<AppState>,
    Path(channel_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    require_channel_membership(&state.db, &channel_id, &auth.user_id).await?;
    let response = crate::federation::e2ee::discover(&state, &channel_id, &auth.user_id).await?;
    Ok(Json(serde_json::json!({"data":response})))
}
