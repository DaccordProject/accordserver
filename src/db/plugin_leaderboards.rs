use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::plugin::LeaderboardRecord;
use crate::snowflake;

/// Upsert a leaderboard record, applying the operator logic.
///
/// `operator`: "set" | "best" | "increment"
/// `sort`: "ascending" | "descending"
pub async fn upsert_record(
    pool: &AnyPool,
    plugin_id: &str,
    space_id: &str,
    board_id: &str,
    user_id: &str,
    score: f64,
    metadata: Option<&serde_json::Value>,
    operator: &str,
    sort: &str,
    is_postgres: bool,
) -> Result<f64, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
    let metadata_str = metadata.map(|m| serde_json::to_string(m).unwrap_or_default());

    // Check for existing record
    let existing = sqlx::query(&super::q(
        "SELECT id, score FROM plugin_leaderboard_records \
         WHERE plugin_id = ? AND space_id = ? AND board_id = ? \
         AND user_id = ? AND period = 'current'",
    ))
    .bind(plugin_id)
    .bind(space_id)
    .bind(board_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    if let Some(row) = existing {
        let existing_id: String = row.get("id");
        let existing_score: f64 = crate::db::get_f64(&row, "score");

        let new_score = match operator {
            "set" => score,
            "increment" => existing_score + score,
            "best" => {
                let is_better = if sort == "ascending" {
                    score < existing_score
                } else {
                    score > existing_score
                };
                if is_better {
                    score
                } else {
                    return Ok(existing_score);
                }
            }
            _ => score,
        };

        let sql = format!(
            "UPDATE plugin_leaderboard_records \
             SET score = ?, metadata = ?, updated_at = {now_fn} \
             WHERE id = ?"
        );
        sqlx::query(&super::q(&sql))
            .bind(new_score)
            .bind(&metadata_str)
            .bind(&existing_id)
            .execute(pool)
            .await?;

        Ok(new_score)
    } else {
        let id = snowflake::generate();
        let sql = format!(
            "INSERT INTO plugin_leaderboard_records \
             (id, plugin_id, space_id, board_id, user_id, score, metadata, period, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'current', {now_fn})"
        );
        sqlx::query(&super::q(&sql))
            .bind(&id)
            .bind(plugin_id)
            .bind(space_id)
            .bind(board_id)
            .bind(user_id)
            .bind(score)
            .bind(&metadata_str)
            .execute(pool)
            .await?;

        Ok(score)
    }
}

/// Fetch the leaderboard sorted by score with rank.
pub async fn get_leaderboard(
    pool: &AnyPool,
    plugin_id: &str,
    space_id: &str,
    board_id: &str,
    limit: i64,
    sort: &str,
) -> Result<Vec<LeaderboardRecord>, AppError> {
    let order = if sort == "ascending" { "ASC" } else { "DESC" };
    let sql = format!(
        "SELECT lr.user_id, lr.score, lr.metadata, \
         COALESCE(u.display_name, u.username, lr.user_id) AS display_name \
         FROM plugin_leaderboard_records lr \
         LEFT JOIN users u ON u.id = lr.user_id \
         WHERE lr.plugin_id = ? AND lr.space_id = ? \
         AND lr.board_id = ? AND lr.period = 'current' \
         ORDER BY lr.score {order} \
         LIMIT ?"
    );
    let rows = sqlx::query(&super::q(&sql))
        .bind(plugin_id)
        .bind(space_id)
        .bind(board_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let metadata_str: Option<String> = row.get("metadata");
        let metadata = metadata_str
            .and_then(|s| serde_json::from_str(&s).ok());
        records.push(LeaderboardRecord {
            user_id: row.get("user_id"),
            display_name: row.get("display_name"),
            score: crate::db::get_f64(row, "score"),
            rank: (i + 1) as i64,
            metadata,
        });
    }
    Ok(records)
}

