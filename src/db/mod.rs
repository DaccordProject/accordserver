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
pub mod relationships;
pub mod reports;
pub mod roles;
pub mod settings;
pub mod soundboard;
pub mod spaces;
pub mod users;

use sqlx::AnyPool;

/// Returns true if the database URL targets PostgreSQL.
pub fn url_is_postgres(database_url: &str) -> bool {
    database_url.starts_with("postgres://") || database_url.starts_with("postgresql://")
}

pub async fn create_pool(database_url: &str) -> Result<AnyPool, sqlx::Error> {
    // Install both SQLite and Postgres drivers so AnyPool can pick at runtime.
    sqlx::any::install_default_drivers();

    let pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    // SQLite-specific PRAGMAs must be sent after connection.
    if !url_is_postgres(database_url) {
        sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await?;
        sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;
    }

    // Run the correct migration set for this backend.
    if url_is_postgres(database_url) {
        sqlx::migrate!("migrations/postgres").run(&pool).await?;
    } else {
        sqlx::migrate!("migrations").run(&pool).await?;
    }

    Ok(pool)
}
