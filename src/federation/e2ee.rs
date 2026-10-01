//! Authenticated identity discovery for federated private chats.
use super::{authority, mapping, sender};
use crate::{
    db,
    e2ee::{self, Identity},
    error::AppError,
    state::AppState,
};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub const IDENTITY_PATH: &str = "/federation/v1/encryption/identity";
pub const CHAT_PATH: &str = "/federation/v1/encryption/chat";
#[derive(Deserialize)]
struct IdentityRequest {
    user_id: String,
    nonce: String,
}
#[derive(Deserialize)]
struct ChatRequest {
    actor_id: String,
    channel_id: String,
    nonce: String,
}

pub async fn handle_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (domain, _, request): (_, _, IdentityRequest) =
        match super::verify::prepare(&state, &headers, IDENTITY_PATH, &body).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let _ = &request.nonce;
    let result = async {
        authority::require_homed_on(&request.user_id, &domain, "identity")?;
        let id = mapping::participant_storage_id(&request.user_id, Some(&domain));
        Ok::<_, AppError>(
            json!({"user_id":request.user_id,"identity":e2ee::identity(&state.db,&id).await?}),
        )
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Keys are immutable here too: a remote home cannot silently rotate a cached identity.
pub async fn refresh_remote(state: &AppState, user_id: &str) -> Result<(), AppError> {
    let Some((_, domain)) = user_id.rsplit_once('@') else {
        return Ok(());
    };
    let body =
        serde_json::to_vec(&json!({"user_id":user_id,"nonce":uuid::Uuid::new_v4().to_string()}))
            .unwrap();
    let (status, bytes) = sender::request_signed(state, domain, IDENTITY_PATH, &body).await?;
    if !status.is_success() {
        return Err(AppError::BadRequest(
            "Participant server does not support private encryption".into(),
        ));
    }
    let response: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::BadRequest("Invalid remote identity response".into()))?;
    if response["user_id"] != user_id {
        return Err(AppError::BadRequest(
            "Remote identity context mismatch".into(),
        ));
    }
    if response["identity"].is_null() {
        return Ok(());
    }
    let identity: Identity = serde_json::from_value(response["identity"].clone())
        .map_err(|_| AppError::BadRequest("Invalid remote identity".into()))?;
    cache_identity(state, user_id, user_id, &identity).await
}

pub async fn cache_identity(
    state: &AppState,
    user_id: &str,
    wire_id: &str,
    key: &Identity,
) -> Result<(), AppError> {
    key.validate()?;
    sqlx::query(&db::q("INSERT INTO e2ee_identities(user_id,exchange_key,signing_key,user_context) VALUES (?,?,?,?) ON CONFLICT(user_id) DO NOTHING"))
        .bind(user_id).bind(&key.exchange_key).bind(&key.signing_key).bind(wire_id).execute(&state.db).await?;
    if e2ee::identity(&state.db, user_id).await?.as_ref() != Some(key)
        || e2ee::user_context(&state.db, user_id).await? != wire_id
    {
        return Err(AppError::Conflict(
            "Remote encryption identity changed".into(),
        ));
    }
    Ok(())
}

