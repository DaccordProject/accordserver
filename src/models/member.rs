use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub user_id: String,
    pub space_id: String,
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub roles: Vec<String>,
    pub joined_at: String,
    pub premium_since: Option<String>,
    pub deaf: bool,
    pub mute: bool,
    pub pending: Option<bool>,
    pub timed_out_until: Option<String>,
    pub permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct MemberRow {
    pub user_id: String,
    pub space_id: String,
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub joined_at: String,
    pub premium_since: Option<String>,
    pub deaf: bool,
    pub mute: bool,
    pub pending: bool,
    pub timed_out_until: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMember {
    pub nickname: Option<String>,
    pub avatar: Option<String>,
    pub roles: Option<Vec<String>>,
    pub mute: Option<bool>,
    pub deaf: Option<bool>,
    /// Moderation timeout. Accepts either `communication_disabled_until` or the
    /// legacy `timed_out_until` key. The double `Option` distinguishes three
    /// inputs: field absent (`None` — leave unchanged), explicit `null`
    /// (`Some(None)` — clear the timeout), or a timestamp (`Some(Some(ts))` —
    /// set the timeout). The value is an RFC3339 timestamp.
    #[serde(
        default,
        alias = "timed_out_until",
        deserialize_with = "deserialize_double_option"
    )]
    pub communication_disabled_until: Option<Option<String>>,
}

/// Deserializes a present-but-possibly-null field into `Some(Option<T>)` while
/// an absent field falls through to the `#[serde(default)]` of `None`. This is
/// the standard trick for distinguishing "omitted" from "explicitly null".
fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}
