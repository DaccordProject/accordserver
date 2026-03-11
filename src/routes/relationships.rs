use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::db;
use crate::error::AppError;
use crate::gateway::events::GatewayBroadcast;
use crate::middleware::auth::AuthUser;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PutRelationshipBody {
    #[serde(rename = "type")]
    pub rel_type: i64,
}

/// Build the JSON representation of a relationship from the current user's perspective.
fn rel_json(rel: &db::relationships::RelationshipRow) -> serde_json::Value {
    let display = rel
        .target_display_name
        .clone()
        .unwrap_or_else(|| rel.target_username.clone());
    serde_json::json!({
        "id": rel.target_user_id,
        "user": {
            "id": rel.target_user_id,
            "username": rel.target_username,
            "display_name": display,
            "avatar": rel.target_avatar
        },
        "type": rel.rel_type,
        "since": rel.created_at
    })
}

/// GET /users/@me/relationships
pub async fn list_relationships(
    state: State<AppState>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let rels = db::relationships::list_relationships(&state.db, &auth.user_id).await?;
    let data: Vec<serde_json::Value> = rels.iter().map(rel_json).collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

/// PUT /users/@me/relationships/{user_id}
/// type=1 → send friend request or accept incoming request
/// type=2 → block user
pub async fn put_relationship(
    state: State<AppState>,
    Path(target_id): Path<String>,
    auth: AuthUser,
    Json(body): Json<PutRelationshipBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    if target_id == auth.user_id {
        return Err(AppError::BadRequest(
            "cannot create a relationship with yourself".into(),
        ));
    }

    // Ensure target user exists
    db::users::get_user(&state.db, &target_id).await?;

    match body.rel_type {
        1 => handle_friend_or_accept(&state, &auth.user_id, &target_id).await,
        2 => handle_block(&state, &auth.user_id, &target_id).await,
        _ => Err(AppError::BadRequest(
            "type must be 1 (friend) or 2 (block)".into(),
        )),
    }
}

/// DELETE /users/@me/relationships/{user_id}
/// Removes a friendship, declines/cancels a pending request, or unblocks.
pub async fn delete_relationship(
    state: State<AppState>,
    Path(target_id): Path<String>,
    auth: AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    // Check what relationship exists
    let existing =
        db::relationships::get_relationship(&state.db, &auth.user_id, &target_id).await?;

    if existing.is_none() {
        return Err(AppError::NotFound("relationship not found".into()));
    }

    // Delete both direction rows (covers friends, pending, and the outgoing side of a block)
    db::relationships::delete_both_directions(&state.db, &auth.user_id, &target_id).await?;

    // Broadcast relationship_remove to both users
    if let Some(ref gtx) = *state.gateway_tx.read().await {
        let event_me = serde_json::json!({
            "op": 0,
            "type": "relationship.remove",
            "data": { "user_id": target_id }
        });
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(vec![auth.user_id.clone()]),
            event: event_me,
            intent: "relationships".to_string(),
        });

        let event_target = serde_json::json!({
            "op": 0,
            "type": "relationship.remove",
            "data": { "user_id": auth.user_id }
        });
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(vec![target_id.clone()]),
            event: event_target,
            intent: "relationships".to_string(),
        });
    }

    Ok(Json(serde_json::json!({})))
}

