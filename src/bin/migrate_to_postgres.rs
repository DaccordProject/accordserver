//! `accord-migrate-pg` — migrate data from SQLite to PostgreSQL.
//!
//! Usage:
//!   SQLITE_URL=sqlite:data/accord.db?mode=rwc \
//!   POSTGRES_URL=postgres://user:pass@host/accord \
//!   cargo run --bin accord-migrate-pg
//!
//! Prerequisites:
//! - The PostgreSQL database must already exist.
//! - Migrations will run automatically against both databases on connect.
//! - The Postgres database should be empty (no prior data).
//!
//! This tool reads all rows from each SQLite table and inserts them into
//! the corresponding Postgres table. It processes tables in dependency
//! order to satisfy foreign key constraints.

use std::error::Error;

use sqlx::{AnyPool, Row};

use accordserver::db;

/// Tables in dependency order (parents before children).
const TABLES: &[TableDef] = &[
    TableDef {
        name: "users",
        columns: &[
            "id",
            "username",
            "display_name",
            "discriminator",
            "avatar",
            "banner",
            "bio",
            "bot",
            "system",
            "flags",
            "public_flags",
            "is_admin",
            "mfa_enabled",
            "disabled",
            "password_hash",
            "force_password_reset",
            "totp_secret",
            "created_at",
        ],
    },
    TableDef {
        name: "spaces",
        columns: &[
            "id",
            "name",
            "slug",
            "description",
            "icon",
            "banner",
            "owner_id",
            "public",
            "default_message_notifications",
            "explicit_content_filter",
            "features",
            "preferred_locale",
            "afk_channel_id",
            "afk_timeout",
            "system_channel_id",
            "system_channel_flags",
            "rules_channel_id",
            "max_members",
            "vanity_url_code",
            "created_at",
        ],
    },
    TableDef {
        name: "channels",
        columns: &[
            "id",
            "type",
            "space_id",
            "name",
            "description",
            "topic",
            "position",
            "parent_id",
            "nsfw",
            "rate_limit",
            "bitrate",
            "user_limit",
            "owner_id",
            "last_message_id",
            "archived",
            "auto_archive_after",
            "created_at",
        ],
    },
    TableDef {
        name: "roles",
        columns: &[
            "id",
            "space_id",
            "name",
            "color",
            "hoist",
            "position",
            "permissions",
            "mentionable",
            "created_at",
        ],
    },
    TableDef {
        name: "members",
        columns: &["user_id", "space_id", "nickname", "joined_at"],
    },
    TableDef {
        name: "member_roles",
        columns: &["user_id", "space_id", "role_id"],
    },
    TableDef {
        name: "messages",
        columns: &[
            "id",
            "channel_id",
            "space_id",
            "author_id",
            "content",
            "type",
            "tts",
            "mention_everyone",
            "mentions",
            "mention_roles",
            "pinned",
            "embeds",
            "reply_to",
            "flags",
            "webhook_id",
            "thread_id",
            "created_at",
            "edited_at",
        ],
    },
    TableDef {
        name: "attachments",
        columns: &[
            "id",
            "message_id",
            "channel_id",
            "filename",
            "content_type",
            "size",
            "url",
            "width",
            "height",
            "created_at",
        ],
    },
    TableDef {
        name: "reactions",
        columns: &[
            "message_id",
            "user_id",
            "emoji_name",
            "emoji_id",
            "created_at",
        ],
    },
    TableDef {
        name: "bans",
        columns: &["user_id", "space_id", "reason", "banned_by", "created_at"],
    },
    TableDef {
        name: "invites",
        columns: &[
            "code",
            "space_id",
            "channel_id",
            "inviter_id",
            "uses",
            "max_uses",
            "max_age",
            "temporary",
            "created_at",
        ],
    },
    TableDef {
        name: "emojis",
        columns: &[
            "id",
            "space_id",
            "name",
            "creator_id",
            "animated",
            "content_type",
            "width",
            "height",
            "file_size",
            "created_at",
        ],
    },
    TableDef {
        name: "emoji_roles",
        columns: &["emoji_id", "role_id"],
    },
    TableDef {
        name: "applications",
        columns: &[
            "id",
            "name",
            "description",
            "owner_id",
            "bot_user_id",
            "created_at",
        ],
    },
    TableDef {
        name: "bot_tokens",
        columns: &["token_hash", "user_id", "created_at"],
    },
    TableDef {
        name: "user_tokens",
        columns: &["token_hash", "user_id", "created_at", "expires_at"],
    },
    TableDef {
        name: "dm_participants",
        columns: &["channel_id", "user_id"],
    },
    TableDef {
        name: "pinned_messages",
        columns: &["channel_id", "message_id", "pinned_at"],
    },
    TableDef {
        name: "permission_overwrites",
        columns: &["id", "channel_id", "type", "allow", "deny"],
    },
    TableDef {
        name: "soundboard_sounds",
        columns: &[
            "id",
            "space_id",
            "name",
            "emoji_id",
            "emoji_name",
            "volume",
            "content_type",
            "file_size",
            "user_id",
            "created_at",
        ],
    },
    TableDef {
        name: "server_settings",
        columns: &[
            "id",
            "registration_enabled",
            "invite_only",
            "max_spaces_per_user",
            "max_channels_per_space",
            "max_message_length",
            "max_attachment_size",
            "max_attachments_per_message",
            "updated_at",
        ],
    },
    TableDef {
        name: "backup_codes",
        columns: &["id", "user_id", "code_hash", "used"],
    },
    TableDef {
        name: "channel_mutes",
        columns: &["user_id", "channel_id", "created_at"],
    },
    TableDef {
        name: "reports",
        columns: &[
            "id",
            "space_id",
            "reporter_id",
            "target_type",
            "target_id",
            "reason",
            "description",
            "status",
            "moderator_id",
            "resolution_note",
            "evidence",
            "created_at",
            "resolved_at",
        ],
    },
    TableDef {
        name: "read_states",
        columns: &[
            "user_id",
            "channel_id",
            "last_read_message_id",
            "mention_count",
            "updated_at",
        ],
    },
    TableDef {
        name: "relationships",
        columns: &["id", "user_id", "target_user_id", "type", "created_at"],
    },
];