/// Fetch records around a specific user's rank.
pub async fn get_around(
    pool: &AnyPool,
    plugin_id: &str,
    space_id: &str,
    board_id: &str,
    user_id: &str,
    limit: i64,
    sort: &str,
) -> Result<Vec<LeaderboardRecord>, AppError> {
    let order = if sort == "ascending" { "ASC" } else { "DESC" };
    let comparator = if sort == "ascending" { "<=" } else { ">=" };

    // Get the user's score first
    let user_row = sqlx::query(&super::q(
        "SELECT score FROM plugin_leaderboard_records \
         WHERE plugin_id = ? AND space_id = ? AND board_id = ? \
         AND user_id = ? AND period = 'current'",
    ))
    .bind(plugin_id)
    .bind(space_id)
    .bind(board_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let user_score: f64 = match user_row {
        Some(row) => crate::db::get_f64(&row, "score"),
        None => return Ok(vec![]),
    };

    // Get the user's rank
    let rank_sql = format!(
        "SELECT COUNT(*) AS rank FROM plugin_leaderboard_records \
         WHERE plugin_id = ? AND space_id = ? AND board_id = ? \
         AND period = 'current' AND score {comparator} ?"
    );
    let rank_row = sqlx::query(&super::q(&rank_sql))
        .bind(plugin_id)
        .bind(space_id)
        .bind(board_id)
        .bind(user_score)
        .fetch_one(pool)
        .await?;
    let user_rank: i64 = rank_row.try_get::<i64, _>("rank").unwrap_or(1);

    // Calculate offset for surrounding records
    let half = limit / 2;
    let offset = (user_rank - 1 - half).max(0);

    let sql = format!(
        "SELECT lr.user_id, lr.score, lr.metadata, \
         COALESCE(u.display_name, u.username, lr.user_id) AS display_name \
         FROM plugin_leaderboard_records lr \
         LEFT JOIN users u ON u.id = lr.user_id \
         WHERE lr.plugin_id = ? AND lr.space_id = ? \
         AND lr.board_id = ? AND lr.period = 'current' \
         ORDER BY lr.score {order} \
         LIMIT ? OFFSET ?"
    );
    let rows = sqlx::query(&super::q(&sql))
        .bind(plugin_id)
        .bind(space_id)
        .bind(board_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

    let mut records = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let metadata_str: Option<String> = row.get("metadata");
        let metadata = metadata_str
            .and_then(|s| serde_json::from_str(&s).ok());
        records.push(LeaderboardRecord {
            user_id: row.get("user_id"),
            display_name: row.get("display_name"),
            score: crate::db::get_f64(row, "score"),
            rank: (offset + i as i64 + 1),
            metadata,
        });
    }
    Ok(records)
}

/// Fetch a single user's record with their rank.
pub async fn get_user_record(
    pool: &AnyPool,
    plugin_id: &str,
    space_id: &str,
    board_id: &str,
    user_id: &str,
    sort: &str,
) -> Result<Option<LeaderboardRecord>, AppError> {
    let comparator = if sort == "ascending" { "<=" } else { ">=" };

    let sql = super::q(
        "SELECT lr.score, lr.metadata, \
         COALESCE(u.display_name, u.username, lr.user_id) AS display_name \
         FROM plugin_leaderboard_records lr \
         LEFT JOIN users u ON u.id = lr.user_id \
         WHERE lr.plugin_id = ? AND lr.space_id = ? \
         AND lr.board_id = ? AND lr.user_id = ? AND lr.period = 'current'",
    );
    let row = match sqlx::query(&sql)
        .bind(plugin_id)
        .bind(space_id)
        .bind(board_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?
    {
        Some(r) => r,
        None => return Ok(None),
    };

    let score: f64 = crate::db::get_f64(&row, "score");

    // Compute rank
    let rank_sql = format!(
        "SELECT COUNT(*) AS rank FROM plugin_leaderboard_records \
         WHERE plugin_id = ? AND space_id = ? AND board_id = ? \
         AND period = 'current' AND score {comparator} ?"
    );
    let rank_row = sqlx::query(&super::q(&rank_sql))
        .bind(plugin_id)
        .bind(space_id)
        .bind(board_id)
        .bind(score)
        .fetch_one(pool)
        .await?;
    let rank: i64 = rank_row.try_get::<i64, _>("rank").unwrap_or(1);

    let metadata_str: Option<String> = row.get("metadata");
    let metadata = metadata_str
        .and_then(|s| serde_json::from_str(&s).ok());

    Ok(Some(LeaderboardRecord {
        user_id: user_id.to_string(),
        display_name: row.get("display_name"),
        score,
        rank,
        metadata,
    }))
}
