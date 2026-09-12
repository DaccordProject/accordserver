use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::state::AppState;

pub const MAX_EMOJI_SIZE: usize = 256 * 1024; // 256 KB
pub const MAX_AVATAR_SIZE: usize = 2 * 1024 * 1024; // 2 MB
pub const MAX_SOUND_SIZE: usize = 2 * 1024 * 1024; // 2 MB
pub const MAX_ATTACHMENT_SIZE: usize = 25 * 1024 * 1024; // 25 MB

pub const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/gif", "image/webp"];
pub const ALLOWED_AUDIO_TYPES: &[&str] = &["audio/ogg", "audio/mpeg", "audio/wav"];

/// Canonical lowercase SHA-256 hex digest used for every stored file. Hash
/// denylist entries, `attachments.content_hash` and automod uploads must all
/// agree on this format, so it lives in one place.
pub fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Parse a `data:<mime>;base64,<data>` URI for images with a custom size limit.
/// Returns `(decoded_bytes, content_type, is_animated)`.
pub fn validate_image_data_uri_with_limit(
    data: &str,
    max_size: usize,
) -> Result<(Vec<u8>, String, bool), AppError> {
    let rest = data
        .strip_prefix("data:")
        .ok_or_else(|| AppError::BadRequest("image must be a data URI".to_string()))?;
    let (mime, b64) = rest
        .split_once(";base64,")
        .ok_or_else(|| AppError::BadRequest("image must be a base64 data URI".to_string()))?;

    if !ALLOWED_IMAGE_TYPES.contains(&mime) {
        return Err(AppError::BadRequest(format!(
            "unsupported image type: {mime}. allowed: png, gif, webp"
        )));
    }

    let bytes = base64_decode(b64)?;
    if bytes.len() > max_size {
        if max_size >= 1024 * 1024 {
            return Err(AppError::PayloadTooLarge(format!(
                "image exceeds maximum size of {} MB",
                max_size / (1024 * 1024)
            )));
        }
        return Err(AppError::PayloadTooLarge(format!(
            "image exceeds maximum size of {} KB",
            max_size / 1024
        )));
    }

    let is_animated = mime == "image/gif";
    Ok((bytes, mime.to_string(), is_animated))
}

/// Parse a `data:<mime>;base64,<data>` URI for images.
/// Returns `(decoded_bytes, content_type, is_animated)`.
pub fn validate_image_data_uri(data: &str) -> Result<(Vec<u8>, String, bool), AppError> {
    validate_image_data_uri_with_limit(data, MAX_EMOJI_SIZE)
}

/// Parse a `data:<mime>;base64,<data>` URI for audio.
/// Returns `(decoded_bytes, content_type)`.
pub fn validate_audio_data_uri(data: &str, max_size: usize) -> Result<(Vec<u8>, String), AppError> {
    let rest = data
        .strip_prefix("data:")
        .ok_or_else(|| AppError::BadRequest("audio must be a data URI".to_string()))?;
    let (mime, b64) = rest
        .split_once(";base64,")
        .ok_or_else(|| AppError::BadRequest("audio must be a base64 data URI".to_string()))?;

    if !ALLOWED_AUDIO_TYPES.contains(&mime) {
        return Err(AppError::BadRequest(format!(
            "unsupported audio type: {mime}. allowed: ogg, mpeg, wav"
        )));
    }

    let bytes = base64_decode(b64)?;
    if bytes.len() > max_size {
        return Err(AppError::PayloadTooLarge(format!(
            "audio exceeds maximum size of {} MB",
            max_size / (1024 * 1024)
        )));
    }

    Ok((bytes, mime.to_string()))
}

