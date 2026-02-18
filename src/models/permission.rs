use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOverwrite {
    pub id: String,
    #[serde(rename = "type")]
    pub overwrite_type: String,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

pub const ALL_PERMISSIONS: &[&str] = &[
    "create_invites",
    "kick_members",
    "ban_members",
    "administrator",
    "manage_channels",
    "manage_space",
    "add_reactions",
    "view_audit_log",
    "priority_speaker",
    "stream",
    "view_channel",
    "send_messages",
    "send_tts",
    "manage_messages",
    "embed_links",
    "attach_files",
    "read_history",
    "mention_everyone",
    "use_external_emojis",
    "connect",
    "speak",
    "mute_members",
    "deafen_members",
    "move_members",
    "use_vad",
    "change_nickname",
    "manage_nicknames",
    "manage_roles",
    "manage_webhooks",
    "manage_emojis",
    "use_commands",
    "manage_events",
    "manage_threads",
    "create_threads",
    "use_external_stickers",
    "send_in_threads",
    "moderate_members",
];

pub fn has_permission(perms: &[String], perm: &str) -> bool {
    perms.iter().any(|p| p == "administrator" || p == perm)
}