struct TableDef {
    name: &'static str,
    columns: &'static [&'static str],
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let sqlite_url = std::env::var("SQLITE_URL")
        .unwrap_or_else(|_| "sqlite:data/accord.db?mode=rwc".to_string());
    let postgres_url = std::env::var("POSTGRES_URL")
        .map_err(|_| "POSTGRES_URL environment variable is required")?;

    if !db::url_is_postgres(&postgres_url) {
        return Err("POSTGRES_URL must start with postgres:// or postgresql://".into());
    }
    if db::url_is_postgres(&sqlite_url) {
        return Err("SQLITE_URL must be a SQLite URL".into());
    }

    println!("Connecting to SQLite: {}", sqlite_url);
    let sqlite_pool = db::create_pool(&sqlite_url).await?;
    println!("Connecting to PostgreSQL: {}", postgres_url);
    let pg_pool = db::create_pool(&postgres_url).await?;

    let mut total_rows: usize = 0;

    for table in TABLES {
        let count = migrate_table(&sqlite_pool, &pg_pool, table).await?;
        if count > 0 {
            println!("  {} — {} rows", table.name, count);
        }
        total_rows += count;
    }

    println!(
        "\nMigration complete: {} total rows transferred.",
        total_rows
    );
    Ok(())
}

async fn migrate_table(
    sqlite: &AnyPool,
    pg: &AnyPool,
    table: &TableDef,
) -> Result<usize, Box<dyn Error>> {
    let col_list = table.columns.join(", ");
    let select_sql = format!("SELECT {} FROM {}", col_list, table.name);

    let rows = sqlx::query(&select_sql).fetch_all(sqlite).await?;

    if rows.is_empty() {
        return Ok(0);
    }

    let placeholders: Vec<String> = (1..=table.columns.len()).map(|_| "?".to_string()).collect();
    let insert_sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT DO NOTHING",
        table.name,
        col_list,
        placeholders.join(", ")
    );

    let mut count = 0;
    for row in &rows {
        let mut query = sqlx::query(&insert_sql);
        for col in table.columns {
            // Read all columns as optional strings to handle NULL uniformly.
            // Numeric and boolean columns are stored as text in SQLite anyway.
            let val: Option<String> = row
                .try_get::<String, _>(*col)
                .ok()
                .or_else(|| {
                    // Try reading as i64 for integer columns
                    row.try_get::<i64, _>(*col).ok().map(|v| v.to_string())
                })
                .or_else(|| {
                    // Try reading as f64 for real columns
                    row.try_get::<f64, _>(*col).ok().map(|v| v.to_string())
                });
            query = query.bind(val);
        }
        match query.execute(pg).await {
            Ok(_) => count += 1,
            Err(e) => {
                eprintln!("  Warning: failed to insert row in {}: {}", table.name, e);
            }
        }
    }

    Ok(count)
}