/// Save a base64-encoded image to disk.
///
/// Every direct upload is admitted through [`crate::automod::admit_direct_upload`]
/// so the per-user upload budget and the hash denylist apply here exactly as
/// they do to message attachments. Returns `(relative_url, content_type, file_size, is_animated)`.
pub async fn save_base64_image(
    state: &AppState,
    uploader: &str,
    space_id: &str,
    file_id: &str,
    data: &str,
    max_size: usize,
) -> Result<(String, String, usize, bool), AppError> {
    let (bytes, content_type, is_animated) = validate_image_data_uri_with_limit(data, max_size)?;
    crate::automod::admit_direct_upload(state, uploader, Some(space_id), &bytes).await?;
    let ext = mime_to_ext(&content_type);
    let size = bytes.len();

    let dir = state.storage_path.join("emojis").join(space_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create emoji directory: {e}")))?;

    let filename = format!("{file_id}.{ext}");
    let file_path = dir.join(&filename);
    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write emoji file: {e}")))?;

    let relative_url = format!("/cdn/emojis/{space_id}/{filename}");
    Ok((relative_url, content_type, size, is_animated))
}

/// Save a base64-encoded audio file to disk. See [`save_base64_image`] for
/// the admission rules. Returns `(relative_url, content_type, file_size)`.
pub async fn save_base64_audio(
    state: &AppState,
    uploader: &str,
    space_id: &str,
    file_id: &str,
    data: &str,
    max_size: usize,
) -> Result<(String, String, usize), AppError> {
    let (bytes, content_type) = validate_audio_data_uri(data, max_size)?;
    crate::automod::admit_direct_upload(state, uploader, Some(space_id), &bytes).await?;
    let ext = mime_to_ext(&content_type);
    let size = bytes.len();

    let dir = state.storage_path.join("sounds").join(space_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create sounds directory: {e}")))?;

    let filename = format!("{file_id}.{ext}");
    let file_path = dir.join(&filename);
    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write sound file: {e}")))?;

    let relative_url = format!("/cdn/sounds/{space_id}/{filename}");
    Ok((relative_url, content_type, size))
}

/// Save a base64-encoded avatar/icon/banner image to disk.
/// `category` should be `"avatars"`, `"icons"`, or `"banners"`. `space_id` is
/// the space whose hash denylist applies (`None` for account-level images,
/// which are still checked against instance-wide blocks). See
/// [`save_base64_image`] for the admission rules.
/// Returns `(relative_url, content_type, file_size, is_animated)`.
#[allow(clippy::too_many_arguments)]
pub async fn save_avatar_image(
    state: &AppState,
    uploader: &str,
    space_id: Option<&str>,
    category: &str,
    entity_id: &str,
    data: &str,
    max_size: usize,
) -> Result<(String, String, usize, bool), AppError> {
    let (bytes, content_type, is_animated) = validate_image_data_uri_with_limit(data, max_size)?;
    crate::automod::admit_direct_upload(state, uploader, space_id, &bytes).await?;
    let ext = mime_to_ext(&content_type);
    let size = bytes.len();
    let storage_path = state.storage_path.as_path();

    let dir = storage_path.join(category);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create {category} directory: {e}")))?;

    // Delete any existing files for this entity (handles extension changes on re-upload)
    delete_avatar(storage_path, category, entity_id).await?;

    let filename = format!("{entity_id}.{ext}");
    let file_path = dir.join(&filename);
    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write {category} file: {e}")))?;

    let relative_url = format!("/cdn/{category}/{filename}");
    Ok((relative_url, content_type, size, is_animated))
}

/// Delete all files matching `entity_id.*` in the category directory.
/// Handles extension changes on re-upload.
pub async fn delete_avatar(
    storage_path: &Path,
    category: &str,
    entity_id: &str,
) -> Result<(), AppError> {
    let dir = storage_path.join(category);
    if !dir.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to read {category} directory: {e}")))?;
    let prefix = format!("{entity_id}.");
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| AppError::Internal(format!("failed to read directory entry: {e}")))?
    {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with(&prefix) {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    Ok(())
}

/// Save an uploaded attachment file to disk.
///
/// Files are organized by `attachment_id` (a stable, unique ID generated for
/// each upload) rather than `message_id`, so the on-disk location does not
/// depend on the message ID the client thinks the attachment belongs to.
/// This makes the URL the single source of truth and avoids 404s if a client
/// reconstructs URLs from a stale or mismatched message ID.
///
/// Returns `(relative_url, file_size)`. Callers already hold the content
/// digest (it is computed while the upload is read), so it is not recomputed here.
pub async fn save_attachment(
    storage_path: &Path,
    channel_id: &str,
    attachment_id: &str,
    filename: &str,
    bytes: &[u8],
    max_size: usize,
) -> Result<(String, usize), AppError> {
    if bytes.len() > max_size {
        return Err(AppError::PayloadTooLarge(format!(
            "attachment exceeds maximum size of {} MB",
            max_size / (1024 * 1024)
        )));
    }

    let dir = storage_path
        .join("attachments")
        .join(channel_id)
        .join(attachment_id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create attachment directory: {e}")))?;

    let safe_filename = sanitize_filename(filename);
    let file_path = dir.join(&safe_filename);
    let size = bytes.len();
    tokio::fs::write(&file_path, bytes)
        .await
        .map_err(|e| AppError::Internal(format!("failed to write attachment file: {e}")))?;

    let relative_url = format!("/cdn/attachments/{channel_id}/{attachment_id}/{safe_filename}");
    Ok((relative_url, size))
}

/// Sanitize a filename to prevent directory traversal and other issues.
/// Only allows alphanumeric characters, hyphens, underscores, and a single dot for extension.
fn sanitize_filename(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Remove leading dots (hidden files / path traversal)
    let name = name.trim_start_matches('.');
    // Collapse consecutive dots to prevent path tricks
    let mut result = String::new();
    let mut last_was_dot = false;
    for c in name.chars() {
        if c == '.' {
            if !last_was_dot {
                result.push(c);
            }
            last_was_dot = true;
        } else {
            last_was_dot = false;
            result.push(c);
        }
    }
    if result.is_empty() {
        "attachment".to_string()
    } else {
        result
    }
}

/// Delete a file given its relative path (e.g. `/cdn/emojis/123/456.png`).
pub async fn delete_file(storage_path: &Path, relative_path: &str) -> Result<(), AppError> {
    // Strip the leading `/cdn/` to get the path relative to storage_path
    let rel = relative_path.strip_prefix("/cdn/").unwrap_or(relative_path);
    let file_path = storage_path.join(rel);

    // Canonicalize both paths to prevent directory traversal. Only absence is success.
    let canonical_file = match tokio::fs::canonicalize(&file_path).await {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(AppError::Internal(format!(
                "failed to inspect file for deletion: {err}"
            )))
        }
    };
    let canonical_storage = tokio::fs::canonicalize(storage_path)
        .await
        .map_err(|err| AppError::Internal(format!("failed to inspect storage: {err}")))?;
    if !canonical_file.starts_with(&canonical_storage) {
        return Err(AppError::BadRequest("invalid file path".into()));
    }
    match tokio::fs::remove_file(canonical_file).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(AppError::Internal(format!("failed to delete file: {err}"))),
    }
}

fn mime_to_ext(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/jpeg" => "jpg",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        _ => "bin",
    }
}

fn base64_decode(input: &str) -> Result<Vec<u8>, AppError> {
    // Simple base64 decoder using a lookup table
    const DECODE_TABLE: [u8; 256] = {
        let mut table = [255u8; 256];
        let mut i = 0u8;
        // A-Z
        while i < 26 {
            table[(b'A' + i) as usize] = i;
            i += 1;
        }
        // a-z
        i = 0;
        while i < 26 {
            table[(b'a' + i) as usize] = 26 + i;
            i += 1;
        }
        // 0-9
        i = 0;
        while i < 10 {
            table[(b'0' + i) as usize] = 52 + i;
            i += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };

    // Filter out whitespace and padding
    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r' && b != b' ')
        .collect();

    let mut output = Vec::with_capacity(clean.len() * 3 / 4);
    let chunks = clean.chunks(4);

    for chunk in chunks {
        let mut buf = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            let val = DECODE_TABLE[b as usize];
            if val == 255 {
                return Err(AppError::BadRequest("invalid base64 data".to_string()));
            }
            buf[i] = val;
        }

        match chunk.len() {
            4 => {
                output.push((buf[0] << 2) | (buf[1] >> 4));
                output.push((buf[1] << 4) | (buf[2] >> 2));
                output.push((buf[2] << 6) | buf[3]);
            }
            3 => {
                output.push((buf[0] << 2) | (buf[1] >> 4));
                output.push((buf[1] << 4) | (buf[2] >> 2));
            }
            2 => {
                output.push((buf[0] << 2) | (buf[1] >> 4));
            }
            _ => {
                return Err(AppError::BadRequest("invalid base64 data".to_string()));
            }
        }
    }

    Ok(output)
}

/// Resolve a storage path to a canonical PathBuf for tests.
///
/// Mirrors the production layout (`data/cdn`) by placing the storage root under
/// a unique per-call parent directory. This keeps sibling state derived from
/// `storage_path.parent()` — notably `federation_key` — isolated per test
/// instead of colliding on a single shared `<tmp>/federation_key`.
pub fn temp_storage_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("accord-test-{}", uuid::Uuid::new_v4()));
    path.push("cdn");
    path
}

