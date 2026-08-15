pub mod dispatcher;
pub mod events;
pub mod heartbeat;
pub mod intents;
pub mod resume;
pub mod session;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashSet, VecDeque};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::db;
use crate::middleware::auth as auth_resolve;
use crate::routes;
use crate::state::AppState;
use events::{
    GatewayBroadcast, GatewayMessage, IdentifyData, PresenceUpdateData, ResumeData,
    VoiceStateUpdateData,
};
use heartbeat::{HEARTBEAT_INTERVAL, HEARTBEAT_TIMEOUT};
use resume::{Handover, ParkedSession};
use session::GatewaySession;

type WsSink = SplitSink<WebSocket, Message>;
type WsStream = SplitStream<WebSocket>;

/// How long a client has to send IDENTIFY or RESUME after HELLO.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Status values a client may ask for.
const VALID_STATUSES: [&str; 4] = ["online", "idle", "dnd", "invisible"];

pub async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// Outcome of the pre-authentication handshake.
enum Handshake {
    Identify {
        auth: ResolvedAuth,
        intents: Vec<String>,
        presence: Option<serde_json::Value>,
    },
    Resume {
        auth: ResolvedAuth,
        session_id: String,
        intents: Vec<String>,
        handover: Handover,
        missed: Vec<String>,
    },
}

async fn send_invalid_session(sink: &mut WsSink) {
    let close = serde_json::json!({
        "op": events::opcode::INVALID_SESSION,
        "data": { "resumable": false }
    });
    let _ = sink.send(Message::Text(close.to_string().into())).await;
}

async fn send_close(sink: &mut WsSink, code: u16, reason: &str) {
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// Wait for IDENTIFY or RESUME. Anything else is answered with INVALID_SESSION
/// straight away rather than left to time out — a client whose opening frame we
/// don't accept used to sit here for the full 30 seconds, long enough for the
/// disconnect to be visible to everyone in its spaces.
async fn await_handshake(
    state: &AppState,
    sink: &mut WsSink,
    stream: &mut WsStream,
) -> Option<Handshake> {
    let timeout = tokio::time::sleep(HANDSHAKE_TIMEOUT);
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                send_invalid_session(sink).await;
                return None;
            }
            msg = stream.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(text))) => text,
                    // A broken stream can keep yielding errors, so treat the
                    // first one as the end rather than spinning on it.
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                    // Ping/pong/binary aren't part of the handshake.
                    _ => continue,
                };

                let Ok(gw_msg) = serde_json::from_str::<GatewayMessage>(&text) else {
                    send_invalid_session(sink).await;
                    return None;
                };

                match gw_msg.op {
                    // HELLO hands out the heartbeat interval, so a client may
                    // well start beating before it finishes identifying.
                    op if op == events::opcode::HEARTBEAT => {
                        let ack = serde_json::json!({ "op": events::opcode::HEARTBEAT_ACK });
                        if sink.send(Message::Text(ack.to_string().into())).await.is_err() {
                            return None;
                        }
                    }
                    op if op == events::opcode::IDENTIFY => {
                        let Some(identify) = gw_msg
                            .data
                            .and_then(|d| serde_json::from_value::<IdentifyData>(d).ok())
                        else {
                            send_invalid_session(sink).await;
                            return None;
                        };
                        let Some(auth) = resolve_token(state, &identify.token).await else {
                            send_invalid_session(sink).await;
                            return None;
                        };
                        return Some(Handshake::Identify {
                            auth,
                            intents: identify.intents,
                            presence: identify.presence,
                        });
                    }
                    op if op == events::opcode::RESUME => {
                        let Some(resume_data) = gw_msg
                            .data
                            .and_then(|d| serde_json::from_value::<ResumeData>(d).ok())
                        else {
                            send_invalid_session(sink).await;
                            return None;
                        };
                        let Some(auth) = resolve_token(state, &resume_data.token).await else {
                            send_invalid_session(sink).await;
                            return None;
                        };
                        let claimed =
                            claim_parked_session(state, &resume_data.session_id, &auth.user_id)
                                .await;
                        let Some((intents, handover)) = claimed else {
                            send_invalid_session(sink).await;
                            return None;
                        };
                        let missed = resume::missed_since(
                            &handover.buffer,
                            handover.seq,
                            resume_data.seq,
                        );
                        let Some(missed) = missed else {
                            // The replay buffer no longer covers the gap, so the
                            // client has to rebuild its state from READY.
                            send_invalid_session(sink).await;
                            return None;
                        };
                        return Some(Handshake::Resume {
                            auth,
                            session_id: resume_data.session_id,
                            intents,
                            handover,
                            missed,
                        });
                    }
                    _ => {
                        send_invalid_session(sink).await;
                        return None;
                    }
                }
            }
        }
    }
}

/// Take a parked session out of the registry and ask the task still holding it
/// to hand over its buffer and broadcast receiver.
async fn claim_parked_session(
    state: &AppState,
    session_id: &str,
    user_id: &str,
) -> Option<(Vec<String>, Handover)> {
    // Ownership is checked before the entry is taken, so a caller holding
    // someone else's session id can't evict it. The read guard is dropped at the
    // end of this statement, before the `remove` below touches the same shard.
    let owned = state
        .resumable_sessions
        .get(session_id)
        .map(|entry| entry.user_id == user_id)
        .unwrap_or(false);
    if !owned {
        return None;
    }
    // Removing hands the session to exactly one socket.
    let (_, parked) = state.resumable_sessions.remove(session_id)?;

    let (reply_tx, reply_rx) = oneshot::channel();
    parked.claim_tx.send(reply_tx).await.ok()?;
    // The parked task replies immediately; the timeout only covers it having
    // already hit its own deadline and exited.
    let handover = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
        .await
        .ok()?
        .ok()?;
    Some((parked.intents, handover))
}

// ---------------------------------------------------------------------------
// Broadcast filtering
// ---------------------------------------------------------------------------

