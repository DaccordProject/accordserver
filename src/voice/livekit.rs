use std::sync::Arc;

use livekit_api::access_token::{AccessToken, VideoGrants};
use livekit_api::services::room::{CreateRoomOptions, RoomClient};

use crate::error::AppError;

#[derive(Clone)]
pub struct LiveKitClient {
    url: String,
    api_key: String,
    api_secret: String,
    room_client: Arc<RoomClient>,
}

impl LiveKitClient {
    pub fn new(url: &str, api_key: &str, api_secret: &str) -> Self {
        let room_client = Arc::new(RoomClient::with_api_key(url, api_key, api_secret));
        Self {
            url: url.to_string(),
            api_key: api_key.to_string(),
            api_secret: api_secret.to_string(),
            room_client,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn room_name(channel_id: &str) -> String {
        format!("channel_{channel_id}")
    }

    pub fn generate_token(&self, user_id: &str, channel_id: &str) -> Result<String, AppError> {
        let room = Self::room_name(channel_id);
        let token = AccessToken::with_api_key(&self.api_key, &self.api_secret)
            .with_identity(user_id)
            .with_grants(VideoGrants {
                room_join: true,
                room,
                can_publish: true,
                can_subscribe: true,
                can_publish_data: true,
                ..Default::default()
            })
            .with_ttl(std::time::Duration::from_secs(6 * 60 * 60))
            .to_jwt()
            .map_err(|e| AppError::Internal(format!("failed to generate LiveKit token: {e}")))?;
        Ok(token)
    }

    pub async fn ensure_room(&self, channel_id: &str) -> Result<(), AppError> {
        let room_name = Self::room_name(channel_id);
        self.room_client
            .create_room(&room_name, CreateRoomOptions::default())
            .await
            .map_err(|e| AppError::Internal(format!("failed to create LiveKit room: {e}")))?;
        Ok(())
    }

    pub async fn remove_participant(&self, channel_id: &str, user_id: &str) {
        let room_name = Self::room_name(channel_id);
        if let Err(e) = self
            .room_client
            .remove_participant(&room_name, user_id)
            .await
        {
            tracing::warn!(
                "failed to remove participant {user_id} from LiveKit room {room_name}: {e}"
            );
        }
    }

    pub async fn delete_room_if_empty(&self, channel_id: &str) {
        let room_name = Self::room_name(channel_id);
        match self.room_client.list_participants(&room_name).await {
            Ok(participants) if participants.is_empty() => {
                if let Err(e) = self.room_client.delete_room(&room_name).await {
                    tracing::warn!("failed to delete empty LiveKit room {room_name}: {e}");
                }
            }
            Err(e) => {
                tracing::warn!("failed to list participants in LiveKit room {room_name}: {e}");
            }
            _ => {}
        }
    }
}
