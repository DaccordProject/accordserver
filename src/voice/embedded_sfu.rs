use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::api::API;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};
use webrtc::track::track_remote::TrackRemote;

use crate::gateway::events::GatewayBroadcast;

/// Top-level embedded SFU manager.
/// Owns a map of rooms keyed by channel_id, and a shared WebRTC API instance.
pub struct EmbeddedSfu {
    api: Arc<API>,
    rooms: DashMap<String, Arc<Mutex<SfuRoom>>>,
    gateway_tx: Arc<RwLock<Option<broadcast::Sender<GatewayBroadcast>>>>,
}

/// Per-channel voice room in the SFU.
struct SfuRoom {
    peers: HashMap<String, SfuPeer>,
    /// Forwarded tracks: user_id -> TrackLocalStaticRTP that other peers subscribe to.
    forwarded_tracks: HashMap<String, Arc<TrackLocalStaticRTP>>,
}

/// Per-user state within a room.
struct SfuPeer {
    pc: Arc<RTCPeerConnection>,
    session_id: String,
    space_id: String,
    /// Whether this peer needs renegotiation (new tracks were added).
    _needs_renegotiation: bool,
}

impl EmbeddedSfu {
    pub fn new(
        gateway_tx: Arc<RwLock<Option<broadcast::Sender<GatewayBroadcast>>>>,
    ) -> Arc<Self> {
        let mut media_engine = MediaEngine::default();
        // Register default codecs (opus for audio)
        media_engine.register_default_codecs().expect("failed to register default codecs");

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .expect("failed to register interceptors");

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        Arc::new(Self {
            api: Arc::new(api),
            rooms: DashMap::new(),
            gateway_tx,
        })
    }

    /// Entry point from gateway. Dispatches to offer/answer/ice handlers.
    pub async fn handle_signal(
        self: &Arc<Self>,
        user_id: &str,
        session_id: &str,
        channel_id: &str,
        space_id: &str,
        signal_type: &str,
        payload: &serde_json::Value,
    ) {
        match signal_type {
            "offer" => {
                self.handle_offer(user_id, session_id, channel_id, space_id, payload)
                    .await;
            }
            "answer" => {
                self.handle_answer(user_id, channel_id, payload).await;
            }
            "ice_candidate" => {
                self.handle_ice_candidate(user_id, channel_id, payload)
                    .await;
            }
            other => {
                tracing::warn!("embedded_sfu: unknown signal type: {}", other);
            }
        }
    }

