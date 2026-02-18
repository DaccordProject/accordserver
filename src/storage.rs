use std::path::{Path, PathBuf};

use crate::error::AppError;

pub const MAX_EMOJI_SIZE: usize = 256 * 1024; // 256 KB
pub const MAX_SOUND_SIZE: usize = 2 * 1024 * 1024; // 2 MB

pub const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/gif", "image/webp"];
pub const ALLOWED_AUDIO_TYPES: &[&str] = &["audio/ogg", "audio/mpeg", "audio/wav"];

/// Parse a `data:<mime>;base64,<data>` URI for images.
/// Returns `(decoded_bytes, content_type, is_animated)`.
pub fn validate_image_data_uri(data: &str) -> Result<(Vec<u8>, String, bool), AppError> {
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
    if bytes.len() > MAX_EMOJI_SIZE {
        return Err(AppError::PayloadTooLarge(format!(
            "image exceeds maximum size of {} KB",
            MAX_EMOJI_SIZE / 1024
        )));
    }

    let is_animated = mime == "image/gif";
    Ok((bytes, mime.to_string(), is_animated))
}

/// Parse a `data:<mime>;base64,<data>` URI for audio.
/// Returns `(decoded_bytes, content_type)`.
pub fn validate_audio_data_uri(data: &str) -> Result<(Vec<u8>, String), AppError> {
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
    if bytes.len() > MAX_SOUND_SIZE {
        return Err(AppError::PayloadTooLarge(format!(
            "audio exceeds maximum size of {} MB",
            MAX_SOUND_SIZE / (1024 * 1024)
        )));
    }

    Ok((bytes, mime.to_string()))
}

/// Save a base64-encoded image to disk.
/// Returns `(relative_url, content_type, file_size)`.
pub async fn save_base64_image(
    storage_path: &Path,
    space_id: &str,
    file_id: &str,
    data: &str,
) -> Result<(String, String, usize, bool), AppError> {
    let (bytes, content_type, is_animated) = validate_image_data_uri(data)?;
    let ext = mime_to_ext(&content_type);
    let size = bytes.len();

    let dir = storage_path.join("emojis").join(space_id);
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

/// Save a base64-encoded audio file to disk.
/// Returns `(relative_url, content_type, file_size)`.
pub async fn save_base64_audio(
    storage_path: &Path,
    space_id: &str,
    file_id: &str,
    data: &str,
) -> Result<(String, String, usize), AppError> {
    let (bytes, content_type) = validate_audio_data_uri(data)?;
    let ext = mime_to_ext(&content_type);
    let size = bytes.len();

    let dir = storage_path.join("sounds").join(space_id);
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

/// Delete a file given its relative path (e.g. `/cdn/emojis/123/456.png`).
pub async fn delete_file(storage_path: &Path, relative_path: &str) -> Result<(), AppError> {
    // Strip the leading `/cdn/` to get the path relative to storage_path
    let rel = relative_path.strip_prefix("/cdn/").unwrap_or(relative_path);
    let file_path = storage_path.join(rel);
    if file_path.exists() {
        tokio::fs::remove_file(&file_path)
            .await
            .map_err(|e| AppError::Internal(format!("failed to delete file: {e}")))?;
    }
    Ok(())
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
pub fn temp_storage_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("accord-test-{}", uuid::Uuid::new_v4()));
    path
}