/// What a session should do with a broadcast it received.
enum Delivery {
    /// Not for this session, or suppressed by a mute or a missing intent.
    Skip,
    /// The session's mute list changed and has to be reloaded.
    RefreshMutes,
    /// This session's own space membership changed: reload the space set, then
    /// deliver the payload if it's `Some` (an intent can still gate the event
    /// itself — the reload is about routing and happens either way).
    RefreshSpaces(Option<serde_json::Value>),
    /// Deliver this event; the caller stamps the session's next `seq` into it.
    Send(serde_json::Value),
}

/// The user a `member.join` / `member.leave` event is about. The two carry the
/// subject differently: `member.join` embeds the whole user object, while
/// `member.leave` sends a bare id.
fn membership_event_subject(event: &serde_json::Value) -> Option<&str> {
    let data = event.get("data")?;
    data.get("user_id")
        .and_then(|u| u.as_str())
        .or_else(|| data.get("user")?.get("id")?.as_str())
}

fn classify_broadcast(
    broadcast: &GatewayBroadcast,
    user_id: &str,
    space_ids: &HashSet<String>,
    muted_channel_ids: &HashSet<String>,
    intents: &[String],
) -> Delivery {
    let event_type = broadcast
        .event
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    // Our own membership changing is checked *before* the space filter below.
    // On a join the space isn't in `space_ids` yet, so the filter would drop
    // the one event that says to add it — and the session would then stay deaf
    // to that space for its whole life.
    if matches!(event_type, "member.join" | "member.leave")
        && membership_event_subject(&broadcast.event) == Some(user_id)
    {
        return Delivery::RefreshSpaces(
            intents::has_intent(intents, event_type).then(|| broadcast.event.clone()),
        );
    }

    let should_receive = match (&broadcast.target_user_ids, &broadcast.space_id) {
        (Some(targets), _) => targets.iter().any(|t| t == user_id),
        (None, Some(sid)) => space_ids.contains(sid),
        (None, None) => true, // global event
    };
    if !should_receive {
        return Delivery::Skip;
    }

    // Mute list updates from the REST API
    if event_type == "channel_mute.create" || event_type == "channel_mute.delete" {
        return Delivery::RefreshMutes;
    }

    // Suppress message/typing events for muted channels
    if event_type.starts_with("message.") || event_type.starts_with("typing.") {
        let channel_id = broadcast
            .event
            .get("data")
            .and_then(|d| d.get("channel_id"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if !channel_id.is_empty() && muted_channel_ids.contains(channel_id) {
            return Delivery::Skip;
        }
    }

    if !intents::has_intent(intents, event_type) {
        return Delivery::Skip;
    }
    Delivery::Send(broadcast.event.clone())
}

/// Re-reads the user's space memberships mid-session.
///
/// Guests are scoped to a single space handed out with their token and have no
/// row in `members`, so re-reading would silently strip their access — they
/// keep [`current`] instead.
async fn reload_space_ids(
    state: &AppState,
    user_id: &str,
    is_guest_session: bool,
    current: &HashSet<String>,
) -> HashSet<String> {
    if is_guest_session {
        return current.clone();
    }
    db::spaces::list_space_ids_for_user(&state.db, user_id)
        .await
        .map(|sids| sids.into_iter().collect())
        .unwrap_or_else(|_| current.clone())
}

/// Mirrors a refreshed space set onto the session registered with the
/// dispatcher, so the two don't drift.
async fn sync_session_spaces(state: &AppState, session_id: &str, space_ids: &HashSet<String>) {
    if let Some(ref dispatcher) = *state.dispatcher.read().await {
        if let Some(mut session) = dispatcher.sessions().get_mut(session_id) {
            session.space_ids = space_ids.clone();
        }
    }
}

fn stamp_seq(mut event: serde_json::Value, seq: u64) -> String {
    if let Some(obj) = event.as_object_mut() {
        obj.insert("seq".to_string(), serde_json::json!(seq));
    }
    event.to_string()
}

// ---------------------------------------------------------------------------
// Presence helpers
// ---------------------------------------------------------------------------

/// Pull `{ status, activities | activity }` out of an IDENTIFY presence
/// payload. `None` for either half means the client expressed no preference, as
/// opposed to asking for an empty one.
fn parse_presence_payload(
    value: &serde_json::Value,
) -> (Option<String>, Option<Vec<serde_json::Value>>) {
    let status = value
        .get("status")
        .and_then(|s| s.as_str())
        .filter(|s| VALID_STATUSES.contains(s))
        .map(|s| s.to_string());
    let activities = match value.get("activities").and_then(|a| a.as_array()) {
        Some(list) => Some(list.clone()),
        None => match value.get("activity") {
            Some(a) if !a.is_null() => Some(vec![a.clone()]),
            _ => None,
        },
    };
    (status, activities)
}

/// Send a `presence.update` to every space the user is in, plus any friends who
/// share none of them. `invisible` is published as `offline`.
async fn broadcast_presence(
    state: &AppState,
    user_id: &str,
    space_ids: &HashSet<String>,
    friend_ids: &HashSet<String>,
    status: &str,
    activities: &[serde_json::Value],
) {
    let gateway_tx = state.gateway_tx.read().await;
    let Some(gtx) = gateway_tx.as_ref() else {
        return;
    };

    let visible = if status == "invisible" {
        "offline"
    } else {
        status
    };
    let client_status = if visible == "offline" {
        serde_json::json!({})
    } else {
        serde_json::json!({ "desktop": visible })
    };
    let data = serde_json::json!({
        "user_id": user_id,
        "status": visible,
        "client_status": client_status,
        "activities": activities
    });
    let event = || {
        serde_json::json!({
            "op": events::opcode::EVENT,
            "type": "presence.update",
            "data": data
        })
    };

    for sid in space_ids {
        let _ = gtx.send(GatewayBroadcast {
            space_id: Some(sid.clone()),
            target_user_ids: None,
            event: event(),
            intent: "presences".to_string(),
        });
    }
    // Friends who may not share any space still track each other's presence.
    if !friend_ids.is_empty() {
        let _ = gtx.send(GatewayBroadcast {
            space_id: None,
            target_user_ids: Some(friend_ids.iter().cloned().collect()),
            event: event(),
            intent: "presences".to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// READY / RESUMED
// ---------------------------------------------------------------------------

/// Build and send the READY payload — the client's full initial state.
/// Returns false when the socket is gone.
async fn send_ready(
    state: &AppState,
    sink: &mut WsSink,
    session_id: &str,
    user_id: &str,
    is_guest_session: bool,
    space_ids: &HashSet<String>,
    muted_channel_ids: &HashSet<String>,
) -> bool {
    let presences_json: Vec<serde_json::Value>;
    let relationships_json: Vec<serde_json::Value>;

    if is_guest_session {
        presences_json = vec![];
        relationships_json = vec![];
    } else {
        // Collect presences of online members in the user's spaces
        let mut all_member_ids = HashSet::new();
        for sid in space_ids {
            if let Ok(members) = db::spaces::list_member_ids_for_space(&state.db, sid).await {
                for mid in members {
                    all_member_ids.insert(mid);
                }
            }
        }
        presences_json = crate::presence::get_space_presences(state, &all_member_ids)
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or_default())
            .collect();

        relationships_json = db::relationships::list_relationships(&state.db, user_id)
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let display = r
                    .target_display_name
                    .clone()
                    .unwrap_or_else(|| r.target_username.clone());
                serde_json::json!({
                    "id": r.target_user_id,
                    "user": {
                        "id": r.target_user_id,
                        "username": r.target_username,
                        "display_name": display,
                        "avatar": r.target_avatar
                    },
                    "type": r.rel_type,
                    "since": r.created_at
                })
            })
            .collect();
    }

    // Fetch full initial state for the READY payload
    let current_user_json = if !is_guest_session {
        db::users::get_user(&state.db, user_id)
            .await
            .ok()
            .map(|u| serde_json::to_value(&u).unwrap_or_default())
    } else {
        None
    };

    let mut spaces_json: Vec<serde_json::Value> = Vec::new();
    let mut all_channels_json: Vec<serde_json::Value> = Vec::new();
    let mut all_members_json: Vec<serde_json::Value> = Vec::new();
    let mut all_roles_json: Vec<serde_json::Value> = Vec::new();
    let mut all_voice_states_json: Vec<serde_json::Value> = Vec::new();
    let mut all_users_json: Vec<serde_json::Value> = Vec::new();
    let mut seen_user_ids: HashSet<String> = HashSet::new();

    for sid in space_ids {
        // Space
        if let Ok(space_row) = db::spaces::get_space_row(&state.db, sid).await {
            spaces_json.push(serde_json::to_value(&space_row).unwrap_or_default());
        }

        // Channels (with permission overwrites)
        if let Ok(channel_rows) = db::channels::list_channels_in_space(&state.db, sid).await {
            if let Ok(channels) =
                routes::spaces::channels_to_json_async(&state.db, &channel_rows).await
            {
                all_channels_json.extend(channels);
            }
        }

        // Roles
        if let Ok(role_rows) = db::roles::list_roles(&state.db, sid).await {
            let roles: Vec<serde_json::Value> = role_rows
                .iter()
                .map(routes::roles::role_row_to_json)
                .collect();
            all_roles_json.extend(roles);
        }

        // Members (all pages, with embedded user objects)
        let mut after: Option<String> = None;
        loop {
            let rows = match db::members::list_members(&state.db, sid, after.as_deref(), 1000).await
            {
                Ok(r) => r,
                Err(_) => break,
            };
            let has_more = rows.len() > 1000;
            let page: Vec<_> = if has_more {
                rows[..1000].to_vec()
            } else {
                rows.clone()
            };

            for member_row in &page {
                let role_ids =
                    db::members::get_member_role_ids(&state.db, sid, &member_row.user_id)
                        .await
                        .unwrap_or_default();
                let member_json = routes::members::member_row_to_json(member_row, &role_ids);
                all_members_json.push(member_json);

                // Collect unique user objects
                if !seen_user_ids.contains(&member_row.user_id) {
                    if let Ok(user) = db::users::get_user(&state.db, &member_row.user_id).await {
                        all_users_json.push(serde_json::to_value(&user).unwrap_or_default());
                        seen_user_ids.insert(member_row.user_id.clone());
                    }
                }
            }

            if has_more {
                after = page.last().map(|m| m.user_id.clone());
            } else {
                break;
            }
        }

        // Voice states for this space
        let voice_states = crate::voice::state::get_space_voice_states(state, sid);
        for vs in &voice_states {
            all_voice_states_json.push(serde_json::to_value(vs).unwrap_or_default());
        }
    }

    // DM channels (with recipients)
    let dm_channels_json: Vec<serde_json::Value> = if !is_guest_session {
        match db::users::get_user_dm_channels(&state.db, user_id).await {
            Ok(dm_rows) => {
                let mut dms = Vec::new();
                for row in &dm_rows {
                    dms.push(routes::spaces::channel_row_to_json_pub(&state.db, row).await);
                }
                dms
            }
            Err(_) => vec![],
        }
    } else {
        vec![]
    };

    // Muted channel IDs (already loaded as muted_channel_ids HashSet)
    let mutes_json: Vec<serde_json::Value> = muted_channel_ids
        .iter()
        .map(|cid| serde_json::json!({ "channel_id": cid }))
        .collect();

    // Unread states
    let unread_json: Vec<serde_json::Value> = if !is_guest_session {
        db::read_states::get_unread_channels(&state.db, user_id)
            .await
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };

    // Send READY event
    let motd = state.settings.load().motd.clone();
    let ready = serde_json::json!({
        "op": events::opcode::EVENT,
        "seq": 1,
        "type": "ready",
        "data": {
            "session_id": session_id,
            "user_id": user_id,
            "user": current_user_json,
            "spaces": spaces_json,
            "channels": all_channels_json,
            "members": all_members_json,
            "roles": all_roles_json,
            "users": all_users_json,
            "voice_states": all_voice_states_json,
            "dm_channels": dm_channels_json,
            "mutes": mutes_json,
            "unread": unread_json,
            "presences": presences_json,
            "relationships": relationships_json,
            "is_guest": is_guest_session,
            "api_version": "v1",
            "server_version": env!("CARGO_PKG_VERSION"),
            "motd": motd
        }
    });
    sink.send(Message::Text(ready.to_string().into()))
        .await
        .is_ok()
}

/// Replay the events the client missed while disconnected, then confirm the
/// resume. Returns false when the socket is gone.
async fn send_resumed(sink: &mut WsSink, session_id: &str, seq: u64, missed: &[String]) -> bool {
    for payload in missed {
        if sink
            .send(Message::Text(payload.clone().into()))
            .await
            .is_err()
        {
            return false;
        }
    }
    let resumed = serde_json::json!({
        "op": events::opcode::EVENT,
        "seq": seq,
        "type": "resumed",
        "data": {
            "session_id": session_id,
            "seq": seq,
            "replayed": missed.len()
        }
    });
    sink.send(Message::Text(resumed.to_string().into()))
        .await
        .is_ok()
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // Send HELLO
    let hello = serde_json::json!({
        "op": events::opcode::HELLO,
        "data": {
            "heartbeat_interval": HEARTBEAT_INTERVAL.as_millis() as u64
        }
    });
    if ws_sink
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Channel for sending messages to this client
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let Some(handshake) = await_handshake(&state, &mut ws_sink, &mut ws_stream).await else {
        return;
    };

    let auth;
    let session_id;
    let user_intents: Vec<String>;
    let identify_presence: Option<serde_json::Value>;
    let is_resume;
    let missed_events: Vec<String>;
    // A resumed session carries its sequence, replay buffer and broadcast
    // receiver forward: the receiver in particular, so nothing dispatched
    // between the two sockets is lost or delivered twice.
    let mut seq: u64;
    let mut replay_buffer: VecDeque<(u64, String)>;
    let mut broadcast_rx: Option<broadcast::Receiver<GatewayBroadcast>>;

    match handshake {
        Handshake::Identify {
            auth: resolved,
            intents,
            presence,
        } => {
            auth = resolved;
            session_id = crate::snowflake::generate();
            user_intents = intents;
            identify_presence = presence;
            is_resume = false;
            missed_events = Vec::new();
            seq = 1;
            replay_buffer = VecDeque::new();
            broadcast_rx = None;
        }
        Handshake::Resume {
            auth: resolved,
            session_id: prior_id,
            intents,
            handover,
            missed,
        } => {
            auth = resolved;
            session_id = prior_id;
            user_intents = intents;
            identify_presence = None;
            is_resume = true;
            missed_events = missed;
            seq = handover.seq;
            replay_buffer = handover.buffer;
            broadcast_rx = handover.broadcast_rx;
        }
    }

    let user_id = auth.user_id;
    let is_bot = auth.is_bot;
    let is_admin = auth.is_admin;

    // Memberships and mutes are reloaded on RESUME too — they can change while
    // a client is away — and `space_ids` is additionally refreshed mid-session
    // whenever this user joins or leaves a space (see Delivery::RefreshSpaces).
    let mut space_ids: HashSet<String>;
    let mut muted_channel_ids: HashSet<String>;
    if auth.is_guest {
        // Guest: use scoped space only, no mutes
        space_ids = auth.guest_space_id.into_iter().collect();
        muted_channel_ids = HashSet::new();
    } else {
        space_ids = db::spaces::list_space_ids_for_user(&state.db, &user_id)
            .await
            .map(|sids| sids.into_iter().collect())
            .unwrap_or_default();

        muted_channel_ids = db::mutes::list_effective_muted_channel_ids(&state.db, &user_id)
            .await
            .map(|ids| ids.into_iter().collect())
            .unwrap_or_default();
    }

    // Guest sessions: track in-memory, skip presence/relationships
    let is_guest_session = user_id.starts_with("guest:");

    // Friend set for presence routing.
    let friend_ids: HashSet<String> = if is_guest_session {
        HashSet::new()
    } else {
        db::relationships::get_friend_ids(&state.db, &user_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect()
    };

    // Resolve the presence this connection publishes, before READY is assembled
    // so the user sees their own presence in it.
    let mut announce_presence: Option<(String, Vec<serde_json::Value>)> = None;
    if !is_guest_session {
        if is_resume {
            // Presence is held across the resume window, so a successful resume
            // normally has nothing to announce — that missing flap is the whole
            // point. Re-publish only if it did lapse.
            if crate::presence::get_user_presence(&state, &user_id).is_none() {
                crate::presence::set_presence(&state, &user_id, "online", vec![]);
                announce_presence = Some(("online".to_string(), vec![]));
            }
        } else {
            // Honour what IDENTIFY asks for; failing that, carry forward what
            // the user's other sessions already published, so reconnecting no
            // longer resets a chosen dnd/idle or a custom status. "online" is
            // the fallback only when there is nothing to preserve.
            let (wanted_status, wanted_activities) = identify_presence
                .as_ref()
                .map(parse_presence_payload)
                .unwrap_or((None, None));
            let prior = crate::presence::get_user_presence(&state, &user_id);
            let status = wanted_status
                .or_else(|| prior.as_ref().map(|p| p.status.clone()))
                .unwrap_or_else(|| "online".to_string());
            let activities = wanted_activities
                .or_else(|| prior.map(|p| p.activities))
                .unwrap_or_default();
            crate::presence::set_presence(&state, &user_id, &status, activities.clone());
            announce_presence = Some((status, activities));
        }
    }

    // True unless the client hung up deliberately or we closed on it: a socket
    // that merely dropped gets parked for RESUME instead of going offline.
    let mut resumable = true;

    // Presence is registered from here on, so failures must fall through to the
    // cleanup below rather than return.
    'session: {
        let sent = if is_resume {
            send_resumed(&mut ws_sink, &session_id, seq, &missed_events).await
        } else {
            send_ready(
                &state,
                &mut ws_sink,
                &session_id,
                &user_id,
                is_guest_session,
                &space_ids,
                &muted_channel_ids,
            )
            .await
        };
        if !sent {
            resumable = false;
            break 'session;
        }

        // Register session with dispatcher
        let session = GatewaySession {
            session_id: session_id.clone(),
            user_id: user_id.clone(),
            intents: user_intents.clone(),
            space_ids: space_ids.clone(),
            sequence: seq,
            tx: tx.clone(),
        };

        if let Some(ref dispatcher) = *state.dispatcher.read().await {
            dispatcher.register_session(session);
        }

        // Guest connect: broadcast anonymous_count_updated
        if is_guest_session {
            if let Some(ref gtx) = *state.gateway_tx.read().await {
                for sid in &space_ids {
                    let count = state.guest_counts.get(sid).map(|c| *c).unwrap_or(0);
                    let event = serde_json::json!({
                        "op": events::opcode::EVENT,
                        "type": "anonymous_count_updated",
                        "data": { "count": count, "space_id": sid }
                    });
                    let _ = gtx.send(GatewayBroadcast {
                        space_id: Some(sid.clone()),
                        target_user_ids: None,
                        event,
                        intent: "members".to_string(),
                    });
                }
            }
        }

        if let Some((ref status, ref activities)) = announce_presence {
            broadcast_presence(
                &state,
                &user_id,
                &space_ids,
                &friend_ids,
                status,
                activities,
            )
            .await;
        }

        // Subscribe to broadcasts. A resumed session already carries the
        // receiver its predecessor was reading from.
        if broadcast_rx.is_none() {
            broadcast_rx = (*state.dispatcher.read().await)
                .as_ref()
                .map(|dispatcher| dispatcher.subscribe());
        }

        let mut last_heartbeat = tokio::time::Instant::now();
        let mut heartbeat_interval = tokio::time::interval(HEARTBEAT_INTERVAL);

        // Per-connection rate limit: max 120 messages per 60 seconds
        const WS_RATE_LIMIT: u32 = 120;
        const WS_RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
        let mut ws_msg_count: u32 = 0;
        let mut ws_rate_window_start = tokio::time::Instant::now();

        loop {
            tokio::select! {
                // Outgoing messages from the session channel
                Some(msg) = rx.recv() => {
                    if ws_sink.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                // Broadcast events
                broadcast = async {
                    if let Some(ref mut rx) = broadcast_rx {
                        rx.recv().await.ok()
                    } else {
                        std::future::pending::<Option<GatewayBroadcast>>().await
                    }
                } => {
                    if let Some(broadcast) = broadcast {
                        match classify_broadcast(&broadcast, &user_id, &space_ids, &muted_channel_ids, &user_intents) {
                            Delivery::Skip => {}
                            Delivery::RefreshMutes => {
                                muted_channel_ids = db::mutes::list_effective_muted_channel_ids(&state.db, &user_id).await
                                    .map(|ids| ids.into_iter().collect())
                                    .unwrap_or_default();
                            }
                            Delivery::RefreshSpaces(event) => {
                                space_ids = reload_space_ids(&state, &user_id, is_guest_session, &space_ids).await;
                                sync_session_spaces(&state, &session_id, &space_ids).await;
                                if let Some(event) = event {
                                    seq += 1;
                                    let payload = stamp_seq(event, seq);
                                    if ws_sink.send(Message::Text(payload.clone().into())).await.is_err() {
                                        break;
                                    }
                                    resume::push(&mut replay_buffer, seq, payload);
                                }
                            }
                            Delivery::Send(event) => {
                                seq += 1;
                                let payload = stamp_seq(event, seq);
                                if ws_sink.send(Message::Text(payload.clone().into())).await.is_err() {
                                    break;
                                }
                                // Retained so a RESUME can replay it. Frames sent
                                // through `tx` are session-scoped and carry no
                                // seq, so they are not replayable.
                                resume::push(&mut replay_buffer, seq, payload);
                            }
                        }
                    }
                }
                // Heartbeat check
                _ = heartbeat_interval.tick() => {
                    if last_heartbeat.elapsed() > HEARTBEAT_TIMEOUT {
                        // Session timed out
                        break;
                    }
                }
                // Incoming messages
                msg = ws_stream.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            let parsed = serde_json::from_str::<GatewayMessage>(&text).ok();

                            // Keepalives are never throttled. Dropping a heartbeat
                            // lets `last_heartbeat` go stale and kills the session
                            // at the next timeout check, which selectively culls
                            // the busiest clients — exactly the ones least likely
                            // to deserve it.
                            let is_heartbeat =
                                parsed.as_ref().map(|m| m.op) == Some(events::opcode::HEARTBEAT);
                            if !is_heartbeat {
                                if ws_rate_window_start.elapsed() >= WS_RATE_WINDOW {
                                    ws_msg_count = 0;
                                    ws_rate_window_start = tokio::time::Instant::now();
                                }
                                ws_msg_count += 1;
                                if ws_msg_count > WS_RATE_LIMIT {
                                    // Say so rather than going quiet: a silent drop
                                    // leaves the client with no idea it should back
                                    // off, and no idea why its traffic stopped.
                                    send_close(
                                        &mut ws_sink,
                                        events::close_code::RATE_LIMITED,
                                        "rate limit exceeded",
                                    ).await;
                                    resumable = false;
                                    break;
                                }
                            }

                            let Some(gw_msg) = parsed else { continue };

                            match gw_msg.op {
                                op if op == events::opcode::HEARTBEAT => {
                                    last_heartbeat = tokio::time::Instant::now();
                                    let ack = serde_json::json!({
                                        "op": events::opcode::HEARTBEAT_ACK
                                    });
                                    if ws_sink.send(Message::Text(ack.to_string().into())).await.is_err() {
                                        break;
                                    }
                                }
                                // The handshake is over. Answering plainly keeps
                                // these from being a rate-limit-exempt spam vector.
                                op if op == events::opcode::IDENTIFY || op == events::opcode::RESUME => {
                                    send_close(
                                        &mut ws_sink,
                                        events::close_code::ALREADY_AUTHENTICATED,
                                        "already authenticated",
                                    ).await;
                                    resumable = false;
                                    break;
                                }
                                op if op == events::opcode::PRESENCE_UPDATE => {
                                    if let Some(data) = gw_msg.data {
                                        if let Ok(psu) = serde_json::from_value::<PresenceUpdateData>(data) {
                                            let status = if VALID_STATUSES.contains(&psu.status.as_str()) {
                                                psu.status.as_str()
                                            } else {
                                                "online"
                                            };
                                            let activities = match psu.activity {
                                                Some(a) => vec![a],
                                                None => vec![],
                                            };
                                            crate::presence::set_presence(&state, &user_id, status, activities.clone());
                                            broadcast_presence(
                                                &state, &user_id, &space_ids, &friend_ids, status, &activities,
                                            ).await;
                                        }
                                    }
                                }
                                op if op == events::opcode::VOICE_STATE_UPDATE => {
                                    if let Some(data) = gw_msg.data {
                                        if let Ok(vsu) = serde_json::from_value::<VoiceStateUpdateData>(data) {
                                            let self_mute = vsu.self_mute.unwrap_or(false);
                                            let self_deaf = vsu.self_deaf.unwrap_or(false);
                                            let self_video = vsu.self_video.unwrap_or(false);
                                            let self_stream = vsu.self_stream.unwrap_or(false);

                                            if let Some(channel_id) = vsu.channel_id {
                                                let auth_user = crate::middleware::auth::AuthUser {
                                                    user_id: user_id.clone(),
                                                    is_bot,
                                                    is_admin,
                                                    is_guest: is_guest_session,
                                                    guest_space_id: None,
                                                };

                                                let channel = match crate::db::channels::get_channel_row(&state.db, &channel_id).await {
                                                    Ok(ch) => ch,
                                                    Err(_) => continue,
                                                };
                                                let is_dm = routes::voice::is_dm_channel(&channel.channel_type);

                                                // The scope comes from the channel, never from the
                                                // payload, so a client cannot aim a broadcast at a
                                                // space the channel doesn't belong to. DM/group DM
                                                // calls resolve to `None`.
                                                let scope_space_id: Option<String> = if is_dm {
                                                    None
                                                } else {
                                                    match channel.space_id.clone() {
                                                        Some(sid) if space_ids.contains(&sid) => Some(sid),
                                                        _ => continue,
                                                    }
                                                };

                                                // A payload that names a space must name the
                                                // channel's own. Omitting it is allowed.
                                                if let Some(ref claimed) = vsu.space_id {
                                                    if scope_space_id.as_deref() != Some(claimed.as_str()) {
                                                        continue;
                                                    }
                                                }

                                                // Participation (DM) or `connect` (space),
                                                // re-checked on every op including flag-only ones.
                                                if crate::middleware::permissions::require_channel_permission(
                                                    &state.db, &channel_id, &auth_user, "connect",
                                                ).await.is_err() {
                                                    continue;
                                                }

                                                // Check if user is already in this exact channel (flag-only update)
                                                let current_channel = crate::voice::state::get_user_voice_state(&state, &user_id)
                                                    .and_then(|vs| vs.channel_id.clone());
                                                let is_same_channel = current_channel.as_deref() == Some(channel_id.as_str());

                                                if is_same_channel {
                                                    // Update flags in-place — no LiveKit teardown/rejoin
                                                    if let Some(voice_state) = crate::voice::state::update_voice_state(
                                                        &state, &user_id, self_mute, self_deaf, self_video, self_stream,
                                                    ) {
                                                        routes::voice::broadcast_voice_state_update(
                                                            &state, &channel_id, scope_space_id.as_deref(), &voice_state,
                                                        ).await;
                                                    }
                                                } else {
                                                    // New join or channel move — full LiveKit flow.
                                                    // DM calls have no channel type or timeout to
                                                    // check; participation was enough.
                                                    if !is_dm {
                                                        if channel.channel_type != "voice" {
                                                            continue;
                                                        }
                                                        // Timed-out members cannot connect to voice.
                                                        let sid = match scope_space_id {
                                                            Some(ref sid) => sid.as_str(),
                                                            None => continue,
                                                        };
                                                        if crate::middleware::permissions::require_not_timed_out(
                                                            &state.db, sid, &auth_user,
                                                        ).await.is_err() {
                                                            continue;
                                                        }
                                                    }

                                                    let (voice_state, prev) = crate::voice::state::join_voice_channel(
                                                        &state, &user_id, scope_space_id.as_deref(), &channel_id,
                                                        &session_id, self_mute, self_deaf, self_video, self_stream,
                                                    );

                                                    // Clean up old LiveKit room if the user moved channels
                                                    if let Some(ref prev_ch) = prev {
                                                        if !state.test_mode {
                                                            if let Some(ref lk) = state.livekit_client {
                                                                lk.remove_participant(prev_ch, &user_id).await;
                                                                lk.delete_room_if_empty(prev_ch).await;
                                                            }
                                                        }
                                                    }

                                                    routes::voice::broadcast_voice_state_update(
                                                        &state, &channel_id, scope_space_id.as_deref(), &voice_state,
                                                    ).await;

                                                    // Send voice.server_update directly to this session
                                                    if let Some(ref lk) = state.livekit_client {
                                                        if !state.test_mode {
                                                            let _ = lk.ensure_room(&channel_id).await;
                                                        }
                                                        let display_name = crate::db::users::get_user(&state.db, &user_id)
                                                            .await
                                                            .ok()
                                                            .and_then(|u| u.display_name.or(Some(u.username)))
                                                            .unwrap_or_else(|| user_id.clone());
                                                        let server_update = match lk.generate_token(&user_id, &display_name, &channel_id) {
                                                            Ok(token) => serde_json::json!({
                                                                "op": events::opcode::EVENT,
                                                                "type": "voice.server_update",
                                                                "data": {
                                                                    "space_id": scope_space_id,
                                                                    "channel_id": channel_id,
                                                                    "backend": "livekit",
                                                                    "url": lk.external_url(),
                                                                    "token": token
                                                                }
                                                            }),
                                                            Err(_) => serde_json::json!({
                                                                "op": events::opcode::EVENT,
                                                                "type": "voice.server_update",
                                                                "data": {
                                                                    "space_id": scope_space_id,
                                                                    "channel_id": channel_id,
                                                                    "backend": "livekit",
                                                                    "error": "failed to generate token"
                                                                }
                                                            }),
                                                        };
                                                        let _ = tx.send(server_update.to_string());
                                                    }
                                                }
                                            } else {
                                                // Leave voice
                                                if let Some(old_vs) = crate::voice::state::leave_voice_channel(&state, &user_id) {
                                                    if let Some(ref left_channel) = old_vs.channel_id {
                                                        let left_state = crate::models::voice::VoiceState {
                                                            user_id: user_id.clone(),
                                                            space_id: old_vs.space_id.clone(),
                                                            channel_id: None,
                                                            session_id: session_id.clone(),
                                                            deaf: false,
                                                            mute: false,
                                                            self_deaf: false,
                                                            self_mute: false,
                                                            self_stream: false,
                                                            self_video: false,
                                                            suppress: false,
                                                        };
                                                        // Routed to the space, or to the DM
                                                        // participants when there is none — a
                                                        // broadcast with neither would reach every
                                                        // session on the instance.
                                                        routes::voice::broadcast_voice_state_update(
                                                            &state, left_channel, old_vs.space_id.as_deref(), &left_state,
                                                        ).await;

                                                        // LiveKit cleanup
                                                        if !state.test_mode {
                                                            if let Some(ref lk) = state.livekit_client {
                                                                lk.remove_participant(left_channel, &user_id).await;
                                                                lk.delete_room_if_empty(left_channel).await;
                                                            }
                                                        }

                                                        end_dm_call_if_empty(&state, &old_vs, left_channel, &user_id).await;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        // An explicit close is a deliberate sign-off, so there is
                        // nothing to hold open for a resume.
                        Some(Ok(Message::Close(_))) => {
                            resumable = false;
                            break;
                        }
                        // A broken stream can keep yielding errors, so treat the
                        // first one as the end rather than spinning on it. The
                        // socket dropping is exactly the case RESUME is for.
                        Some(Err(_)) | None => break,
                        _ => {}
                    }
                }
            }
        }
    }

    // Cleanup: remove from voice if connected
    if let Some(old_vs) = crate::voice::state::leave_voice_channel(&state, &user_id) {
        if let Some(ref ch_id) = old_vs.channel_id {
            let left_state = crate::models::voice::VoiceState {
                user_id: user_id.clone(),
                space_id: old_vs.space_id.clone(),
                channel_id: None,
                session_id: session_id.clone(),
                deaf: false,
                mute: false,
                self_deaf: false,
                self_mute: false,
                self_stream: false,
                self_video: false,
                suppress: false,
            };
            // Reaches DM participants as well as spaces; dropping the socket
            // mid-call used to leave the other side of a DM call showing a
            // participant who was already gone.
            routes::voice::broadcast_voice_state_update(
                &state,
                ch_id,
                old_vs.space_id.as_deref(),
                &left_state,
            )
            .await;

            // LiveKit cleanup on disconnect
            if !state.test_mode {
                if let Some(ref lk) = state.livekit_client {
                    lk.remove_participant(ch_id, &user_id).await;
                    lk.delete_room_if_empty(ch_id).await;
                }
            }

            end_dm_call_if_empty(&state, &old_vs, ch_id, &user_id).await;
        }
    }

    // Cleanup: remove session from dispatcher
    if let Some(ref dispatcher) = *state.dispatcher.read().await {
        dispatcher.remove_session(&session_id);
    }

    // Guest cleanup: decrement guest count and broadcast updated count
    if is_guest_session {
        for sid in &space_ids {
            let new_count = {
                let mut entry = state.guest_counts.entry(sid.clone()).or_insert(0);
                if *entry > 0 {
                    *entry -= 1;
                }
                *entry
            };
            // Broadcast anonymous_count_updated to the space
            if let Some(ref gtx) = *state.gateway_tx.read().await {
                let event = serde_json::json!({
                    "op": events::opcode::EVENT,
                    "type": "anonymous_count_updated",
                    "data": { "count": new_count, "space_id": sid }
                });
                let _ = gtx.send(GatewayBroadcast {
                    space_id: Some(sid.clone()),
                    target_user_ids: None,
                    event,
                    intent: "members".to_string(),
                });
            }
        }
    }

    // Guests have no presence, and an anonymous session is cheap to
    // re-establish, so they are never parked for resume.
    if !resumable || is_guest_session {
        if !is_guest_session
            && !crate::presence::user_has_other_sessions(&state, &user_id, &session_id).await
        {
            crate::presence::remove_presence(&state, &user_id);
            broadcast_presence(&state, &user_id, &space_ids, &friend_ids, "offline", &[]).await;
        }
        return;
    }

    // The socket dropped rather than signed off, so park the session instead of
    // ending it: this task keeps collecting events for a RESUME to replay, and
    // presence stays standing for the window. A transient blip used to broadcast
    // offline immediately and online again on reconnect, which everyone in the
    // user's spaces saw as a flap.
    drop(ws_sink);
    drop(ws_stream);

    let (claim_tx, mut claim_rx) = mpsc::channel::<oneshot::Sender<Handover>>(1);
    state.resumable_sessions.insert(
        session_id.clone(),
        ParkedSession {
            user_id: user_id.clone(),
            intents: user_intents.clone(),
            claim_tx,
        },
    );

    let deadline = tokio::time::Instant::now() + resume::RESUME_WINDOW;
    let mut claim: Option<oneshot::Sender<Handover>> = None;
    loop {
        tokio::select! {
            reply = claim_rx.recv() => {
                claim = reply;
                break;
            }
            _ = tokio::time::sleep_until(deadline) => break,
            broadcast = async {
                if let Some(ref mut rx) = broadcast_rx {
                    rx.recv().await.ok()
                } else {
                    std::future::pending::<Option<GatewayBroadcast>>().await
                }
            } => {
                let Some(broadcast) = broadcast else { continue };
                match classify_broadcast(&broadcast, &user_id, &space_ids, &muted_channel_ids, &user_intents) {
                    Delivery::Skip => {}
                    Delivery::RefreshMutes => {
                        muted_channel_ids = db::mutes::list_effective_muted_channel_ids(&state.db, &user_id).await
                            .map(|ids| ids.into_iter().collect())
                            .unwrap_or_default();
                    }
                    Delivery::RefreshSpaces(event) => {
                        space_ids = reload_space_ids(&state, &user_id, is_guest_session, &space_ids).await;
                        sync_session_spaces(&state, &session_id, &space_ids).await;
                        if let Some(event) = event {
                            seq += 1;
                            resume::push(&mut replay_buffer, seq, stamp_seq(event, seq));
                        }
                    }
                    Delivery::Send(event) => {
                        seq += 1;
                        resume::push(&mut replay_buffer, seq, stamp_seq(event, seq));
                    }
                }
            }
        }
    }

    if let Some(reply) = claim {
        // A resuming socket took the session over; presence carries on untouched.
        let _ = reply.send(Handover {
            seq,
            buffer: replay_buffer,
            broadcast_rx,
        });
        return;
    }

    // The window closed unclaimed. The registry entry is only ours to drop here
    // — a claim removes it before asking for the handover.
    state.resumable_sessions.remove(&session_id);
    if !crate::presence::user_has_other_sessions(&state, &user_id, &session_id).await {
        crate::presence::remove_presence(&state, &user_id);
        broadcast_presence(&state, &user_id, &space_ids, &friend_ids, "offline", &[]).await;
    }
}

/// After the last participant leaves a DM call, tell the rest of the channel the
/// call is over so ringing/active-call UI clears. Mirrors `POST /voice/leave`;
/// no-ops for space channels, which have no call lifecycle.
async fn end_dm_call_if_empty(
    state: &AppState,
    old_vs: &crate::models::voice::VoiceState,
    channel_id: &str,
    user_id: &str,
) {
    if old_vs.space_id.is_some()
        || !crate::voice::state::get_channel_voice_states(state, channel_id).is_empty()
    {
        return;
    }
    routes::voice::broadcast_call_event(
        state,
        channel_id,
        "call.end",
        serde_json::json!({
            "channel_id": channel_id,
            "user_id": user_id,
        }),
    )
    .await;
}

struct ResolvedAuth {
    user_id: String,
    is_bot: bool,
    is_admin: bool,
    is_guest: bool,
    guest_space_id: Option<String>,
}

async fn resolve_token(state: &AppState, token: &str) -> Option<ResolvedAuth> {
    // Token format: "Bot xxx" or "Bearer xxx"
    let (user_id, is_bot) = if let Some(tok) = token.strip_prefix("Bot ") {
        let token_hash = auth_resolve::create_token_hash(tok);
        let row = sqlx::query_as::<_, (String,)>(&crate::db::q(
            "SELECT user_id FROM bot_tokens WHERE token_hash = ?",
        ))
        .bind(&token_hash)
        .fetch_optional(&state.db)
        .await
        .ok()??;
        (row.0, true)
    } else {
        // Neither prefix means the token is unusable — `?` bails the same way
        // the old trailing `else { return None }` did.
        let tok = token.strip_prefix("Bearer ")?;
        let token_hash = auth_resolve::create_token_hash(tok);
        let now_fn = crate::db::now_sql(state.db_is_postgres);
        let sql = crate::db::q(&format!(
            "SELECT user_id FROM user_tokens WHERE token_hash = ? AND expires_at > {now_fn}",
        ));
        let row = sqlx::query_as::<_, (String,)>(&sql)
            .bind(&token_hash)
            .fetch_optional(&state.db)
            .await
            .ok()?;

        if let Some(row) = row {
            (row.0, false)
        } else {
            // Try guest token lookup
            let now_fn2 = crate::db::now_sql(state.db_is_postgres);
            let guest_sql = crate::db::q(&format!(
                "SELECT space_id FROM guest_tokens WHERE token_hash = ? AND expires_at > {now_fn2}",
            ));
            let guest_row = sqlx::query_as::<_, (String,)>(&guest_sql)
                .bind(&token_hash)
                .fetch_optional(&state.db)
                .await
                .ok()??;

            let guest_user_id = format!("guest:{}", &token_hash[..16]);
            return Some(ResolvedAuth {
                user_id: guest_user_id,
                is_bot: false,
                is_admin: false,
                is_guest: true,
                guest_space_id: Some(guest_row.0),
            });
        }
    };

    let user = crate::db::users::get_user(&state.db, &user_id).await.ok()?;

    // Disabled users cannot connect to the gateway
    if user.disabled {
        return None;
    }

    Some(ResolvedAuth {
        user_id,
        is_bot,
        is_admin: user.is_admin,
        is_guest: false,
        guest_space_id: None,
    })
}
