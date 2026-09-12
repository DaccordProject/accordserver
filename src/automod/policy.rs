use crate::{
    db,
    error::AppError,
    middleware::{auth::AuthUser, permissions},
    models::{channel::ChannelRow, permission::has_permission},
    state::AppState,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    pub rules: Vec<Rule>,
    pub exempt_roles: Vec<String>,
    pub exempt_permissions: Vec<String>,
    pub retention_days: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: 7,
            exempt_roles: vec![],
            exempt_permissions: vec!["manage_messages".into()],
            rules: vec![
                Rule {
                    id: "blocked-file".into(),
                    trigger: Trigger::HashDenylist,
                    scope: Scope::All,
                    action: Action::Quarantine,
                },
                Rule {
                    id: "explicit-image".into(),
                    trigger: Trigger::Media {
                        categories: vec![
                            "FEMALE_BREAST_EXPOSED".into(),
                            "FEMALE_GENITALIA_EXPOSED".into(),
                            "MALE_GENITALIA_EXPOSED".into(),
                            "ANUS_EXPOSED".into(),
                        ],
                        threshold: 0.8,
                    },
                    scope: Scope::NonNsfw,
                    action: Action::Quarantine,
                },
            ],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub trigger: Trigger,
    pub scope: Scope,
    pub action: Action,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Media {
        categories: Vec<String>,
        threshold: f32,
    },
    HashDenylist,
    LowTrust {
        min_account_age_hours: u32,
        min_space_age_hours: u32,
        require_role: bool,
    },
    NonNsfwAttachment,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    All,
    NonNsfw,
    Channels { ids: Vec<String> },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Quarantine,
    Reject,
    Flag,
    Timeout { seconds: u32 },
}
impl Policy {
    pub fn validate(&self) -> Result<(), AppError> {
        let bad = || {
            AppError::BadRequest("invalid automod policy: check rule IDs, thresholds, scopes, actions and retention limits".into())
        };
        if self.rules.len() > 32
            || self.retention_days == 0
            || self.retention_days > 90
            || self.exempt_roles.len() > 100
            || self.exempt_permissions.len() > 32
        {
            return Err(bad());
        }
        let mut ids = std::collections::HashSet::new();
        for rule in &self.rules {
            if rule.id.is_empty() || rule.id.len() > 64 || !ids.insert(&rule.id) {
                return Err(bad());
            }
            if let Scope::Channels { ids } = &rule.scope {
                if ids.is_empty() || ids.len() > 100 {
                    return Err(bad());
                }
            }
            if let Trigger::Media {
                categories,
                threshold,
            } = &rule.trigger
            {
                if !threshold.is_finite()
                    || !(0.0..=1.0).contains(threshold)
                    || categories.is_empty()
                    || categories.len() > 64
                    || categories.iter().any(|s| s.is_empty() || s.len() > 64)
                    || matches!(rule.action, Action::Timeout { .. })
                {
                    return Err(bad());
                }
            }
            if let Action::Timeout { seconds } = rule.action {
                if seconds == 0
                    || seconds > 86400
                    || !matches!(
                        rule.trigger,
                        Trigger::HashDenylist | Trigger::LowTrust { .. }
                    )
                {
                    return Err(bad());
                }
            }
        }
        Ok(())
    }
}
impl Policy {
    /// Whether an enabled hash-denylist rule covers `channel`. When one does,
    /// a blocked digest is handled by that rule (quarantine/timeout) rather than
    /// rejected outright at admission.
    pub fn has_hash_rule(&self, channel: &ChannelRow) -> bool {
        self.rules
            .iter()
            .any(|r| matches!(r.trigger, Trigger::HashDenylist) && r.applies(channel))
    }
}
impl Rule {
    pub fn applies(&self, channel: &ChannelRow) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::NonNsfw => !channel.nsfw,
            Scope::Channels { ids } => ids.contains(&channel.id),
        }
    }
}
pub async fn load(state: &AppState, space: Option<&str>) -> Result<Policy, AppError> {
    let row: Option<(String,)> = sqlx::query_as(&db::q("SELECT policy FROM automod_policies WHERE scope_id = ? OR scope_id = '*' ORDER BY CASE WHEN scope_id = '*' THEN 1 ELSE 0 END LIMIT 1"))
        .bind(space.unwrap_or("*")).fetch_optional(&state.db).await?;
    row.map(|r| serde_json::from_str(&r.0).map_err(|e| AppError::Internal(e.to_string())))
        .unwrap_or_else(|| Ok(Policy::default()))
}
pub async fn exempt(
    state: &AppState,
    policy: &Policy,
    channel: &ChannelRow,
    auth: &AuthUser,
) -> Result<bool, AppError> {
    if let Some(space) = &channel.space_id {
        let perms = permissions::resolve_member_permissions_with_admin(
            &state.db,
            space,
            &auth.user_id,
            auth.is_admin,
        )
        .await?;
        let roles = db::members::get_member_role_ids(&state.db, space, &auth.user_id).await?;
        return Ok(policy
            .exempt_permissions
            .iter()
            .any(|p| has_permission(&perms, p))
            || policy.exempt_roles.iter().any(|r| roles.contains(r)));
    }
    Ok(false)
}
fn age_hours(value: &str) -> i64 {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map(|d| d.naive_utc())
        .ok()
        .or_else(|| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok())
        .or_else(|| chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").ok());
    parsed
        .map(|d| (chrono::Utc::now().naive_utc() - d).num_hours().max(0))
        .unwrap_or(0)
}
pub async fn non_media_match(
    state: &AppState,
    rule: &Rule,
    channel: &ChannelRow,
    author: &str,
    hash: &str,
) -> Result<bool, AppError> {
    if !rule.applies(channel) {
        return Ok(false);
    }
    Ok(match &rule.trigger {
        Trigger::Media { .. } => false,
        Trigger::NonNsfwAttachment => !channel.nsfw,
        Trigger::HashDenylist => hash_blocked(state, channel, hash).await?,
        Trigger::LowTrust {
            min_account_age_hours,
            min_space_age_hours,
            require_role,
        } => {
            let user = db::users::get_user(&state.db, author).await?;
            let mut low = age_hours(&user.created_at) < i64::from(*min_account_age_hours);
            if let Some(space) = &channel.space_id {
                let member = db::members::get_member_row(&state.db, space, author).await?;
                low |= age_hours(&member.joined_at) < i64::from(*min_space_age_hours);
                low |= *require_role
                    && db::members::get_member_role_ids(&state.db, space, author)
                        .await?
                        .is_empty();
            }
            low
        }
    })
}

pub async fn hash_blocked(
    state: &AppState,
    channel: &ChannelRow,
    hash: &str,
) -> Result<bool, AppError> {
    hash_blocked_in_scope(state, channel.space_id.as_deref(), hash).await
}

/// Whether `hash` is blocked instance-wide or in `space_id`.
pub async fn hash_blocked_in_scope(
    state: &AppState,
    space_id: Option<&str>,
    hash: &str,
) -> Result<bool, AppError> {
    let (n,): (i64,) = sqlx::query_as(&db::q(
        "SELECT COUNT(*) FROM automod_hashes WHERE hash=? AND (scope_id='*' OR scope_id=?)",
    ))
    .bind(hash)
    .bind(super::scope_key(space_id))
    .fetch_one(&state.db)
    .await?;
    Ok(n > 0)
}
