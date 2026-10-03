use crate::{db::q, error::AppError};
use serde::{Deserialize, Serialize};
use sqlx::{Any, AnyPool, Executor, Row, Transaction};

pub const IDLE_TIMEOUT_SECONDS: i64 = 7 * 86400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub user_id: String,
    pub role: String,
    pub slot: Option<usize>,
    pub ready: bool,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub space_id: String,
    pub game_id: String,
    pub version: String,
    pub digest: String,
    pub installation_generation: i64,
    pub mode: String,
    pub state: String,
    pub host_user_id: String,
    pub invite_only: bool,
    pub invited: Vec<String>,
    pub participants: Vec<Participant>,
    pub revision: i64,
    pub game: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub turn_user_id: Option<String>,
    pub turn_timeout_seconds: i64,
    pub deadline: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub last_activity_at: i64,
    #[serde(default)]
    pub idle_expires_at: Option<i64>,
}

impl Session {
    pub fn normalize_activity(&mut self) {
        if self.last_activity_at == 0 {
            self.last_activity_at = self.updated_at;
        }
        self.idle_expires_at =
            (self.state != "ended").then_some(self.last_activity_at + IDLE_TIMEOUT_SECONDS);
    }
    pub fn record_player_activity(&mut self) {
        self.last_activity_at = chrono::Utc::now().timestamp();
        self.normalize_activity();
    }
    pub fn idle_expired(&self, now: i64) -> bool {
        self.state != "ended" && now >= self.last_activity_at + IDLE_TIMEOUT_SECONDS
    }
    pub fn can_observe(&self, user: &str) -> bool {
        !self.invite_only
            || self.invited.iter().any(|u| u == user)
            || self.participants.iter().any(|p| p.user_id == user)
    }
    pub fn end(&mut self, reason: &str, winner: Option<String>) {
        self.state = "ended".into();
        self.result = Some(
            serde_json::json!({"reason": reason, "winner_user_id": winner, "outcome": if winner.is_some() {"win"} else if ["draw", "move_limit_draw"].contains(&reason) {"draw"} else {"cancelled"}}),
        );
        self.turn_user_id = None;
        self.deadline = None;
        self.idle_expires_at = None;
    }
}

pub async fn load<'e, E>(executor: E, space: &str, id: &str) -> Result<Session, AppError>
where
    E: Executor<'e, Database = Any>,
{
    let row = sqlx::query(&q(
        "SELECT session_json FROM experience_sessions WHERE space_id = ? AND id = ?",
    ))
    .bind(space)
    .bind(id)
    .fetch_one(executor)
    .await?;
    let mut session: Session = serde_json::from_str(row.get("session_json"))
        .map_err(|_| AppError::Internal("Invalid stored session".into()))?;
    session.normalize_activity();
    Ok(session)
}

/// Reserve a live lifecycle write before loading its latest snapshot. This
/// no-op write locks the row on PostgreSQL and the writer on SQLite, including
/// ticks from other server processes. Keep remote approval work outside it.
pub async fn lock_live<'a>(
    pool: &'a AnyPool,
    space: &str,
    id: &str,
) -> Result<Transaction<'a, Any>, AppError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query(&q(
        "UPDATE experience_sessions SET revision = revision WHERE space_id = ? AND id = ?",
    ))
    .bind(space)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Session not found".into()));
    }
    Ok(tx)
}

pub async fn list(pool: &AnyPool, space: &str) -> Result<Vec<Session>, AppError> {
    let cutoff = chrono::Utc::now().timestamp() - 30 * 86400;
    let rows = sqlx::query(&q("SELECT session_json FROM experience_sessions WHERE space_id = ? AND (state != 'ended' OR updated_at > ?) ORDER BY CASE WHEN state = 'ended' THEN 1 ELSE 0 END, updated_at DESC LIMIT 128")).bind(space).bind(cutoff).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            let mut session: Session = serde_json::from_str(r.get("session_json"))
                .map_err(|_| AppError::Internal("Invalid session".into()))?;
            session.normalize_activity();
            Ok(session)
        })
        .collect()
}

pub async fn insert(pool: &AnyPool, session: &Session) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    if crate::db::is_pg() {
        // Serialize capacity checks across concurrent PostgreSQL connections.
        // SQLite serializes the INSERT SELECT as a write automatically.
        sqlx::query(&q("SELECT id FROM spaces WHERE id = ? FOR UPDATE"))
            .bind(&session.space_id)
            .fetch_one(&mut *tx)
            .await?;
    }
    let result = sqlx::query(&q("INSERT INTO experience_sessions (id, space_id, game_id, revision, session_json, updated_at, state, deadline, last_activity_at) SELECT ?, ?, ?, ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM space_experiences e WHERE e.space_id = ? AND e.game_id = ? AND e.enabled = 1 AND e.generation = ?) AND NOT EXISTS (SELECT 1 FROM space_arcades a WHERE a.space_id = ? AND a.enabled = 0) AND (SELECT COUNT(*) FROM experience_sessions s WHERE s.space_id = ? AND s.state != 'ended') < 32"))
        .bind(&session.id).bind(&session.space_id).bind(&session.game_id).bind(session.revision).bind(serde_json::to_string(session).unwrap()).bind(session.updated_at).bind(&session.state).bind(session.deadline).bind(session.last_activity_at).bind(&session.space_id).bind(&session.game_id).bind(session.installation_generation).bind(&session.space_id).bind(&session.space_id).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::Conflict(
            "Experience changed during session creation".into(),
        ));
    }
    tx.commit().await?;
    Ok(())
}

/// Compare-and-swap serializes moves, ready changes and host transfer across
/// concurrent requests without relying on process-local mutexes.
pub async fn save<'e, E>(executor: E, session: &mut Session) -> Result<(), AppError>
where
    E: Executor<'e, Database = Any>,
{
    let previous = session.revision;
    session.normalize_activity();
    session.revision += 1;
    session.updated_at = chrono::Utc::now().timestamp();
    let result = sqlx::query(&q("UPDATE experience_sessions SET revision = ?, session_json = ?, updated_at = ?, state = ?, deadline = ?, last_activity_at = ? WHERE id = ? AND space_id = ? AND revision = ? AND (? = 1 OR (EXISTS (SELECT 1 FROM space_experiences e WHERE e.space_id = ? AND e.game_id = ? AND e.enabled = 1 AND e.generation = ?) AND NOT EXISTS (SELECT 1 FROM space_arcades a WHERE a.space_id = ? AND a.enabled = 0)))"))
        .bind(session.revision).bind(serde_json::to_string(session).unwrap()).bind(session.updated_at).bind(&session.state).bind(session.deadline).bind(session.last_activity_at).bind(&session.id).bind(&session.space_id).bind(previous).bind(if session.state == "ended" {1i64} else {0}).bind(&session.space_id).bind(&session.game_id).bind(session.installation_generation).bind(&session.space_id).execute(executor).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::SessionChanged);
    }
    Ok(())
}
