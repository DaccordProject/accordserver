use serde::{Deserialize, Serialize};

use super::emoji::Emoji;
use super::role::Role;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    pub splash: Option<String>,
    pub owner_id: String,
    pub features: Vec<String>,
    pub verification_level: String,
    pub default_notifications: String,
    pub explicit_content_filter: String,
    pub roles: Vec<Role>,
    pub emojis: Vec<Emoji>,
    pub member_count: Option<i64>,
    pub presence_count: Option<i64>,
    pub max_members: Option<i64>,
    pub vanity_url_code: Option<String>,
    pub preferred_locale: String,
    pub afk_channel_id: Option<String>,
    pub afk_timeout: i64,
    pub system_channel_id: Option<String>,
    pub rules_channel_id: Option<String>,
    pub nsfw_level: String,
    pub premium_tier: String,
    pub public: bool,
    pub premium_subscription_count: i64,
    pub created_at: String,
}

/// Public space listing with member count for directory/discovery use.
#[derive(Debug, Clone, Serialize)]
pub struct PublicSpaceRow {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub member_count: i64,
    pub public: bool,
}

/// Lightweight version from the DB row before loading relations.
#[derive(Debug, Clone, Serialize)]
pub struct SpaceRow {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    pub splash: Option<String>,
    pub owner_id: String,
    pub verification_level: String,
    pub default_notifications: String,
    pub explicit_content_filter: String,
    pub vanity_url_code: Option<String>,
    pub preferred_locale: String,
    pub afk_channel_id: Option<String>,
    pub afk_timeout: i64,
    pub system_channel_id: Option<String>,
    pub rules_channel_id: Option<String>,
    pub nsfw_level: String,
    pub premium_tier: String,
    pub public: bool,
    pub premium_subscription_count: i64,
    pub max_members: i64,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSpace {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub public: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSpace {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    pub verification_level: Option<String>,
    pub default_notifications: Option<String>,
    pub afk_channel_id: Option<String>,
    pub afk_timeout: Option<i64>,
    pub system_channel_id: Option<String>,
    pub rules_channel_id: Option<String>,
    pub preferred_locale: Option<String>,
    pub public: Option<bool>,
}