/// Send a friend request or accept an existing incoming request.
async fn handle_friend_or_accept(
    state: &AppState,
    user_id: &str,
    target_id: &str,
) -> Result<Json<serde_json::Value>, AppError> {
    // Check if target blocked us
    if db::relationships::is_blocked_by(&state.db, target_id, user_id).await? {
        return Err(AppError::Forbidden(
            "you cannot send a friend request to this user".into(),
        ));
    }

    // Check if there's already an incoming pending from the target (type=3 on my side)
    let my_existing = db::relationships::get_relationship(&state.db, user_id, target_id).await?;

    // Check if target already sent me a request (their type=4, my type=3)
    let target_existing =
        db::relationships::get_relationship(&state.db, target_id, user_id).await?;

    let is_accepting = my_existing.as_ref().is_some_and(|r| r.rel_type == 3)
        || target_existing.as_ref().is_some_and(|r| r.rel_type == 4);

    if is_accepting {
        // Accept: set both rows to type=1 (friend)
        db::relationships::upsert_relationship(&state.db, user_id, target_id, 1).await?;
        db::relationships::upsert_relationship(&state.db, target_id, user_id, 1).await?;

        // Broadcast relationship.update (type=1) to both users
        broadcast_relationship_event(state, user_id, target_id, "relationship.update", 1).await;
        broadcast_relationship_event(state, target_id, user_id, "relationship.update", 1).await;
    } else if my_existing.as_ref().is_some_and(|r| r.rel_type == 1) {
        // Already friends — no-op
    } else {
        // New request: outgoing for us (type=4), incoming for target (type=3)
        db::relationships::upsert_relationship(&state.db, user_id, target_id, 4).await?;
        db::relationships::upsert_relationship(&state.db, target_id, user_id, 3).await?;

        // Broadcast relationship.add to both parties
        broadcast_relationship_event(state, user_id, target_id, "relationship.add", 4).await;
        broadcast_relationship_event(state, target_id, user_id, "relationship.add", 3).await;
    }

    // Return the relationship from our perspective
    let rel = db::relationships::get_relationship(&state.db, user_id, target_id)
        .await?
        .ok_or_else(|| AppError::Internal("relationship missing after upsert".into()))?;

    Ok(Json(serde_json::json!({ "data": rel_json(&rel) })))
}

/// Block a user.
async fn handle_block(
    state: &AppState,
    user_id: &str,
    target_id: &str,
) -> Result<Json<serde_json::Value>, AppError> {
    // Set our side to blocked (type=2)
    db::relationships::upsert_relationship(&state.db, user_id, target_id, 2).await?;

    // Remove the target's relationship to us (they should no longer see a pending/friend)
    let target_had_rel =
        db::relationships::delete_relationship(&state.db, target_id, user_id).await?;

    // Broadcast relationship.add (blocked) to blocker
    broadcast_relationship_event(state, user_id, target_id, "relationship.add", 2).await;

    // If target had any relationship with us, notify them it's gone
    if target_had_rel {
        if let Some(ref gtx) = *state.gateway_tx.read().await {
            let event = serde_json::json!({
                "op": 0,
                "type": "relationship.remove",
                "data": { "user_id": user_id }
            });
            let _ = gtx.send(GatewayBroadcast {
                space_id: None,
                target_user_ids: Some(vec![target_id.to_string()]),
                event,
                intent: "relationships".to_string(),
            });
        }
    }

    let rel = db::relationships::get_relationship(&state.db, user_id, target_id)
        .await?
        .ok_or_else(|| AppError::Internal("relationship missing after block".into()))?;

    Ok(Json(serde_json::json!({ "data": rel_json(&rel) })))
}

/// Broadcast a relationship gateway event to a specific user, including the target user's info.
async fn broadcast_relationship_event(
    state: &AppState,
    recipient_id: &str,
    about_user_id: &str,
    event_type: &str,
    rel_type: i64,
) {
    if let Some(ref gtx) = *state.gateway_tx.read().await {
        // Fetch the target user info for the payload
        let user_json = if let Ok(u) = db::users::get_user(&state.db, about_user_id).await {
            let display = u.display_name.unwrap_or_else(|| u.username.clone());
            serde_json::json!({
                "id": u.id,
                "username": u.username,
                "display_name": display,
                "avatar": u.avatar
            })
        } else {
            serde_json::json!({ "id": about_user_id })
        };

        let event = serde_json::json!({
            "op": 0,
            "type": event_type,
            "data": {
                "id": about_user_id,
                "user": user_json,
                "type": rel_type
            }
        });
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(vec![recipient_id.to_string()]),
            event,
            intent: "relationships".to_string(),
        });
    }
}
