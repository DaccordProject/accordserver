use crate::error::AppError;
use livekit_api::access_token::{AccessToken, VideoGrants};
use livekit_api::services::room::{CreateRoomOptions, RoomClient};
use std::sync::Arc;

#[derive(Clone)]
pub struct LiveKitClient {
    internal_url: String,
    external_url: String,
    api_key: String,
    api_secret: String,
    room_client: Arc<RoomClient>,
}

impl LiveKitClient {
    pub fn new(internal_url: &str, external_url: &str, api_key: &str, api_secret: &str) -> Self {
        Self {
            internal_url: internal_url.to_string(),
            external_url: external_url.to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            room_client: Arc::new(RoomClient::with_api_key(internal_url, api_key, api_secret)),
        }
    }

    pub fn internal_url(&self) -> &str {
        &self.internal_url
    }

    pub fn external_url(&self) -> &str {
        &self.external_url
    }

    pub fn room_name(channel_id: &str) -> String {
        format!("channel_{channel_id}")
    }

    pub fn generate_token(&self, user_id: &str, channel_id: &str) -> Result<String, AppError> {
        let room_name = Self::room_name(channel_id);
        AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity(user_id)
            .with_name(user_id)
            .with_grants(VideoGrants {
                room_join: true,
                room: room_name,
                ..Default::default()
            })
            .to_jwt()
            .map_err(|e| AppError::Internal(format!("failed to generate livekit token: {}", e)))
    }

    pub async fn ensure_room(&self, channel_id: &str) -> Result<(), AppError> {
        let room_name = Self::room_name(channel_id);
        self.room_client
            .create_room(
                &room_name,
                CreateRoomOptions {
                    empty_timeout: 300,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| AppError::Internal(format!("failed to create livekit room: {}", e)))?;
        Ok(())
    }

    pub async fn remove_participant(&self, channel_id: &str, user_id: &str) {
        let room_name = Self::room_name(channel_id);
        if let Err(e) = self.room_client.remove_participant(&room_name, user_id).await {
            tracing::warn!("Failed to remove participant {} from {}: {}", user_id, room_name, e);
        }
    }

    pub async fn delete_room_if_empty(&self, channel_id: &str) {
        let room_name = Self::room_name(channel_id);
        match self.room_client.list_participants(&room_name).await {
            Ok(participants) => {
                if participants.is_empty() {
                    if let Err(e) = self.room_client.delete_room(&room_name).await {
                        tracing::warn!("Failed to delete empty room {}: {}", room_name, e);
                    } else {
                        tracing::debug!("Deleted empty LiveKit room {}", room_name);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to list participants for room {}: {}", room_name, e);
            }
        }
    }
}