pub async fn discover(
    state: &AppState,
    channel_id: &str,
    actor_id: &str,
) -> Result<Value, AppError> {
    let channel = db::channels::get_channel_row(&state.db, channel_id).await?;
    if !matches!(channel.channel_type.as_str(), "dm" | "group_dm") {
        return Err(AppError::BadRequest(
            "Encryption is only available in private chats".into(),
        ));
    }
    if !db::dm_participants::is_participant(&state.db, channel_id, actor_id).await? {
        return Err(AppError::Forbidden("Not a private chat participant".into()));
    }
    if let Some(home) = db::federation::channel_origin(&state.db, channel_id).await? {
        let fed = state
            .federation
            .as_ref()
            .ok_or_else(|| AppError::BadRequest("Federation is disabled".into()))?;
        let actor = mapping::qualify(actor_id, &fed.domain);
        let body=serde_json::to_vec(&json!({"actor_id":actor,"channel_id":channel_id,"nonce":uuid::Uuid::new_v4().to_string()})).unwrap();
        let (status, bytes) = sender::request_signed(state, &home, CHAT_PATH, &body).await?;
        if !status.is_success() {
            return Err(AppError::BadRequest(
                "Home server could not provide private encryption keys".into(),
            ));
        }
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::BadRequest("Invalid encryption discovery response".into()))?;
        if response["channel_id"] != channel_id || response["self_user_id"] != actor {
            return Err(AppError::BadRequest(
                "Encryption discovery context mismatch".into(),
            ));
        }
        let mut rows = response["participants"]
            .as_array()
            .ok_or_else(|| AppError::BadRequest("Invalid encryption discovery response".into()))?
            .clone();
        for row in &mut rows {
            let wire = row["wire_user_id"]
                .as_str()
                .ok_or_else(|| AppError::BadRequest("Invalid participant identity".into()))?
                .to_owned();
            let local = mapping::participant_storage_id(&wire, Some(&fed.domain));
            // A former author may not yet be cached on a replica; don't invent profile data.
            if db::users::get_user(&state.db, &local).await.is_ok() && !row["identity"].is_null() {
                let key: Identity = serde_json::from_value(row["identity"].clone())
                    .map_err(|_| AppError::BadRequest("Invalid identity".into()))?;
                cache_identity(state, &local, &wire, &key).await?;
            }
            row["user_id"] = json!(local);
        }
        remember_chat(state, channel_id, channel_id).await?;
        return Ok(
            json!({"channel_id":channel_id,"self_user_id":actor,"cdn_url":response["cdn_url"],"participants":rows}),
        );
    }
    let current = db::dm_participants::list_participant_ids(&state.db, channel_id).await?;
    let former: Vec<(String,)> = sqlx::query_as(&db::q(
        "SELECT DISTINCT author_id FROM messages WHERE channel_id=?",
    ))
    .bind(channel_id)
    .fetch_all(&state.db)
    .await?;
    let mut users = current.clone();
    for (id,) in former {
        if !users.contains(&id) {
            users.push(id);
        }
    }
    users.sort();
    let mut rows = Vec::new();
    for id in users {
        // Old history continues to use a pinned key when a former member's server is offline.
        if id.contains('@') && e2ee::identity(&state.db,&id).await?.is_none() {
            refresh_remote(state,&id).await?;
        }
        let wire = if id.contains('@') {
            id.clone()
        } else if let Some(fed) = &state.federation {
            mapping::qualify(&id, &fed.domain)
        } else {
            id.clone()
        };
        rows.push(json!({"user_id":id,"wire_user_id":wire,"current":current.contains(&id),"identity":e2ee::identity(&state.db,&id).await?}));
    }
    let context = state
        .federation
        .as_ref()
        .map(|fed| mapping::qualify(channel_id, &fed.domain))
        .unwrap_or_else(|| channel_id.into());
    remember_chat(state, channel_id, &context).await?;
    let self_id = state
        .federation
        .as_ref()
        .map(|fed| mapping::qualify(actor_id, &fed.domain))
        .unwrap_or_else(|| actor_id.into());
    Ok(
        json!({"channel_id":context,"self_user_id":self_id,"cdn_url":state.federation.as_ref().map(|fed|format!("{}/cdn",fed.public_url.trim_end_matches('/'))),"participants":rows}),
    )
}

async fn remember_chat(state: &AppState, channel_id: &str, context: &str) -> Result<(), AppError> {
    sqlx::query(&db::q("INSERT INTO e2ee_chat_contexts(channel_id,wire_id) VALUES (?,?) ON CONFLICT(channel_id) DO NOTHING"))
        .bind(channel_id).bind(context).execute(&state.db).await?;
    if e2ee::chat_context(&state.db, channel_id).await? != context {
        return Err(AppError::Conflict(
            "Private chat encryption context changed".into(),
        ));
    }
    Ok(())
}

pub async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (domain, peer, request): (_, _, ChatRequest) =
        match super::verify::prepare(&state, &headers, CHAT_PATH, &body).await {
            Ok(v) => v,
            Err(r) => return *r,
        };
    let _ = &request.nonce;
    let result = async {
        authority::require_homed_on(&request.actor_id, &peer.domain, "actor")?;
        authority::require_homed_on(&request.channel_id, &domain, "channel")?;
        let channel_id = mapping::participant_storage_id(&request.channel_id, Some(&domain));
        discover(&state, &channel_id, &request.actor_id).await
    }
    .await;
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}
