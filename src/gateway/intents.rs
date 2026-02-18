/// All valid intent strings.
pub const ALL_INTENTS: &[&str] = &[
    "spaces",
    "moderation",
    "emojis",
    "voice_states",
    "messages",
    "message_reactions",
    "message_typing",
    "direct_messages",
    "dm_reactions",
    "dm_typing",
    "scheduled_events",
    // Privileged
    "members",
    "presences",
    "message_content",
];

pub const PRIVILEGED_INTENTS: &[&str] = &["members", "presences", "message_content"];

/// Map an event type to its required intent.
pub fn intent_for_event(event_type: &str) -> Option<&'static str> {
    match event_type {
        "message.create" | "message.update" | "message.delete" | "message.delete_bulk" => {
            Some("messages")
        }
        "member.join" | "member.leave" | "member.update" | "member.chunk" => Some("members"),
        "space.create" | "space.update" | "space.delete" => Some("spaces"),
        "channel.create" | "channel.update" | "channel.delete" | "channel.pins_update" => {
            Some("spaces")
        }
        "role.create" | "role.update" | "role.delete" => Some("spaces"),
        "reaction.add" | "reaction.remove" | "reaction.clear" | "reaction.clear_emoji" => {
            Some("message_reactions")
        }
        "typing.start" => Some("message_typing"),
        "presence.update" => Some("presences"),
        "voice.state_update" | "voice.server_update" | "voice.signal" => Some("voice_states"),
        "ban.create" | "ban.delete" => Some("moderation"),
        "invite.create" | "invite.delete" => Some("spaces"),
        "emoji.update" => Some("emojis"),
        "interaction.create" => None, // always delivered
        _ => None,
    }
}

/// Check if a set of intents includes the required intent for an event.
pub fn has_intent(intents: &[String], event_type: &str) -> bool {
    match intent_for_event(event_type) {
        Some(required) => intents.iter().any(|i| i == required),
        None => true, // No intent required = always delivered
    }
}
