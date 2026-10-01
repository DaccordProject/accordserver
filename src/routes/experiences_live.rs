use crate::{
    db,
    error::AppError,
    middleware::{
        auth::{create_token_hash, resolve_hash},
        permissions::require_membership,
    },
    state::{AppState, RateLimitBucket},
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::Response,
};
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::json;
use tokio::time::{Duration, Instant};

pub async fn upgrade(
    State(state): State<AppState>,
    Path((space, id)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.max_message_size(2048)
        .max_frame_size(2048)
        .on_upgrade(move |socket| run(socket, state, space, id))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identify {
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    sequence: u64,
    kind: String,
    a: i32,
}

async fn run(mut socket: WebSocket, state: AppState, space: String, id: String) {
    let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Ok(identify) = serde_json::from_str::<Identify>(&text) else {
        return;
    };
    let Some(token) = identify.token.strip_prefix("Bearer ") else {
        return;
    };
    let hash = create_token_hash(token);
    drop(identify);
    drop(text);
    let Some(mut auth) = resolve_hash(&state.db, &hash, false, false).await else {
        return;
    };
    if auth.is_guest || auth.is_bot {
        return;
    }
    let Ok(mut session) = super::experiences::checked_session(&state, &space, &id, &auth).await
    else {
        return;
    };
    if session.mode != "real_time"
        || session.state != "running"
        || !session
            .participants
            .iter()
            .any(|p| p.user_id == auth.user_id)
    {
        return;
    }
    let rate_key = format!("experience:{space}:{id}:{}", auth.user_id);
    let mut last_sequence = 0;
    let mut pending_input = None;
    let mut trust_at = Instant::now();
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            frame=socket.recv() => {
                let Some(Ok(Message::Text(text)))=frame else {break;};
                let Ok(input)=serde_json::from_str::<Input>(&text) else {break;};
                if input.kind!="input" || input.sequence<=last_sequence || input.a<0 || input.a>864 {break;}
                last_sequence=input.sequence;
                {
                    let mut bucket=state.rate_limits.entry(rate_key.clone()).or_insert(RateLimitBucket{remaining:30,last_refill:Instant::now()});
                    if bucket.last_refill.elapsed()>=Duration::from_secs(1) {bucket.remaining=30;bucket.last_refill=Instant::now();}
                    if bucket.remaining==0 {break;}bucket.remaining-=1;
                }
                // Retain only the newest target. A racing tick must not lose
                // the final position of a drag when no later input is sent.
                pending_input=Some(input.a);
                match apply_input(&state,&space,&id,&auth.user_id,input.a).await {
                    Ok(()) => pending_input=None,
                    Err(AppError::SessionChanged) => {},
                    Err(_) => break,
                }
            },
            _=interval.tick() => {
                if trust_at.elapsed()>=Duration::from_secs(5) {
                    let Some(renewed)=resolve_hash(&state.db,&hash,false,false).await else {break;};auth=renewed;
                    if require_membership(&state.db,&space,&auth.user_id).await.is_err() {break;}
                    let Ok(current)=super::experiences::checked_session(&state,&space,&id,&auth).await else {break;};
                    session=current;trust_at=Instant::now();
                } else {
                    let Ok(current)=db::experiences::load(&state.db,&space,&id).await else {break;};session=current;
                }
                if session.state!="running" || !session.participants.iter().any(|p|p.user_id==auth.user_id) {break;}
                if let Some(target)=pending_input {
                    match apply_input(&state,&space,&id,&auth.user_id,target).await {
                        Ok(()) => {
                            pending_input=None;
                            let Ok(current)=db::experiences::load(&state.db,&space,&id).await else {break;};session=current;
                        },
                        Err(AppError::SessionChanged) => continue,
                        Err(_) => break,
                    }
                }
                let now=chrono::Utc::now().timestamp();
                if let Some(player)=session.participants.iter_mut().find(|p|p.user_id==auth.user_id && p.role=="player") {player.last_seen=now;}
                let players:Vec<_>=session.participants.iter().filter(|p|p.role=="player").collect();
                let disconnected=players.iter().find(|p|now-p.last_seen>5);
                let abandoned=players.iter().find(|p|now-p.last_seen>60);
                if let Some(player)=abandoned {let winner=players.iter().find(|p|p.user_id!=player.user_id).map(|p|p.user_id.clone());session.end("disconnect_forfeit",winner);}
                else {session.game["paused"]=json!(disconnected.is_some());super::experiences_rules::pong_tick(&mut session,chrono::Utc::now().timestamp_millis());}
                if db::experiences::save(&state.db,&mut session).await.is_err() {continue;}
                let frame=json!({"data":session}).to_string();
                if !matches!(tokio::time::timeout(Duration::from_secs(1),socket.send(Message::Text(frame.into()))).await, Ok(Ok(()))) {break;}
            }
        }
    }
    let _ = socket.close().await;
    // Keep participant slots for a 60s reconnect window. The remaining live
    // connection pauses after 5s and records a forfeit after 60s.
}

/// Apply a sequenced paddle target to a fresh server-owned snapshot. A failed
/// compare-and-swap writes nothing; the caller retains the latest target.
async fn apply_input(
    state: &AppState,
    space: &str,
    id: &str,
    user_id: &str,
    target: i32,
) -> Result<(), AppError> {
    let mut current = db::experiences::load(&state.db, space, id).await?;
    if current.state != "running" {
        return Err(AppError::Conflict("Session ended".into()));
    }
    let player = current
        .participants
        .iter_mut()
        .find(|p| p.user_id == user_id && p.role == "player")
        .ok_or_else(|| AppError::Forbidden("Player membership required".into()))?;
    let slot = player.slot.unwrap();
    player.last_seen = chrono::Utc::now().timestamp();
    super::experiences_rules::pong_input(&mut current, slot, target)?;
    db::experiences::save(&state.db, &mut current).await
}
