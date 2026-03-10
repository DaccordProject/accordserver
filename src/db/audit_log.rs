use sqlx::SqlitePool;

use crate::error::AppError;

pub struct AuditLogEntry {
    pub id: i64,
    pub space_id: Option<String>,
    pub actor_id: String,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub metadata: String,
    pub created_at: String,
}

pub async fn write(
    pool: &SqlitePool,
    space_id: Option<&str>,
    actor_id: &str,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    metadata: serde_json::Value,
) {
    let _ = sqlx::query(
        "INSERT INTO audit_log (space_id, actor_id, action, target_type, target_id, metadata) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(space_id)
    .bind(actor_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(metadata.to_string())
    .execute(pool)
    .await;
}

pub async fn list(
    pool: &SqlitePool,
    space_id: &str,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<AuditLogEntry>, AppError> {
    let rows = if let Some(before_id) = before {
        sqlx::query_as::<
            _,
            (
                i64,
                Option<String>,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
                String,
            ),
        >(
            "SELECT id, space_id, actor_id, action, target_type, target_id, metadata, created_at \
             FROM audit_log WHERE space_id = ? AND id < ? ORDER BY id DESC LIMIT ?",
        )
        .bind(space_id)
        .bind(before_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(AppError::from)?
    } else {
        sqlx::query_as::<
            _,
            (
                i64,
                Option<String>,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
                String,
            ),
        >(
            "SELECT id, space_id, actor_id, action, target_type, target_id, metadata, created_at \
             FROM audit_log WHERE space_id = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(space_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(AppError::from)?
    };

    Ok(rows
        .into_iter()
        .map(|r| AuditLogEntry {
            id: r.0,
            space_id: r.1,
            actor_id: r.2,
            action: r.3,
            target_type: r.4,
            target_id: r.5,
            metadata: r.6,
            created_at: r.7,
        })
        .collect())
}
