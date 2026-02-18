use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::models::member::{MemberRow, UpdateMember};

fn row_to_member(row: sqlx::sqlite::SqliteRow) -> MemberRow {
    MemberRow {
        user_id: row.get("user_id"),
        space_id: row.get("space_id"),
        nickname: row.get("nickname"),
        avatar: row.get("avatar"),
        joined_at: row.get("joined_at"),
        premium_since: row.get("premium_since"),
        deaf: row.get("deaf"),
        mute: row.get("mute"),
        pending: row.get("pending"),
        timed_out_until: row.get("timed_out_until"),
    }
}

const SELECT_MEMBERS: &str = "SELECT user_id, space_id, nickname, avatar, joined_at, premium_since, deaf, mute, pending, timed_out_until FROM members";

pub async fn get_member_row(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<MemberRow, AppError> {
    let row = sqlx::query(&format!(
        "{SELECT_MEMBERS} WHERE space_id = ? AND user_id = ?"
    ))
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("member not found".to_string()))?;

    Ok(row_to_member(row))
}

pub async fn list_members(
    pool: &SqlitePool,
    space_id: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<MemberRow>, AppError> {
    let rows = if let Some(after_id) = after {
        sqlx::query(&format!(
            "{SELECT_MEMBERS} WHERE space_id = ? AND user_id > ? ORDER BY user_id ASC LIMIT ?"
        ))
        .bind(space_id)
        .bind(after_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(&format!(
            "{SELECT_MEMBERS} WHERE space_id = ? ORDER BY user_id ASC LIMIT ?"
        ))
        .bind(space_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    };

    Ok(rows.into_iter().map(row_to_member).collect())
}

pub async fn search_members(
    pool: &SqlitePool,
    space_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<MemberRow>, AppError> {
    let pattern = format!("%{query}%");
    let rows = sqlx::query(
        "SELECT m.user_id, m.space_id, m.nickname, m.avatar, m.joined_at, m.premium_since, m.deaf, m.mute, m.pending, m.timed_out_until FROM members m INNER JOIN users u ON m.user_id = u.id WHERE m.space_id = ? AND (u.username LIKE ? OR m.nickname LIKE ?) LIMIT ?"
    )
    .bind(space_id)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_member).collect())
}

pub async fn add_member(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<MemberRow, AppError> {
    sqlx::query("INSERT OR IGNORE INTO members (user_id, space_id) VALUES (?, ?)")
        .bind(user_id)
        .bind(space_id)
        .execute(pool)
        .await?;

    get_member_row(pool, space_id, user_id).await
}

pub async fn remove_member(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM members WHERE space_id = ? AND user_id = ?")
        .bind(space_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_member(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    input: &UpdateMember,
) -> Result<MemberRow, AppError> {
    if let Some(ref nickname) = input.nickname {
        sqlx::query("UPDATE members SET nickname = ? WHERE space_id = ? AND user_id = ?")
            .bind(nickname)
            .bind(space_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    }

    if let Some(mute) = input.mute {
        sqlx::query("UPDATE members SET mute = ? WHERE space_id = ? AND user_id = ?")
            .bind(mute)
            .bind(space_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    }
    if let Some(deaf) = input.deaf {
        sqlx::query("UPDATE members SET deaf = ? WHERE space_id = ? AND user_id = ?")
            .bind(deaf)
            .bind(space_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    }

    // Handle role updates
    if let Some(ref roles) = input.roles {
        sqlx::query("DELETE FROM member_roles WHERE user_id = ? AND space_id = ?")
            .bind(user_id)
            .bind(space_id)
            .execute(pool)
            .await?;
        for role_id in roles {
            sqlx::query("INSERT INTO member_roles (user_id, space_id, role_id) VALUES (?, ?, ?)")
                .bind(user_id)
                .bind(space_id)
                .bind(role_id)
                .execute(pool)
                .await?;
        }
    }

    get_member_row(pool, space_id, user_id).await
}

pub async fn get_member_role_ids(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query_as::<_, (String,)>(
        "SELECT role_id FROM member_roles WHERE user_id = ? AND space_id = ?",
    )
    .bind(user_id)
    .bind(space_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn add_role_to_member(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    role_id: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT OR IGNORE INTO member_roles (user_id, space_id, role_id) VALUES (?, ?, ?)")
        .bind(user_id)
        .bind(space_id)
        .bind(role_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn remove_role_from_member(
    pool: &SqlitePool,
    space_id: &str,
    user_id: &str,
    role_id: &str,
) -> Result<(), AppError> {
    sqlx::query("DELETE FROM member_roles WHERE user_id = ? AND space_id = ? AND role_id = ?")
        .bind(user_id)
        .bind(space_id)
        .bind(role_id)
        .execute(pool)
        .await?;
    Ok(())
}
