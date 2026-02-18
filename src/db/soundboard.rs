use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::soundboard::{CreateSound, SoundboardSound, UpdateSound};
use crate::snowflake;

type SoundRow = (
    String,
    String,
    Option<String>,
    f64,
    Option<String>,
    String,
    String,
);

fn row_to_sound(row: SoundRow) -> SoundboardSound {
    SoundboardSound {
        id: row.0,
        name: row.1,
        audio_url: row.2,
        volume: row.3,
        creator_id: row.4,
        created_at: row.5,
        updated_at: row.6,
    }
}

pub async fn get_sound(pool: &SqlitePool, sound_id: &str) -> Result<SoundboardSound, AppError> {
    let row = sqlx::query_as::<_, SoundRow>(
        "SELECT id, name, audio_path, volume, creator_id, created_at, updated_at FROM soundboard_sounds WHERE id = ?"
    )
    .bind(sound_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown_sound".to_string()))?;

    Ok(row_to_sound(row))
}

pub async fn list_sounds(
    pool: &SqlitePool,
    space_id: &str,
) -> Result<Vec<SoundboardSound>, AppError> {
    let rows = sqlx::query_as::<_, SoundRow>(
        "SELECT id, name, audio_path, volume, creator_id, created_at, updated_at FROM soundboard_sounds WHERE space_id = ? ORDER BY created_at ASC"
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_sound).collect())
}

pub async fn create_sound(
    pool: &SqlitePool,
    space_id: &str,
    creator_id: &str,
    input: &CreateSound,
    audio_path: Option<&str>,
    audio_content_type: Option<&str>,
    audio_size: Option<usize>,
) -> Result<SoundboardSound, AppError> {
    let id = snowflake::generate();
    let volume = input.volume.unwrap_or(1.0).clamp(0.0, 2.0);

    sqlx::query(
        "INSERT INTO soundboard_sounds (id, space_id, name, audio_path, audio_content_type, audio_size, volume, creator_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(space_id)
    .bind(&input.name)
    .bind(audio_path)
    .bind(audio_content_type)
    .bind(audio_size.map(|s| s as i64))
    .bind(volume)
    .bind(creator_id)
    .execute(pool)
    .await?;

    get_sound(pool, &id).await
}

pub async fn update_sound(
    pool: &SqlitePool,
    sound_id: &str,
    input: &UpdateSound,
) -> Result<SoundboardSound, AppError> {
    if let Some(ref name) = input.name {
        sqlx::query(
            "UPDATE soundboard_sounds SET name = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(name)
        .bind(sound_id)
        .execute(pool)
        .await?;
    }
    if let Some(volume) = input.volume {
        let volume = volume.clamp(0.0, 2.0);
        sqlx::query(
            "UPDATE soundboard_sounds SET volume = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(volume)
        .bind(sound_id)
        .execute(pool)
        .await?;
    }
    get_sound(pool, sound_id).await
}

/// Delete a sound. Returns the audio_path for file cleanup.
pub async fn delete_sound(pool: &SqlitePool, sound_id: &str) -> Result<Option<String>, AppError> {
    let audio_path: Option<String> =
        sqlx::query_scalar("SELECT audio_path FROM soundboard_sounds WHERE id = ?")
            .bind(sound_id)
            .fetch_optional(pool)
            .await?
            .flatten();

    sqlx::query("DELETE FROM soundboard_sounds WHERE id = ?")
        .bind(sound_id)
        .execute(pool)
        .await?;

    Ok(audio_path)
}
