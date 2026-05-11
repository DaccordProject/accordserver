use std::collections::HashMap;

use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::attachment::Attachment;

/// Insert an attachment row. The caller is responsible for generating the
/// `attachment_id` and passing the canonical `url` (the same URL that
/// resolves to the file written by `storage::save_attachment`). The URL
/// returned to the client and the URL stored in the database are identical:
/// the file path on disk is the single source of truth.
#[allow(clippy::too_many_arguments)]
pub async fn insert_attachment(
    pool: &AnyPool,
    attachment_id: &str,
    message_id: &str,
    filename: &str,
    content_type: Option<&str>,
    size: i64,
    url: &str,
    width: Option<i64>,
    height: Option<i64>,
) -> Result<Attachment, AppError> {
    sqlx::query(
        &super::q("INSERT INTO attachments (id, message_id, filename, content_type, size, url, width, height) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)"),
    )
    .bind(attachment_id)
    .bind(message_id)
    .bind(filename)
    .bind(content_type)
    .bind(size)
    .bind(url)
    .bind(width)
    .bind(height)
    .execute(pool)
    .await?;

    Ok(Attachment {
        id: attachment_id.to_string(),
        filename: filename.to_string(),
        description: None,
        content_type: content_type.map(|s| s.to_string()),
        size,
        url: url.to_string(),
        width,
        height,
    })
}

pub async fn get_attachments_for_message(
    pool: &AnyPool,
    message_id: &str,
) -> Result<Vec<Attachment>, AppError> {
    let rows = sqlx::query(&super::q(
        "SELECT id, filename, description, content_type, size, url, width, height \
         FROM attachments WHERE message_id = ?",
    ))
    .bind(message_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_attachment).collect())
}

pub async fn get_attachments_for_messages(
    pool: &AnyPool,
    message_ids: &[String],
) -> Result<HashMap<String, Vec<Attachment>>, AppError> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<&str> = message_ids.iter().map(|_| "?").collect();
    let in_clause = placeholders.join(", ");
    let sql = format!(
        "SELECT id, message_id, filename, description, content_type, size, url, width, height \
         FROM attachments WHERE message_id IN ({in_clause}) ORDER BY id ASC"
    );

    let sql = super::q(&sql);
    let mut q = sqlx::query(&sql);
    for id in message_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;

    let mut result: HashMap<String, Vec<Attachment>> = HashMap::new();
    for row in rows {
        let msg_id: String = row.get("message_id");
        let attachment = row_to_attachment(row);
        result.entry(msg_id).or_default().push(attachment);
    }

    Ok(result)
}

fn row_to_attachment(row: sqlx::any::AnyRow) -> Attachment {
    Attachment {
        id: row.get("id"),
        filename: row.get("filename"),
        description: row.get("description"),
        content_type: row.get("content_type"),
        size: row.get("size"),
        url: row.get("url"),
        width: row.get("width"),
        height: row.get("height"),
    }
}
