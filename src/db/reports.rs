use sqlx::AnyPool;

use crate::error::AppError;
use crate::snowflake;

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub id: String,
    /// `None` for a report that belongs to no space — a direct message, or a
    /// user reported from outside any space. Those are the instance operator's
    /// to action, not a space moderator's.
    pub space_id: Option<String>,
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

#[allow(clippy::too_many_arguments)]
pub async fn create_report(
    pool: &AnyPool,
    space_id: Option<&str>,
    reporter_id: &str,
    target_type: &str,
    target_id: &str,
    channel_id: Option<&str>,
    category: &str,
    description: Option<&str>,
) -> Result<ReportRow, AppError> {
    let mut conn = pool.acquire().await?;
    let id = create_report_in(
        &mut conn,
        space_id,
        reporter_id,
        target_type,
        target_id,
        channel_id,
        category,
        description,
    )
    .await?;
    drop(conn);
    get_report(pool, &id).await
}

/// Insert a report on an explicit connection (pass `&mut *tx` to commit it
/// with related changes) and return its ID. `category` must be one of
/// `routes::reports::REPORT_CATEGORIES`, which mirrors the CHECK constraint.
#[allow(clippy::too_many_arguments)]
pub async fn create_report_in(
    conn: &mut sqlx::AnyConnection,
    space_id: Option<&str>,
    reporter_id: &str,
    target_type: &str,
    target_id: &str,
    channel_id: Option<&str>,
    category: &str,
    description: Option<&str>,
) -> Result<String, AppError> {
    debug_assert!(
        crate::routes::reports::REPORT_CATEGORIES
            .iter()
            .any(|(key, _)| *key == category),
        "unknown report category {category}"
    );
    let id = snowflake::generate();
    sqlx::query(
        &super::q("INSERT INTO reports (id, space_id, reporter_id, target_type, target_id, channel_id, category, description) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"),
    )
    .bind(&id)
    .bind(space_id)
    .bind(reporter_id)
    .bind(target_type)
    .bind(target_id)
    .bind(channel_id)
    .bind(category)
    .bind(description)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

pub async fn get_report(pool: &AnyPool, report_id: &str) -> Result<ReportRow, AppError> {
    let row = sqlx::query_as::<_, (String, Option<String>, String, String, String, Option<String>, String, Option<String>, String, Option<String>, Option<String>, String, Option<String>)>(
        &super::q("SELECT id, space_id, reporter_id, target_type, target_id, channel_id, category, description, status, actioned_by, action_taken, created_at, resolved_at FROM reports WHERE id = ?")
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

    let query = super::q(&query);
    let mut q = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ),
    >(&query)
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

/// Which reports the instance-wide operator queue should return.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReportScope {
    /// Everything on the instance, space-scoped or not.
    All,
    /// Only reports with no space — the ones no space moderator can see.
    Direct,
    /// Only reports that belong to a space.
    Space,
}

/// Instance-wide report list for a server admin, newest first.
///
/// The per-space [`list_reports`] cannot serve this: a report filed from a DM
/// has no space, so it appears in no space's queue. Clients that aggregate
/// per-space queues (the admin panel fans out over the spaces it knows) would
/// never show it.
pub async fn list_all_reports(
    pool: &AnyPool,
    scope: ReportScope,
    status_filter: Option<&str>,
    limit: i64,
    before: Option<&str>,
) -> Result<Vec<ReportRow>, AppError> {
    let mut query = String::from("SELECT id, space_id, reporter_id, target_type, target_id, channel_id, category, description, status, actioned_by, action_taken, created_at, resolved_at FROM reports WHERE 1 = 1");

    match scope {
        ReportScope::All => {}
        ReportScope::Direct => query.push_str(" AND space_id IS NULL"),
        ReportScope::Space => query.push_str(" AND space_id IS NOT NULL"),
    }
    if status_filter.is_some() {
        query.push_str(" AND status = ?");
    }
    if before.is_some() {
        query.push_str(" AND id < ?");
    }
    query.push_str(" ORDER BY created_at DESC LIMIT ?");

    let query = super::q(&query);
    let mut q = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ),
    >(&query);

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
    let now_fn = crate::db::now_sql(is_postgres);
    let sql = format!(
        "UPDATE reports SET status = ?, actioned_by = ?, action_taken = ?, resolved_at = {now_fn} WHERE id = ?"
    );
    let sql = super::q(&sql);
    sqlx::query(&sql)
        .bind(status)
        .bind(actioned_by)
        .bind(action_taken)
        .bind(report_id)
        .execute(pool)
        .await?;

    get_report(pool, report_id).await
}
