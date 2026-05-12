use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::member::{MemberRow, UpdateMember};

fn row_to_member(row: sqlx::any::AnyRow) -> MemberRow {
    MemberRow {
        user_id: row.get("user_id"),
        space_id: row.get("space_id"),
        nickname: row.get("nickname"),
        avatar: row.get("avatar"),
        joined_at: row.get("joined_at"),
        premium_since: row.get("premium_since"),
        deaf: crate::db::get_bool(&row, "deaf"),
        mute: crate::db::get_bool(&row, "mute"),
        pending: crate::db::get_bool(&row, "pending"),
        timed_out_until: row.get("timed_out_until"),
    }
}

const SELECT_MEMBERS: &str = "SELECT user_id, space_id, nickname, avatar, joined_at, premium_since, deaf, mute, pending, timed_out_until FROM members";

pub async fn get_member_row(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
) -> Result<MemberRow, AppError> {
    let row = sqlx::query(&super::q(&format!(
        "{SELECT_MEMBERS} WHERE space_id = ? AND user_id = ?"
    )))
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("member not found".to_string()))?;

    Ok(row_to_member(row))
}

pub async fn list_members(
    pool: &AnyPool,
    space_id: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<MemberRow>, AppError> {
    // Join users so we can hide the System user from the sidebar.
    let select = "SELECT m.user_id, m.space_id, m.nickname, m.avatar, m.joined_at, m.premium_since, m.deaf, m.mute, m.pending, m.timed_out_until FROM members m INNER JOIN users u ON m.user_id = u.id";
    let rows = if let Some(after_id) = after {
        sqlx::query(&super::q(&format!(
            "{select} WHERE m.space_id = ? AND u.system = FALSE AND m.user_id > ? ORDER BY m.user_id ASC LIMIT ?"
        )))
        .bind(space_id)
        .bind(after_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(&super::q(&format!(
            "{select} WHERE m.space_id = ? AND u.system = FALSE ORDER BY m.user_id ASC LIMIT ?"
        )))
        .bind(space_id)
        .bind(limit + 1)
        .fetch_all(pool)
        .await?
    };

    Ok(rows.into_iter().map(row_to_member).collect())
}

pub async fn search_members(
    pool: &AnyPool,
    space_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<MemberRow>, AppError> {
    let pattern = format!("%{query}%");
    let rows = sqlx::query(
        &super::q("SELECT m.user_id, m.space_id, m.nickname, m.avatar, m.joined_at, m.premium_since, m.deaf, m.mute, m.pending, m.timed_out_until FROM members m INNER JOIN users u ON m.user_id = u.id WHERE m.space_id = ? AND u.system = FALSE AND (u.username LIKE ? OR m.nickname LIKE ?) LIMIT ?")
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
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    is_postgres: bool,
) -> Result<MemberRow, AppError> {
    let sql = if is_postgres {
        "INSERT INTO members (user_id, space_id) VALUES (?, ?) ON CONFLICT DO NOTHING"
    } else {
        "INSERT OR IGNORE INTO members (user_id, space_id) VALUES (?, ?)"
    };
    sqlx::query(&super::q(sql))
        .bind(user_id)
        .bind(space_id)
        .execute(pool)
        .await?;

    get_member_row(pool, space_id, user_id).await
}

pub async fn remove_member(pool: &AnyPool, space_id: &str, user_id: &str) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "DELETE FROM members WHERE space_id = ? AND user_id = ?",
    ))
    .bind(space_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_member(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    input: &UpdateMember,
) -> Result<MemberRow, AppError> {
    if let Some(ref nickname) = input.nickname {
        sqlx::query(&super::q(
            "UPDATE members SET nickname = ? WHERE space_id = ? AND user_id = ?",
        ))
        .bind(nickname)
        .bind(space_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    }

    if let Some(ref avatar) = input.avatar {
        if avatar.is_empty() {
            sqlx::query(&super::q(
                "UPDATE members SET avatar = NULL WHERE space_id = ? AND user_id = ?",
            ))
            .bind(space_id)
            .bind(user_id)
            .execute(pool)
            .await?;
        } else {
            sqlx::query(&super::q(
                "UPDATE members SET avatar = ? WHERE space_id = ? AND user_id = ?",
            ))
            .bind(avatar)
            .bind(space_id)
            .bind(user_id)
            .execute(pool)
            .await?;
        }
    }

    if let Some(mute) = input.mute {
        sqlx::query(&super::q(
            "UPDATE members SET mute = ? WHERE space_id = ? AND user_id = ?",
        ))
        .bind(mute)
        .bind(space_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    }
    if let Some(deaf) = input.deaf {
        sqlx::query(&super::q(
            "UPDATE members SET deaf = ? WHERE space_id = ? AND user_id = ?",
        ))
        .bind(deaf)
        .bind(space_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    }

    // Handle role updates
    if let Some(ref roles) = input.roles {
        sqlx::query(&super::q(
            "DELETE FROM member_roles WHERE user_id = ? AND space_id = ?",
        ))
        .bind(user_id)
        .bind(space_id)
        .execute(pool)
        .await?;
        for role_id in roles {
            sqlx::query(&super::q(
                "INSERT INTO member_roles (user_id, space_id, role_id) VALUES (?, ?, ?)",
            ))
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
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
) -> Result<Vec<String>, AppError> {
    let rows = sqlx::query_as::<_, (String,)>(&super::q(
        "SELECT role_id FROM member_roles WHERE user_id = ? AND space_id = ?",
    ))
    .bind(user_id)
    .bind(space_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn add_role_to_member(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    role_id: &str,
    is_postgres: bool,
) -> Result<(), AppError> {
    let sql = if is_postgres {
        "INSERT INTO member_roles (user_id, space_id, role_id) VALUES (?, ?, ?) ON CONFLICT DO NOTHING"
    } else {
        "INSERT OR IGNORE INTO member_roles (user_id, space_id, role_id) VALUES (?, ?, ?)"
    };
    sqlx::query(&super::q(sql))
        .bind(user_id)
        .bind(space_id)
        .bind(role_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn remove_role_from_member(
    pool: &AnyPool,
    space_id: &str,
    user_id: &str,
    role_id: &str,
) -> Result<(), AppError> {
    sqlx::query(&super::q(
        "DELETE FROM member_roles WHERE user_id = ? AND space_id = ? AND role_id = ?",
    ))
    .bind(user_id)
    .bind(space_id)
    .bind(role_id)
    .execute(pool)
    .await?;
    Ok(())
}
