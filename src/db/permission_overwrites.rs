use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::permission::PermissionOverwrite;

pub async fn list_overwrites(
    pool: &SqlitePool,
    channel_id: &str,
) -> Result<Vec<PermissionOverwrite>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, type, allow, deny FROM permission_overwrites WHERE channel_id = ?",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, overwrite_type, allow, deny)| PermissionOverwrite {
            id,
            overwrite_type,
            allow: serde_json::from_str(&allow).unwrap_or_default(),
            deny: serde_json::from_str(&deny).unwrap_or_default(),
        })
        .collect())
}

pub async fn upsert_overwrite(
    pool: &SqlitePool,
    channel_id: &str,
    overwrite: &PermissionOverwrite,
) -> Result<(), AppError> {
    let allow_json = serde_json::to_string(&overwrite.allow).unwrap();
    let deny_json = serde_json::to_string(&overwrite.deny).unwrap();

    sqlx::query(
        "INSERT INTO permission_overwrites (id, channel_id, type, allow, deny) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT (id, channel_id) DO UPDATE SET type = excluded.type, allow = excluded.allow, deny = excluded.deny",
    )
    .bind(&overwrite.id)
    .bind(channel_id)
    .bind(&overwrite.overwrite_type)
    .bind(&allow_json)
    .bind(&deny_json)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn delete_overwrite(
    pool: &SqlitePool,
    channel_id: &str,
    overwrite_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM permission_overwrites WHERE id = ? AND channel_id = ?")
        .bind(overwrite_id)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(())
}
