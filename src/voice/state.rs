use crate::models::voice::VoiceState;
use crate::state::AppState;

/// Join a voice channel. Returns the new VoiceState and the previous channel_id if the user moved.
pub fn join_voice_channel(
    state: &AppState,
    user_id: &str,
    space_id: &str,
    channel_id: &str,
    session_id: &str,
    self_mute: bool,
    self_deaf: bool,
    self_video: bool,
    self_stream: bool,
) -> (VoiceState, Option<String>) {
    let previous_channel = state
        .voice_states
        .get(user_id)
        .and_then(|vs| vs.channel_id.clone());

    let voice_state = VoiceState {
        user_id: user_id.to_string(),
        space_id: Some(space_id.to_string()),
        channel_id: Some(channel_id.to_string()),
        session_id: session_id.to_string(),
        deaf: false,
        mute: false,
        self_deaf,
        self_mute,
        self_stream,
        self_video,
        suppress: false,
    };

    state
        .voice_states
        .insert(user_id.to_string(), voice_state.clone());

    (voice_state, previous_channel)
}

/// Update an existing voice state's flags in-place without changing channel or session.
/// Returns the updated VoiceState, or None if the user is not in voice.
pub fn update_voice_state(
    state: &AppState,
    user_id: &str,
    self_mute: bool,
    self_deaf: bool,
    self_video: bool,
    self_stream: bool,
) -> Option<VoiceState> {
    let mut entry = state.voice_states.get_mut(user_id)?;
    let vs = entry.value_mut();
    vs.self_mute = self_mute;
    vs.self_deaf = self_deaf;
    vs.self_video = self_video;
    vs.self_stream = self_stream;
    Some(vs.clone())
}

/// Leave voice. Returns the old VoiceState if the user was in voice.
pub fn leave_voice_channel(state: &AppState, user_id: &str) -> Option<VoiceState> {
    state.voice_states.remove(user_id).map(|(_, vs)| vs)
}

/// Get all voice states for a given channel.
pub fn get_channel_voice_states(state: &AppState, channel_id: &str) -> Vec<VoiceState> {
    state
        .voice_states
        .iter()
        .filter(|entry| entry.value().channel_id.as_deref() == Some(channel_id))
        .map(|entry| entry.value().clone())
        .collect()
}

/// Get a single user's voice state.
pub fn get_user_voice_state(state: &AppState, user_id: &str) -> Option<VoiceState> {
    state.voice_states.get(user_id).map(|vs| vs.clone())
}
