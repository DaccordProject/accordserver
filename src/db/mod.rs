pub mod admin;
pub mod attachments;
pub mod auth;
pub mod bans;
pub mod channels;
pub mod dm_participants;
pub mod emojis;
pub mod invites;
pub mod members;
pub mod messages;
pub mod mutes;
pub mod permission_overwrites;
pub mod read_states;
pub mod relationships;
pub mod reports;
pub mod roles;
pub mod settings;
pub mod soundboard;
pub mod spaces;
pub mod users;

use std::str::FromStr;
use std::sync::OnceLock;

use sqlx::any::AnyConnectOptions;
use sqlx::AnyPool;

// ---------------------------------------------------------------------------
// Global backend flag + placeholder rewriter
// ---------------------------------------------------------------------------

static DB_IS_POSTGRES: OnceLock<bool> = OnceLock::new();

/// Record the database backend once at startup (called from `create_pool`).
pub fn set_is_postgres(v: bool) {
    DB_IS_POSTGRES.set(v).ok();
}

/// Returns `true` when the runtime database is PostgreSQL.
pub fn is_pg() -> bool {
    DB_IS_POSTGRES.get().copied().unwrap_or(false)
}

/// Rewrite `?` placeholders to `$1, $2, …` when running on PostgreSQL.
/// On SQLite the string is returned as-is (as an owned copy — the allocation
/// cost is negligible compared to actual query I/O).
pub fn q(sql: &str) -> String {
    if !is_pg() {
        return sql.to_string();
    }
    let mut result = String::with_capacity(sql.len() + 16);
    let mut idx = 0u32;
    for ch in sql.chars() {
        if ch == '?' {
            idx += 1;
            result.push('$');
            result.push_str(&idx.to_string());
        } else {
            result.push(ch);
        }
    }
    result
}

/// Read a boolean column from an `AnyRow`.
///
/// The `Any` driver maps SQLite `INTEGER` columns to `BIGINT`, which cannot be
/// decoded directly as Rust `bool`.  This helper tries `bool` first (works for
/// Postgres) and falls back to reading an `i64` (works for SQLite).
pub fn get_bool(row: &sqlx::any::AnyRow, col: &str) -> bool {
    use sqlx::Row;
    row.try_get::<bool, _>(col)
        .unwrap_or_else(|_| row.get::<i64, _>(col) != 0)
}

/// Read a float column from an `AnyRow`.
///
/// PostgreSQL `REAL` is `float4` which decodes as `f32`, while SQLite `REAL`
/// is always 64-bit.  This helper tries `f64` first (works for `DOUBLE
/// PRECISION` / SQLite) and falls back to `f32` → `f64` (works for PG `REAL`).
pub fn get_f64(row: &sqlx::any::AnyRow, col: &str) -> f64 {
    use sqlx::Row;
    row.try_get::<f64, _>(col)
        .unwrap_or_else(|_| row.get::<f32, _>(col) as f64)
}

/// Returns the SQL expression for the current timestamp for the given backend.
///
/// Both SQLite and PostgreSQL return timestamps as TEXT in `'YYYY-MM-DD HH24:MI:SS'`
/// format so that `row.get::<String, _>()` works identically across backends.
pub fn now_sql(is_postgres: bool) -> &'static str {
    if is_postgres {
        "to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')"
    } else {
        "datetime('now')"
    }
}

/// Returns true if the database URL targets PostgreSQL.
pub fn url_is_postgres(database_url: &str) -> bool {
    database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")
}

pub async fn create_pool(database_url: &str) -> Result<AnyPool, sqlx::Error> {
    // Install both SQLite and Postgres drivers so AnyPool can pick at runtime.
    sqlx::any::install_default_drivers();

    let is_pg = url_is_postgres(database_url);
    set_is_postgres(is_pg);
    let connect_opts = AnyConnectOptions::from_str(database_url)?;

    // In-memory SQLite creates a separate database per connection, so restrict
    // to a single connection to keep schema and data visible across operations.
    let max_conns = if database_url.contains(":memory:") {
        1
    } else {
        5
    };
    let mut pool_opts = sqlx::any::AnyPoolOptions::new().max_connections(max_conns);

    // foreign_keys is a per-connection PRAGMA in SQLite — must be set on every
    // new connection, not just once after pool creation.
    if !is_pg {
        pool_opts = pool_opts.after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("PRAGMA foreign_keys=ON")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        });
    }

    let pool = pool_opts.connect_with(connect_opts).await?;

    // journal_mode=WAL is database-level (persists across connections), so once is fine.
    if !is_pg {
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await?;
    }

    // Run the correct migration set for this backend.
    if is_pg {
        sqlx::migrate!("./migrations/postgres").run(&pool).await?;
    } else {
        sqlx::migrate!("./migrations").run(&pool).await?;
    }

    Ok(pool)
}
