use sqlx::AnyPool;

use crate::error::AppError;
use crate::snowflake;

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub id: String,
    pub space_id: String,
    pub reporter_id: String,
    pub target_type: String,
    pub target_id: String,
    pub channel_id: Option<String>,
    pub category: String,
    pub description: Option<String>,
    pub status: String,
    pub actioned_by: Option<String>,
    pub action_taken: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

pub async fn create_report(
    pool: &AnyPool,
    space_id: &str,
    reporter_id: &str,
    target_type: &str,
    target_id: &str,
    channel_id: Option<&str>,
    category: &str,
    description: Option<&str>,
) -> Result<ReportRow, AppError> {
    let id = snowflake::generate();
    sqlx::query(
        "INSERT INTO reports (id, space_id, reporter_id, target_type, target_id, channel_id, category, description) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(space_id)
    .bind(reporter_id)
    .bind(target_type)
    .bind(target_id)
    .bind(channel_id)
    .bind(category)
    .bind(description)
    .execute(pool)
    .await?;

    get_report(pool, &id).await
}

pub async fn get_report(pool: &AnyPool, report_id: &str) -> Result<ReportRow, AppError> {
    let row = sqlx::query_as::<_, (String, String, String, String, String, Option<String>, String, Option<String>, String, Option<String>, Option<String>, String, Option<String>)>(
        "SELECT id, space_id, reporter_id, target_type, target_id, channel_id, category, description, status, actioned_by, action_taken, created_at, resolved_at FROM reports WHERE id = ?"
    )
    .bind(report_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("report not found".to_string()))?;

    Ok(ReportRow {
        id: row.0,
        space_id: row.1,
        reporter_id: row.2,
        target_type: row.3,
        target_id: row.4,
        channel_id: row.5,
        category: row.6,
        description: row.7,
        status: row.8,
        actioned_by: row.9,
        action_taken: row.10,
        created_at: row.11,
        resolved_at: row.12,
    })
}

pub async fn list_reports(
    pool: &AnyPool,
    space_id: &str,
    status_filter: Option<&str>,
    limit: i64,
    before: Option<&str>,
) -> Result<Vec<ReportRow>, AppError> {
    let mut query = String::from("SELECT id, space_id, reporter_id, target_type, target_id, channel_id, category, description, status, actioned_by, action_taken, created_at, resolved_at FROM reports WHERE space_id = ?");

    if status_filter.is_some() {
        query.push_str(" AND status = ?");
    }
    if before.is_some() {
        query.push_str(" AND id < ?");
    }
    query.push_str(" ORDER BY created_at DESC LIMIT ?");

    let mut q = sqlx::query_as::<_, (String, String, String, String, String, Option<String>, String, Option<String>, String, Option<String>, Option<String>, String, Option<String>)>(&query)
        .bind(space_id);

    if let Some(s) = status_filter {
        q = q.bind(s);
    }
    if let Some(b) = before {
        q = q.bind(b);
    }
    q = q.bind(limit);

    let rows = q.fetch_all(pool).await?;

    Ok(rows
        .into_iter()
        .map(|row| ReportRow {
            id: row.0,
            space_id: row.1,
            reporter_id: row.2,
            target_type: row.3,
            target_id: row.4,
            channel_id: row.5,
            category: row.6,
            description: row.7,
            status: row.8,
            actioned_by: row.9,
            action_taken: row.10,
            created_at: row.11,
            resolved_at: row.12,
        })
        .collect())
}

pub async fn resolve_report(
    pool: &AnyPool,
    report_id: &str,
    actioned_by: &str,
    status: &str,
    action_taken: Option<&str>,
    is_postgres: bool,
) -> Result<ReportRow, AppError> {
    let now_fn = if is_postgres { "NOW()" } else { "datetime('now')" };
    let sql = format!(
        "UPDATE reports SET status = ?, actioned_by = ?, action_taken = ?, resolved_at = {now_fn} WHERE id = ?"
    );
    sqlx::query(&sql)
        .bind(status)
        .bind(actioned_by)
        .bind(action_taken)
        .bind(report_id)
        .execute(pool)
        .await?;

    get_report(pool, report_id).await
}
