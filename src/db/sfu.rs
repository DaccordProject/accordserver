use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::state::SfuNode;

fn row_to_sfu_node(row: sqlx::sqlite::SqliteRow) -> SfuNode {
    SfuNode {
        id: row.get("id"),
        endpoint: row.get("endpoint"),
        region: row.get("region"),
        capacity: row.get("capacity"),
        current_load: row.get("current_load"),
        status: row.get("status"),
    }
}

pub async fn upsert_node(
    pool: &SqlitePool,
    id: &str,
    endpoint: &str,
    region: &str,
    capacity: i64,
) -> Result<SfuNode, AppError> {
    sqlx::query(
        "INSERT INTO sfu_nodes (id, endpoint, region, capacity, current_load, status, last_heartbeat)
         VALUES (?, ?, ?, ?, 0, 'online', datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
           endpoint = excluded.endpoint,
           region = excluded.region,
           capacity = excluded.capacity,
           status = 'online',
           last_heartbeat = datetime('now'),
           updated_at = datetime('now')",
    )
    .bind(id)
    .bind(endpoint)
    .bind(region)
    .bind(capacity)
    .execute(pool)
    .await?;

    let row = sqlx::query(
        "SELECT id, endpoint, region, capacity, current_load, status FROM sfu_nodes WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    Ok(row_to_sfu_node(row))
}

pub async fn heartbeat_node(
    pool: &SqlitePool,
    node_id: &str,
    current_load: i64,
) -> Result<(), AppError> {
    let result = sqlx::query(
        "UPDATE sfu_nodes SET current_load = ?, last_heartbeat = datetime('now'), updated_at = datetime('now') WHERE id = ? AND status = 'online'",
    )
    .bind(current_load)
    .bind(node_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("sfu_node_not_found".to_string()));
    }

    Ok(())
}

pub async fn deregister_node(pool: &SqlitePool, node_id: &str) -> Result<(), AppError> {
    let result = sqlx::query(
        "UPDATE sfu_nodes SET status = 'offline', updated_at = datetime('now') WHERE id = ?",
    )
    .bind(node_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("sfu_node_not_found".to_string()));
    }

    Ok(())
}

pub async fn load_all_online(pool: &SqlitePool) -> Result<Vec<SfuNode>, AppError> {
    let rows = sqlx::query(
        "SELECT id, endpoint, region, capacity, current_load, status FROM sfu_nodes WHERE status = 'online'",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_sfu_node).collect())
}

pub async fn mark_stale_nodes_offline(
    pool: &SqlitePool,
    timeout_secs: i64,
) -> Result<Vec<String>, AppError> {
    let stale_rows = sqlx::query(
        "SELECT id FROM sfu_nodes WHERE status = 'online' AND last_heartbeat < datetime('now', ?)",
    )
    .bind(format!("-{timeout_secs} seconds"))
    .fetch_all(pool)
    .await?;

    let stale_ids: Vec<String> = stale_rows.iter().map(|r| r.get("id")).collect();

    if !stale_ids.is_empty() {
        sqlx::query(
            "UPDATE sfu_nodes SET status = 'offline', updated_at = datetime('now') WHERE status = 'online' AND last_heartbeat < datetime('now', ?)",
        )
        .bind(format!("-{timeout_secs} seconds"))
        .execute(pool)
        .await?;
    }

    Ok(stale_ids)
}
