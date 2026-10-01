use crate::{db::q, error::AppError};
use serde::{Deserialize, Serialize};
use sqlx::{AnyPool, Row};

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
}

impl Session {
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
    }
}

pub async fn load(pool: &AnyPool, space: &str, id: &str) -> Result<Session, AppError> {
    let row = sqlx::query(&q(
        "SELECT session_json FROM experience_sessions WHERE space_id = ? AND id = ?",
    ))
    .bind(space)
    .bind(id)
    .fetch_one(pool)
    .await?;
    serde_json::from_str(row.get("session_json"))
        .map_err(|_| AppError::Internal("Invalid stored session".into()))
}

pub async fn list(pool: &AnyPool, space: &str) -> Result<Vec<Session>, AppError> {
    let cutoff = chrono::Utc::now().timestamp() - 30 * 86400;
    let rows = sqlx::query(&q("SELECT session_json FROM experience_sessions WHERE space_id = ? AND updated_at > ? ORDER BY updated_at DESC LIMIT 128")).bind(space).bind(cutoff).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            serde_json::from_str(r.get("session_json"))
                .map_err(|_| AppError::Internal("Invalid session".into()))
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
    let result = sqlx::query(&q("INSERT INTO experience_sessions (id, space_id, game_id, revision, session_json, updated_at, state, deadline) SELECT ?, ?, ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM space_experiences e WHERE e.space_id = ? AND e.game_id = ? AND e.enabled = 1 AND e.generation = ?) AND NOT EXISTS (SELECT 1 FROM space_arcades a WHERE a.space_id = ? AND a.enabled = 0) AND (SELECT COUNT(*) FROM experience_sessions s WHERE s.space_id = ? AND s.state != 'ended') < 32"))
        .bind(&session.id).bind(&session.space_id).bind(&session.game_id).bind(session.revision).bind(serde_json::to_string(session).unwrap()).bind(session.updated_at).bind(&session.state).bind(session.deadline).bind(&session.space_id).bind(&session.game_id).bind(session.installation_generation).bind(&session.space_id).bind(&session.space_id).execute(&mut *tx).await?;
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
pub async fn save(pool: &AnyPool, session: &mut Session) -> Result<(), AppError> {
    let previous = session.revision;
    session.revision += 1;
    session.updated_at = chrono::Utc::now().timestamp();
    let result = sqlx::query(&q("UPDATE experience_sessions SET revision = ?, session_json = ?, updated_at = ?, state = ?, deadline = ? WHERE id = ? AND space_id = ? AND revision = ? AND (? = 1 OR (EXISTS (SELECT 1 FROM space_experiences e WHERE e.space_id = ? AND e.game_id = ? AND e.enabled = 1 AND e.generation = ?) AND NOT EXISTS (SELECT 1 FROM space_arcades a WHERE a.space_id = ? AND a.enabled = 0)))"))
        .bind(session.revision).bind(serde_json::to_string(session).unwrap()).bind(session.updated_at).bind(&session.state).bind(session.deadline).bind(&session.id).bind(&session.space_id).bind(previous).bind(if session.state == "ended" {1i64} else {0}).bind(&session.space_id).bind(&session.game_id).bind(session.installation_generation).bind(&session.space_id).execute(pool).await?;
    if result.rows_affected() == 0 {
        return Err(AppError::SessionChanged);
    }
    Ok(())
}
