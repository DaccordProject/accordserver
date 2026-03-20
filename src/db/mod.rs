pub mod admin;
pub mod attachments;
pub mod audit_log;
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
pub mod plugins;
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

/// Check schema privileges and database ownership on an open connection.
async fn ensure_schema_privileges(
    conn: &mut sqlx::postgres::PgConnection,
    user_name: &str,
    db_name: &str,
) {
    use sqlx::Row;

    // Check if we have CREATE privilege on the public schema.
    let has_create: Result<bool, _> =
        sqlx::query_scalar("SELECT has_schema_privilege(current_user, 'public', 'CREATE')")
            .fetch_one(&mut *conn)
            .await;

    match has_create {
        Ok(false) => {
            tracing::info!("granting CREATE on schema public to `{user_name}`");
            let grant_sql = if is_safe_pg_identifier(user_name) {
                format!("GRANT ALL ON SCHEMA public TO \"{user_name}\"")
            } else {
                "GRANT ALL ON SCHEMA public TO PUBLIC".to_string()
            };
            if let Err(e) = sqlx::query(&grant_sql).execute(&mut *conn).await {
                tracing::warn!(
                    "could not grant schema privileges (you may need to run as the database owner or superuser): {e}"
                );
            }
        }
        Err(e) => tracing::warn!("could not check schema privileges: {e}"),
        _ => {}
    }

    // Verify the current user owns the database (informational).
    let owner_row = sqlx::query(
        "SELECT pg_catalog.pg_get_userbyid(d.datdba) AS owner \
         FROM pg_catalog.pg_database d WHERE d.datname = current_database()",
    )
    .fetch_optional(&mut *conn)
    .await;

    if let Ok(Some(row)) = owner_row {
        let owner: String = row.get("owner");
        if owner != user_name {
            tracing::warn!(
                "database `{db_name}` is owned by `{owner}`, not `{user_name}` — \
                 migrations may fail if schema privileges are insufficient"
            );
        }
    }
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
    use sqlx::ConnectOptions;

    // Normalize the URL through PgConnectOptions → to_url_lossy() to ensure
    // credentials are unambiguously embedded (same workaround as AnyPool).
    let opts = PgConnectOptions::from_str(database_url)?;
    let canonical_url = opts.to_url_lossy();
    let opts = PgConnectOptions::from_str(canonical_url.as_str())?;

    let db_name = opts.get_database().unwrap_or("accord").to_owned();
    let user_name = opts.get_username().to_owned();

    tracing::info!(
        "checking if postgres database `{db_name}` exists (user=`{user_name}`, host=`{}`)",
        opts.get_host(),
    );

    if !is_safe_pg_identifier(&db_name) {
        tracing::error!("refusing to auto-create database with unsafe name: {db_name}");
        return Ok(());
    }

    // Try connecting to the target database directly first. In the common
    // case (e.g. Docker with POSTGRES_DB), it already exists and we can
    // skip the maintenance-database connection entirely.
    //
    // Retry up to 15 times with 2-second delays to handle the window between
    // PostgreSQL accepting TCP connections (pg_isready passes) and finishing
    // init-script execution (POSTGRES_USER role creation). Docker Compose
    // environments can take 10+ seconds for the init scripts to complete.
    const MAX_ATTEMPTS: u32 = 15;
    let target_opts = PgConnectOptions::from_str(canonical_url.as_str())?;
    for attempt in 1..=MAX_ATTEMPTS {
        match sqlx::postgres::PgConnection::connect_with(&target_opts).await {
            Ok(mut target_conn) => {
                tracing::info!("target database `{db_name}` is reachable");
                ensure_schema_privileges(&mut target_conn, &user_name, &db_name).await;
                return Ok(());
            }
            Err(e) => {
                // Check if the error is specifically about the database not existing
                // (as opposed to role/auth errors). The sqlx error string includes
                // "error returned from database:" as a prefix, so we must check for
                // the actual PostgreSQL message, not just substrings.
                let is_db_missing = match e.as_database_error() {
                    Some(db_err) => {
                        let msg = db_err.message();
                        msg.contains("does not exist")
                            && (msg.starts_with("database") || msg.contains("database \""))
                    }
                    None => false,
                };

                if is_db_missing {
                    tracing::info!("target database `{db_name}` does not exist, will create it");
                    break;
                }

                // Transient error (role not yet created, etc.) — retry.
                if attempt < MAX_ATTEMPTS {
                    tracing::info!(
                        "postgres not ready (attempt {attempt}/{MAX_ATTEMPTS}): {e} — retrying in 2s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                } else {
                    tracing::warn!(
                        "could not connect to target database `{db_name}` after {MAX_ATTEMPTS} attempts: {e}"
                    );
                    break;
                }
            }
        }
    }

    // Connect to the default `postgres` maintenance database to create the
    // target database.
    let maint_opts = PgConnectOptions::from_str(canonical_url.as_str())?.database("postgres");
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

    // Connect to the (now existing) target database for schema privilege check.
    let target_opts = PgConnectOptions::from_str(canonical_url.as_str())?;
    let mut target_conn = sqlx::postgres::PgConnection::connect_with(&target_opts).await?;
    ensure_schema_privileges(&mut target_conn, &user_name, &db_name).await;

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

    // For PostgreSQL, normalize the URL through PgConnectOptions to ensure
    // credentials are correctly embedded.  The AnyPool internally converts
    // AnyConnectOptions → PgConnectOptions via TryFrom, which calls
    // PgConnectOptions::parse_from_url().  That starts from
    // new_without_pgpass() whose default username is whoami::username()
    // (often "root" in Docker).  While the URL username should override
    // the default, building a canonical URL from PgConnectOptions
    // guarantees the username, password, host, port, and database are
    // unambiguously present — eliminating any URL-parsing edge cases.
    let connect_opts = if is_pg {
        use sqlx::postgres::PgConnectOptions;
        let pg_opts = PgConnectOptions::from_str(database_url)?;
        tracing::info!(
            "postgres connection: user=`{}` host=`{}` port={} db=`{}`",
            pg_opts.get_username(),
            pg_opts.get_host(),
            pg_opts.get_port(),
            pg_opts.get_database().unwrap_or("<default>"),
        );
        use sqlx::ConnectOptions;
        AnyConnectOptions::from_str(pg_opts.to_url_lossy().as_str())?
    } else {
        AnyConnectOptions::from_str(database_url)?
    };

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
