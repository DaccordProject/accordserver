use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
    pub banner: Option<String>,
    pub accent_color: Option<i64>,
    pub bio: Option<String>,
    pub bot: bool,
    pub system: bool,
    pub is_admin: bool,
    pub mfa_enabled: bool,
    pub disabled: bool,
    pub flags: i64,
    pub public_flags: i64,
    pub created_at: String,
}

/// Public-facing subset of `User` returned when looking up another user's profile.
/// Omits sensitive fields: `is_admin`, `mfa_enabled`, `disabled`, `flags`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicUser {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
    pub banner: Option<String>,
    pub accent_color: Option<i64>,
    pub bio: Option<String>,
    pub bot: bool,
    pub system: bool,
    pub public_flags: i64,
    pub created_at: String,
}

impl From<User> for PublicUser {
    fn from(u: User) -> Self {
        PublicUser {
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            avatar: u.avatar,
            banner: u.banner,
            accent_color: u.accent_color,
            bio: u.bio,
            bot: u.bot,
            system: u.system,
            public_flags: u.public_flags,
            created_at: u.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateUser {
    pub username: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUser {
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
    pub banner: Option<String>,
    pub accent_color: Option<i64>,
    pub bio: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdminUpdateUser {
    pub is_admin: Option<bool>,
    pub disabled: Option<bool>,
    pub force_password_reset: Option<bool>,
    pub username: Option<String>,
    pub display_name: Option<String>,
}