    /// Handle an SDP offer from a client.
    /// Creates a PeerConnection, sets remote description, creates answer,
    /// sends answer back, and adds existing forwarded tracks.
    async fn handle_offer(
        self: &Arc<Self>,
        user_id: &str,
        session_id: &str,
        channel_id: &str,
        space_id: &str,
        payload: &serde_json::Value,
    ) {
        let sdp = match payload.get("sdp").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                tracing::warn!("embedded_sfu: offer missing sdp");
                return;
            }
        };

        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = match self.api.new_peer_connection(config).await {
            Ok(pc) => Arc::new(pc),
            Err(e) => {
                tracing::error!("embedded_sfu: failed to create peer connection: {}", e);
                return;
            }
        };

        // Add a transceiver for receiving audio
        if let Err(e) = pc
            .add_transceiver_from_kind(RTPCodecType::Audio, None)
            .await
        {
            tracing::error!("embedded_sfu: failed to add audio transceiver: {}", e);
            return;
        }

        // Set up on_track handler
        let sfu = Arc::clone(self);
        let channel_id_clone = channel_id.to_string();
        let user_id_clone = user_id.to_string();
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let sfu = Arc::clone(&sfu);
            let channel_id = channel_id_clone.clone();
            let user_id = user_id_clone.clone();
            Box::pin(async move {
                sfu.on_track(&channel_id, &user_id, track).await;
            })
        }));

        // Set up connection state change handler for cleanup
        let sfu_cleanup = Arc::clone(self);
        let channel_id_cleanup = channel_id.to_string();
        let user_id_cleanup = user_id.to_string();
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let _sfu = Arc::clone(&sfu_cleanup);
            let channel_id = channel_id_cleanup.clone();
            let user_id = user_id_cleanup.clone();
            Box::pin(async move {
                match state {
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                        tracing::info!(
                            "embedded_sfu: peer {} state {:?} in channel {}",
                            user_id,
                            state,
                            channel_id
                        );
                    }
                    _ => {}
                }
            })
        }));

        // Set remote description (client's offer)
        let offer = RTCSessionDescription::offer(sdp).expect("invalid offer SDP");
        if let Err(e) = pc.set_remote_description(offer).await {
            tracing::error!("embedded_sfu: failed to set remote description: {}", e);
            return;
        }

        // Get or create room
        let room = self
            .rooms
            .entry(channel_id.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(SfuRoom {
                    peers: HashMap::new(),
                    forwarded_tracks: HashMap::new(),
                }))
            })
            .clone();

        // Add existing forwarded tracks to this peer's connection
        {
            let room_guard = room.lock().await;
            for (track_user_id, track) in &room_guard.forwarded_tracks {
                if track_user_id != user_id {
                    let rtp_sender = pc
                        .add_track(Arc::clone(track) as Arc<dyn TrackLocal + Send + Sync>)
                        .await;
                    if let Err(e) = rtp_sender {
                        tracing::error!(
                            "embedded_sfu: failed to add forwarded track from {} to {}: {}",
                            track_user_id,
                            user_id,
                            e
                        );
                    }
                }
            }
        }

        // Create answer
        let answer = match pc.create_answer(None).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("embedded_sfu: failed to create answer: {}", e);
                return;
            }
        };

        // Set local description
        if let Err(e) = pc.set_local_description(answer.clone()).await {
            tracing::error!("embedded_sfu: failed to set local description: {}", e);
            return;
        }

        // Wait for ICE gathering to complete
        let mut gather_complete = pc.gathering_complete_promise().await;
        let _ = gather_complete.recv().await;

        // Get the final local description (with ICE candidates)
        let local_desc = match pc.local_description().await {
            Some(desc) => desc,
            None => {
                tracing::error!("embedded_sfu: no local description after gathering");
                return;
            }
        };

        // Store peer in room
        {
            let mut room_guard = room.lock().await;

            // Notify existing peers about the new peer
            let existing_user_ids: Vec<(String, String)> = room_guard
                .peers
                .iter()
                .map(|(uid, peer)| (uid.clone(), peer.space_id.clone()))
                .collect();

            room_guard.peers.insert(
                user_id.to_string(),
                SfuPeer {
                    pc: Arc::clone(&pc),
                    session_id: session_id.to_string(),
                    space_id: space_id.to_string(),
                    _needs_renegotiation: false,
                },
            );

            // Send peer_joined signals
            for (existing_uid, existing_space_id) in &existing_user_ids {
                // Tell existing peer about new peer
                self.send_signal_to_user(
                    existing_space_id,
                    existing_uid,
                    "peer_joined",
                    &serde_json::json!({ "user_id": user_id }),
                )
                .await;
                // Tell new peer about existing peer
                self.send_signal_to_user(
                    space_id,
                    user_id,
                    "peer_joined",
                    &serde_json::json!({ "user_id": existing_uid }),
                )
                .await;
            }
        }

        // Send answer back to the client
        let answer_payload = serde_json::json!({
            "sdp": local_desc.sdp,
            "type": "answer"
        });
        self.send_signal_to_user(space_id, user_id, "answer", &answer_payload)
            .await;

        tracing::info!(
            "embedded_sfu: peer {} joined channel {} (session {})",
            user_id,
            channel_id,
            session_id
        );
    }

    /// Handle an SDP answer from a client (during renegotiation).
    async fn handle_answer(
        &self,
        user_id: &str,
        channel_id: &str,
        payload: &serde_json::Value,
    ) {
        let sdp = match payload.get("sdp").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                tracing::warn!("embedded_sfu: answer missing sdp");
                return;
            }
        };

        if let Some(room_ref) = self.rooms.get(channel_id) {
            let room = room_ref.lock().await;
            if let Some(peer) = room.peers.get(user_id) {
                let answer =
                    RTCSessionDescription::answer(sdp).expect("invalid answer SDP");
                if let Err(e) = peer.pc.set_remote_description(answer).await {
                    tracing::error!(
                        "embedded_sfu: failed to set remote description for {}: {}",
                        user_id,
                        e
                    );
                }
            }
        }
    }

    /// Handle an ICE candidate from a client.
    async fn handle_ice_candidate(
        &self,
        user_id: &str,
        channel_id: &str,
        payload: &serde_json::Value,
    ) {
        // The client sends ICE candidates with mid/index/sdp keys.
        // The webrtc crate expects candidate_init format.
        let sdp = match payload.get("sdp").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return,
        };
        let mid = payload
            .get("mid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u16;

        let candidate = webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
            candidate: sdp,
            sdp_mid: Some(mid),
            sdp_mline_index: Some(index),
            username_fragment: None,
        };

        if let Some(room_ref) = self.rooms.get(channel_id) {
            let room = room_ref.lock().await;
            if let Some(peer) = room.peers.get(user_id) {
                if let Err(e) = peer.pc.add_ice_candidate(candidate).await {
                    tracing::error!(
                        "embedded_sfu: failed to add ICE candidate for {}: {}",
                        user_id,
                        e
                    );
                }
            }
        }
    }

    /// Called when a remote audio track arrives from a peer.
    /// Creates a TrackLocalStaticRTP for forwarding, spawns an RTP forwarding task,
    /// and adds the track to all other peers' PeerConnections (triggering renegotiation).
    async fn on_track(
        self: &Arc<Self>,
        channel_id: &str,
        user_id: &str,
        remote_track: Arc<TrackRemote>,
    ) {
        let codec = remote_track.codec();
        tracing::info!(
            "embedded_sfu: track from {} in channel {} (codec: {})",
            user_id,
            channel_id,
            codec.capability.mime_type
        );

        // Create a local track for forwarding
        let track_id = format!("fwd-{}-{}", user_id, channel_id);
        let stream_id = format!("stream-{}", user_id);
        let local_track = Arc::new(TrackLocalStaticRTP::new(
            codec.capability.clone(),
            track_id,
            stream_id,
        ));

        // Spawn RTP forwarding task
        let local_track_fwd = Arc::clone(&local_track);
        let user_id_fwd = user_id.to_string();
        let channel_id_fwd = channel_id.to_string();
        tokio::spawn(async move {
            loop {
                match remote_track.read_rtp().await {
                    Ok((packet, _)) => {
                        if let Err(err) = local_track_fwd.write_rtp(&packet).await {
                            let msg = err.to_string();
                            // Ignore closed errors when peer has left
                            if !msg.contains("closed") {
                                tracing::error!(
                                    "embedded_sfu: RTP write error for {} in {}: {}",
                                    user_id_fwd,
                                    channel_id_fwd,
                                    msg
                                );
                            }
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            tracing::debug!(
                "embedded_sfu: RTP forwarding ended for {} in {}",
                user_id_fwd,
                channel_id_fwd
            );
        });

        // Store forwarded track and add to all other peers
        if let Some(room_ref) = self.rooms.get(channel_id) {
            let mut room = room_ref.lock().await;
            room.forwarded_tracks
                .insert(user_id.to_string(), Arc::clone(&local_track));

            // Add this track to all other peers and trigger renegotiation
            let other_peers: Vec<(String, Arc<RTCPeerConnection>, String)> = room
                .peers
                .iter()
                .filter(|(uid, _)| uid.as_str() != user_id)
                .map(|(uid, peer)| (uid.clone(), Arc::clone(&peer.pc), peer.space_id.clone()))
                .collect();

            for (peer_uid, peer_pc, peer_space_id) in other_peers {
                // Add the forwarded track
                if let Err(e) = peer_pc
                    .add_track(Arc::clone(&local_track) as Arc<dyn TrackLocal + Send + Sync>)
                    .await
                {
                    tracing::error!(
                        "embedded_sfu: failed to add track to {}: {}",
                        peer_uid,
                        e
                    );
                    continue;
                }

                // Renegotiate: create offer for this peer
                match peer_pc.create_offer(None).await {
                    Ok(offer) => {
                        if let Err(e) = peer_pc.set_local_description(offer.clone()).await {
                            tracing::error!(
                                "embedded_sfu: failed to set local desc for {}: {}",
                                peer_uid,
                                e
                            );
                            continue;
                        }

                        // Wait for ICE gathering
                        let mut gather_complete = peer_pc.gathering_complete_promise().await;
                        let _ = gather_complete.recv().await;

                        if let Some(local_desc) = peer_pc.local_description().await {
                            let offer_payload = serde_json::json!({
                                "sdp": local_desc.sdp,
                                "type": "offer"
                            });
                            self.send_signal_to_user(
                                &peer_space_id,
                                &peer_uid,
                                "offer",
                                &offer_payload,
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "embedded_sfu: failed to create renegotiation offer for {}: {}",
                            peer_uid,
                            e
                        );
                    }
                }
            }
        }
    }

    /// Remove a peer from a room. Closes PeerConnection, removes forwarded tracks,
    /// sends peer_left signals. Destroys empty rooms.
    pub async fn remove_peer(&self, channel_id: &str, user_id: &str) {
        let (peer, remaining_peers, had_forwarded_track) = {
            let room_ref = match self.rooms.get(channel_id) {
                Some(r) => r.clone(),
                None => return,
            };
            let mut room = room_ref.lock().await;
            let peer = room.peers.remove(user_id);
            let had_track = room.forwarded_tracks.remove(user_id).is_some();
            let remaining: Vec<(String, Arc<RTCPeerConnection>, String)> = room
                .peers
                .iter()
                .map(|(uid, p)| (uid.clone(), Arc::clone(&p.pc), p.space_id.clone()))
                .collect();
            (peer, remaining, had_track)
        };

        // Close peer connection
        if let Some(peer) = peer {
            let _ = peer.pc.close().await;
            tracing::info!(
                "embedded_sfu: peer {} left channel {} (session {})",
                user_id,
                channel_id,
                peer.session_id
            );
        }

        // Send peer_left to remaining peers and remove forwarded tracks
        for (peer_uid, peer_pc, peer_space_id) in &remaining_peers {
            // Send peer_left signal
            self.send_signal_to_user(
                peer_space_id,
                peer_uid,
                "peer_left",
                &serde_json::json!({ "user_id": user_id }),
            )
            .await;

            // If the leaving peer had a forwarded track, we need to renegotiate
            if had_forwarded_track {
                // The track was already removed from forwarded_tracks above.
                // The RTP forwarding task will end on its own when the remote track closes.
                // We need to renegotiate with remaining peers to remove the track.
                match peer_pc.create_offer(None).await {
                    Ok(offer) => {
                        if let Err(e) = peer_pc.set_local_description(offer.clone()).await {
                            tracing::error!(
                                "embedded_sfu: failed to set local desc for {}: {}",
                                peer_uid,
                                e
                            );
                            continue;
                        }
                        let mut gather_complete = peer_pc.gathering_complete_promise().await;
                        let _ = gather_complete.recv().await;

                        if let Some(local_desc) = peer_pc.local_description().await {
                            let offer_payload = serde_json::json!({
                                "sdp": local_desc.sdp,
                                "type": "offer"
                            });
                            self.send_signal_to_user(
                                peer_space_id,
                                peer_uid,
                                "offer",
                                &offer_payload,
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "embedded_sfu: failed to create renegotiation offer for {}: {}",
                            peer_uid,
                            e
                        );
                    }
                }
            }
        }

        // Destroy empty room
        if remaining_peers.is_empty() {
            self.rooms.remove(channel_id);
            tracing::debug!("embedded_sfu: destroyed empty room {}", channel_id);
        }
    }

    /// Send a voice.signal event to a specific user via the gateway broadcast channel.
    async fn send_signal_to_user(
        &self,
        space_id: &str,
        target_user_id: &str,
        signal_type: &str,
        payload: &serde_json::Value,
    ) {
        let event = serde_json::json!({
            "op": 0,
            "type": "voice.signal",
            "data": {
                "user_id": "sfu",
                "session_id": "sfu",
                "channel_id": "",
                "type": signal_type,
                "payload": payload
            }
        });

        if let Some(ref tx) = *self.gateway_tx.read().await {
            let _ = tx.send(GatewayBroadcast {
                space_id: Some(space_id.to_string()),
                target_user_ids: Some(vec![target_user_id.to_string()]),
                event,
                intent: "voice_states".to_string(),
            });
        }
    }
}
