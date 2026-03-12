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
use sqlx::Connection;

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

/// Validate that a PostgreSQL identifier contains only safe characters.
fn is_safe_pg_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Connect to the `postgres` maintenance database and ensure the target
/// database exists, is owned by the connecting user, and that the `public`
/// schema grants sufficient privileges for migrations and table creation.
///
/// This handles the common first-run case where the PostgreSQL server is
/// running but the application database has not been created yet (e.g. a
/// fresh Docker volume).  It also works around the PostgreSQL 15+ change
/// that revokes `CREATE` on the `public` schema from non-owner roles.
async fn ensure_pg_database_exists(database_url: &str) -> Result<(), sqlx::Error> {
    use sqlx::postgres::PgConnectOptions;
    use sqlx::Row;

    let opts = PgConnectOptions::from_str(database_url)?;
    let db_name = opts.get_database().unwrap_or("accord").to_owned();
    let user_name = opts.get_username().to_owned();

    tracing::info!("checking if postgres database `{db_name}` exists");

    if !is_safe_pg_identifier(&db_name) {
        tracing::error!("refusing to auto-create database with unsafe name: {db_name}");
        return Ok(());
    }

    // Connect to the default `postgres` maintenance database.
    let maint_opts = opts.clone().database("postgres");
    let mut conn = sqlx::postgres::PgConnection::connect_with(&maint_opts).await?;

    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(&db_name)
            .fetch_one(&mut conn)
            .await?;

    if !exists {
        tracing::info!("creating postgres database `{db_name}`");
        if is_safe_pg_identifier(&user_name) {
            sqlx::query(&format!(
                "CREATE DATABASE \"{db_name}\" OWNER \"{user_name}\""
            ))
            .execute(&mut conn)
            .await?;
        } else {
            sqlx::query(&format!("CREATE DATABASE \"{db_name}\""))
                .execute(&mut conn)
                .await?;
        }
        tracing::info!("postgres database `{db_name}` created");
    }

    drop(conn);

    // Connect to the target database and ensure the public schema is usable.
    // On PostgreSQL 15+, CREATE on the public schema is revoked from PUBLIC,
    // so non-superuser roles may not be able to run migrations without this.
    let target_opts = opts.database(&db_name);
    let mut target_conn = sqlx::postgres::PgConnection::connect_with(&target_opts).await?;

    // Check if we have CREATE privilege on the public schema.
    let has_create: bool =
        sqlx::query_scalar("SELECT has_schema_privilege(current_user, 'public', 'CREATE')")
            .fetch_one(&mut target_conn)
            .await?;

    if !has_create {
        tracing::info!("granting CREATE on schema public to `{user_name}`");
        // This requires the connecting user to be the database owner or a
        // superuser.  If it fails, we log the error but let the caller
        // surface the real migration failure with a better message.
        let grant_sql = if is_safe_pg_identifier(&user_name) {
            format!("GRANT ALL ON SCHEMA public TO \"{user_name}\"")
        } else {
            "GRANT ALL ON SCHEMA public TO PUBLIC".to_string()
        };
        if let Err(e) = sqlx::query(&grant_sql).execute(&mut target_conn).await {
            tracing::warn!(
                "could not grant schema privileges (you may need to run as the database owner or superuser): {e}"
            );
        }
    }

    // Verify the current user owns the database (informational).
    let owner_row = sqlx::query(
        "SELECT pg_catalog.pg_get_userbyid(d.datdba) AS owner \
         FROM pg_catalog.pg_database d WHERE d.datname = current_database()",
    )
    .fetch_optional(&mut target_conn)
    .await?;

    if let Some(row) = owner_row {
        let owner: String = row.get("owner");
        if owner != user_name {
            tracing::warn!(
                "database `{db_name}` is owned by `{owner}`, not `{user_name}` — \
                 migrations may fail if schema privileges are insufficient"
            );
        }
    }

    Ok(())
}

pub async fn create_pool(database_url: &str) -> Result<AnyPool, sqlx::Error> {
    // Install both SQLite and Postgres drivers so AnyPool can pick at runtime.
    sqlx::any::install_default_drivers();

    let is_pg = url_is_postgres(database_url);
    set_is_postgres(is_pg);

    // For PostgreSQL, attempt to create the database if it doesn't exist.
    if is_pg {
        match ensure_pg_database_exists(database_url).await {
            Ok(()) => tracing::info!("postgres database check passed"),
            Err(e) => tracing::error!("could not ensure postgres database exists: {e}"),
        }
    }

    // Diagnostic: verify the parsed connection options before creating the pool.
    if is_pg {
        use sqlx::postgres::PgConnectOptions;

        // Path A: direct PgConnectOptions::from_str (what ensure_pg_database_exists uses)
        match PgConnectOptions::from_str(database_url) {
            Ok(pg_opts) => {
                tracing::info!(
                    "diag path-A (direct): user=`{}` host=`{}` port={} db=`{}`",
                    pg_opts.get_username(),
                    pg_opts.get_host(),
                    pg_opts.get_port(),
                    pg_opts.get_database().unwrap_or("<default>"),
                );
                match sqlx::postgres::PgConnection::connect_with(&pg_opts).await {
                    Ok(_) => tracing::info!("diag path-A: PgConnection succeeded"),
                    Err(e) => tracing::error!("diag path-A: PgConnection failed: {e}"),
                }
            }
            Err(e) => tracing::error!("diag path-A: parse failed: {e}"),
        }

        // Path B: AnyConnectOptions → PgConnectOptions (what AnyPool actually uses)
        match AnyConnectOptions::from_str(database_url) {
            Ok(ref any_opts) => {
                tracing::info!(
                    "diag path-B: AnyConnectOptions.database_url = `{}`",
                    any_opts.database_url.as_str()
                );
                match PgConnectOptions::try_from(any_opts) {
                    Ok(pg_opts_b) => {
                        tracing::info!(
                            "diag path-B (via Any): user=`{}` host=`{}` port={} db=`{}`",
                            pg_opts_b.get_username(),
                            pg_opts_b.get_host(),
                            pg_opts_b.get_port(),
                            pg_opts_b.get_database().unwrap_or("<default>"),
                        );
                        match sqlx::postgres::PgConnection::connect_with(&pg_opts_b).await {
                            Ok(_) => tracing::info!("diag path-B: PgConnection succeeded"),
                            Err(e) => tracing::error!("diag path-B: PgConnection failed: {e}"),
                        }
                    }
                    Err(e) => tracing::error!("diag path-B: TryFrom conversion failed: {e}"),
                }
            }
            Err(e) => tracing::error!("diag path-B: AnyConnectOptions parse failed: {e}"),
        }
    }

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
