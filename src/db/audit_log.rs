use sqlx::AnyPool;

use crate::error::AppError;
use crate::snowflake;

#[derive(Debug, Clone)]
pub struct AuditLogRow {
    pub id: String,
    pub space_id: String,
    pub user_id: String,
    pub action_type: String,
    pub target_id: Option<String>,
    pub target_type: Option<String>,
    pub reason: Option<String>,
    pub changes: Option<String>,
    pub created_at: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_entry(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    action_type: &str,
    target_id: Option<&str>,
    target_type: Option<&str>,
    reason: Option<&str>,
    changes: Option<&str>,
) -> Result<AuditLogRow, AppError> {
    let id = snowflake::generate();
    sqlx::query(
        &super::q("INSERT INTO audit_log (id, space_id, user_id, action_type, target_id, target_type, reason, changes) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"),
    )
    .bind(&id)
    .bind(space_id)
    .bind(user_id)
    .bind(action_type)
    .bind(target_id)
    .bind(target_type)
    .bind(reason)
    .bind(changes)
    .execute(pool)
    .await?;

    // Return the row we just inserted
    let row = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>, String)>(
        &super::q("SELECT id, space_id, user_id, action_type, target_id, target_type, reason, changes, created_at FROM audit_log WHERE id = ?"),
    )
    .bind(&id)
    .fetch_one(pool)
    .await?;

    Ok(AuditLogRow {
        id: row.0,
        space_id: row.1,
        user_id: row.2,
        action_type: row.3,
        target_id: row.4,
        target_type: row.5,
        reason: row.6,
        changes: row.7,
        created_at: row.8,
    })
}

pub async fn list_entries(
    pool: &AnyPool,
    space_id: &str,
    action_type: Option<&str>,
    user_id: Option<&str>,
    before: Option<&str>,
    limit: i64,
) -> Result<Vec<AuditLogRow>, AppError> {
    let mut query = String::from("SELECT id, space_id, user_id, action_type, target_id, target_type, reason, changes, created_at FROM audit_log WHERE space_id = ?");

    if action_type.is_some() {
        query.push_str(" AND action_type = ?");
    }
    if user_id.is_some() {
        query.push_str(" AND user_id = ?");
    }
    if before.is_some() {
        query.push_str(" AND id < ?");
    }
    query.push_str(" ORDER BY id DESC LIMIT ?");

    let query = super::q(&query);
    let mut q = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ),
    >(&query)
    .bind(space_id);

    if let Some(at) = action_type {
        q = q.bind(at);
    }
    if let Some(uid) = user_id {
        q = q.bind(uid);
    }
    if let Some(b) = before {
        q = q.bind(b);
    }
    q = q.bind(limit);

    let rows = q.fetch_all(pool).await?;

    Ok(rows
        .into_iter()
        .map(|row| AuditLogRow {
            id: row.0,
            space_id: row.1,
            user_id: row.2,
            action_type: row.3,
            target_id: row.4,
            target_type: row.5,
            reason: row.6,
            changes: row.7,
            created_at: row.8,
        })
        .collect())
}