/// Retry durable deletions; a failed unlink stays queued across restarts.
pub async fn drain_attachment_deletions(state: &AppState) -> Result<(), AppError> {
    // The mutation middleware calls this after every write. Do not touch the
    // publication lock unless there is actually something queued.
    let (queued,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM attachment_deletions")
        .fetch_one(&state.db)
        .await?;
    if queued == 0 {
        return Ok(());
    }
    // Release may reuse an attachment ID after a moderator withdraws it. Do
    // not unlink a newly released copy using a stale deletion queue entry.
    let _automod_guard = state.automod.publication.lock().await;
    let rows: Vec<(String,)> = sqlx::query_as("SELECT url FROM attachment_deletions LIMIT 100")
        .fetch_all(&state.db)
        .await?;
    if rows.is_empty() {
        return Ok(());
    }
    // Federation can attach remote URLs; these have no local file to unlink.
    let local: Vec<&str> = rows
        .iter()
        .map(|(url,)| url.as_str())
        .filter(|url| url.starts_with("/cdn/attachments/"))
        .collect();
    let mut live = std::collections::HashSet::new();
    if !local.is_empty() {
        let placeholders = vec!["?"; local.len()].join(",");
        let sql = crate::db::q(&format!(
            "SELECT url FROM attachments WHERE url IN ({placeholders})"
        ));
        let mut query = sqlx::query_as::<_, (String,)>(&sql);
        for url in &local {
            query = query.bind(*url);
        }
        live.extend(query.fetch_all(&state.db).await?.into_iter().map(|r| r.0));
    }
    for (url,) in &rows {
        if url.starts_with("/cdn/attachments/") && !live.contains(url) {
            delete_file(&state.storage_path, url).await?;
        }
        sqlx::query(&crate::db::q(
            "DELETE FROM attachment_deletions WHERE url = ?",
        ))
        .bind(url)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// Gate downloads against live metadata even when filesystem cleanup is delayed.
pub async fn serve_attachment(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(path): axum::extract::Path<String>,
    req: axum::extract::Request,
) -> Result<axum::response::Response, AppError> {
    use tower::ServiceExt;
    if Path::new(&path)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(AppError::NotFound("attachment not found".into()));
    }
    let url = format!("/cdn/attachments/{path}");
    let exists: (i64,) = sqlx::query_as(&crate::db::q(
        "SELECT COUNT(*) FROM attachments a WHERE url = ? AND NOT EXISTS (SELECT 1 FROM automod_uploads u WHERE u.id=a.id AND u.status <> 'published')",
    ))
    .bind(&url)
    .fetch_one(&state.db)
    .await?;
    if exists.0 == 0 {
        return Err(AppError::NotFound("attachment not found".into()));
    }
    let root = tokio::fs::canonicalize(state.storage_path.join("attachments"))
        .await
        .map_err(|_| AppError::NotFound("attachment not found".into()))?;
    let file_path = tokio::fs::canonicalize(root.join(&path))
        .await
        .map_err(|_| AppError::NotFound("attachment not found".into()))?;
    if !file_path.starts_with(&root) {
        return Err(AppError::NotFound("attachment not found".into()));
    }
    // Serve through ServeFile for the content type, range support and revalidation
    // headers a hand-rolled stream cannot provide. Active content stays inert: the
    // /cdn/ layer still applies nosniff, a sandbox CSP and Content-Disposition.
    let mut response = tower_http::services::ServeFile::new(&file_path)
        .oneshot(req)
        .await
        .map_err(|err| AppError::Internal(format!("attachment read failed: {err}")))?
        .map(axum::body::Body::new);
    // Moderation can withdraw an attachment after publication. Require a fresh
    // server check on every download, including Range/conditional requests.
    response
        .headers_mut()
        .insert("Cache-Control", "private, no-cache".parse().unwrap());
    Ok(response)
}

/// Remove pre-upgrade orphans. The grace period protects uploads before their DB commit.
pub async fn reconcile_attachment_orphans(state: &AppState) -> Result<(), AppError> {
    let root = state.storage_path.join("attachments");
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(AppError::Internal(format!("orphan scan failed: {err}"))),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|err| AppError::Internal(err.to_string()))?
        {
            let kind = entry
                .file_type()
                .await
                .map_err(|err| AppError::Internal(err.to_string()))?;
            if kind.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            } // Never follow symlinks.
            let metadata = entry
                .metadata()
                .await
                .map_err(|err| AppError::Internal(err.to_string()))?;
            if !metadata
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() >= 3600)
            {
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(&state.storage_path)
                .map_err(|_| AppError::Internal("invalid orphan path".into()))?;
            let url = format!("/cdn/{}", relative.to_string_lossy());
            let exists: (i64,) = sqlx::query_as(&crate::db::q(
                "SELECT COUNT(*) FROM attachments WHERE url = ?",
            ))
            .bind(&url)
            .fetch_one(&state.db)
            .await?;
            if exists.0 == 0 {
                delete_file(&state.storage_path, &url).await?;
            }
        }
    }
    Ok(())
}

/// Stream a legacy local attachment's digest; never fetch a remote URL.
pub async fn hash_local_attachment(storage_path: &Path, url: &str) -> Result<String, AppError> {
    use tokio::io::AsyncReadExt;
    let relative = url
        .strip_prefix("/cdn/attachments/")
        .ok_or_else(|| AppError::BadRequest("only local attachments can be hashed".into()))?;
    let root = tokio::fs::canonicalize(storage_path.join("attachments"))
        .await
        .map_err(|_| AppError::NotFound("attachment file missing".into()))?;
    let path = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| AppError::NotFound("attachment file missing".into()))?;
    if !path.starts_with(&root) {
        return Err(AppError::BadRequest("invalid attachment path".into()));
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::Internal(format!("cannot hash attachment: {e}")))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| AppError::Internal(format!("cannot hash attachment: {e}")))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
