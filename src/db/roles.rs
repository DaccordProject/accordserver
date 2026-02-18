use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::role::{CreateRole, RoleRow, UpdateRole};
use crate::snowflake;

pub async fn get_role_row(pool: &SqlitePool, role_id: &str) -> Result<RoleRow, AppError> {
    let row = sqlx::query_as::<_, (String, String, String, i64, bool, Option<String>, i64, String, bool, bool)>(
        "SELECT id, space_id, name, color, hoist, icon, position, permissions, managed, mentionable FROM roles WHERE id = ?"
    )
    .bind(role_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown_role".to_string()))?;

    Ok(RoleRow {
        id: row.0,
        space_id: row.1,
        name: row.2,
        color: row.3,
        hoist: row.4,
        icon: row.5,
        position: row.6,
        permissions: row.7,
        managed: row.8,
        mentionable: row.9,
    })
}

pub async fn list_roles(pool: &SqlitePool, space_id: &str) -> Result<Vec<RoleRow>, AppError> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, bool, Option<String>, i64, String, bool, bool)>(
        "SELECT id, space_id, name, color, hoist, icon, position, permissions, managed, mentionable FROM roles WHERE space_id = ? ORDER BY position"
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| RoleRow {
            id: row.0,
            space_id: row.1,
            name: row.2,
            color: row.3,
            hoist: row.4,
            icon: row.5,
            position: row.6,
            permissions: row.7,
            managed: row.8,
            mentionable: row.9,
        })
        .collect())
}

pub async fn create_role(
    pool: &SqlitePool,
    space_id: &str,
    input: &CreateRole,
) -> Result<RoleRow, AppError> {
    let id = snowflake::generate();
    let permissions = serde_json::to_string(&input.permissions.as_deref().unwrap_or(&[])).unwrap();

    // Get max position
    let max_pos: Option<i64> =
        sqlx::query_scalar("SELECT MAX(position) FROM roles WHERE space_id = ?")
            .bind(space_id)
            .fetch_one(pool)
            .await?;
    let position = max_pos.unwrap_or(0) + 1;

    sqlx::query(
        "INSERT INTO roles (id, space_id, name, color, hoist, permissions, mentionable, position) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(space_id)
    .bind(&input.name)
    .bind(input.color.unwrap_or(0))
    .bind(input.hoist.unwrap_or(false))
    .bind(&permissions)
    .bind(input.mentionable.unwrap_or(false))
    .bind(position)
    .execute(pool)
    .await?;

    get_role_row(pool, &id).await
}

pub async fn update_role(
    pool: &SqlitePool,
    role_id: &str,
    input: &UpdateRole,
) -> Result<RoleRow, AppError> {
    let mut sets: Vec<String> = Vec::new();
    let mut str_values: Vec<String> = Vec::new();
    let mut int_vals: Vec<(String, i64)> = Vec::new();

    if let Some(ref name) = input.name {
        sets.push("name = ?".to_string());
        str_values.push(name.clone());
    }
    if let Some(ref icon) = input.icon {
        sets.push("icon = ?".to_string());
        str_values.push(icon.clone());
    }
    if let Some(ref permissions) = input.permissions {
        let json = serde_json::to_string(permissions).unwrap();
        sets.push("permissions = ?".to_string());
        str_values.push(json);
    }

    if let Some(color) = input.color {
        int_vals.push(("color".to_string(), color));
    }
    if let Some(hoist) = input.hoist {
        int_vals.push(("hoist".to_string(), hoist as i64));
    }
    if let Some(position) = input.position {
        int_vals.push(("position".to_string(), position));
    }
    if let Some(mentionable) = input.mentionable {
        int_vals.push(("mentionable".to_string(), mentionable as i64));
    }

    for (col, _) in &int_vals {
        sets.push(format!("{col} = ?"));
    }

    if sets.is_empty() {
        return get_role_row(pool, role_id).await;
    }

    sets.push("updated_at = datetime('now')".to_string());
    let set_clause = sets.join(", ");
    let query = format!("UPDATE roles SET {set_clause} WHERE id = ?");
    let mut q = sqlx::query(&query);
    for v in &str_values {
        q = q.bind(v);
    }
    for (_, val) in &int_vals {
        q = q.bind(val);
    }
    q = q.bind(role_id);
    q.execute(pool).await?;

    get_role_row(pool, role_id).await
}

pub async fn delete_role(pool: &SqlitePool, role_id: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM roles WHERE id = ?")
        .bind(role_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reorder_roles(pool: &SqlitePool, updates: &[(String, i64)]) -> Result<(), AppError> {
    for (id, position) in updates {
        sqlx::query("UPDATE roles SET position = ? WHERE id = ?")
            .bind(position)
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}
