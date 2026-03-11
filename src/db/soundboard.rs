use sqlx::{AnyPool, Row};

use crate::error::AppError;
use crate::models::soundboard::{CreateSound, SoundboardSound, UpdateSound};
use crate::snowflake;

fn row_to_sound(row: sqlx::any::AnyRow) -> SoundboardSound {
    SoundboardSound {
        id: row.get("id"),
        name: row.get("name"),
        audio_url: row.get("audio_path"),
        volume: crate::db::get_f64(&row, "volume"),
        creator_id: row.get("creator_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub async fn get_sound(pool: &AnyPool, sound_id: &str) -> Result<SoundboardSound, AppError> {
    let row = sqlx::query(
        &super::q("SELECT id, name, audio_path, volume, creator_id, created_at, updated_at FROM soundboard_sounds WHERE id = ?")
    )
    .bind(sound_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("unknown_sound".to_string()))?;

    Ok(row_to_sound(row))
}

pub async fn list_sounds(pool: &AnyPool, space_id: &str) -> Result<Vec<SoundboardSound>, AppError> {
    let rows = sqlx::query(
        &super::q("SELECT id, name, audio_path, volume, creator_id, created_at, updated_at FROM soundboard_sounds WHERE space_id = ? ORDER BY created_at ASC")
    )
    .bind(space_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_sound).collect())
}

pub async fn create_sound(
    pool: &AnyPool,
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
        &super::q("INSERT INTO soundboard_sounds (id, space_id, name, audio_path, audio_content_type, audio_size, volume, creator_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
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
    pool: &AnyPool,
    sound_id: &str,
    input: &UpdateSound,
    is_postgres: bool,
) -> Result<SoundboardSound, AppError> {
    let now_fn = crate::db::now_sql(is_postgres);
    if let Some(ref name) = input.name {
        let sql =
            format!("UPDATE soundboard_sounds SET name = ?, updated_at = {now_fn} WHERE id = ?");
        let sql = super::q(&sql);
        sqlx::query(&sql)
            .bind(name)
            .bind(sound_id)
            .execute(pool)
            .await?;
    }
    if let Some(volume) = input.volume {
        let volume = volume.clamp(0.0, 2.0);
        let sql =
            format!("UPDATE soundboard_sounds SET volume = ?, updated_at = {now_fn} WHERE id = ?");
        let sql = super::q(&sql);
        sqlx::query(&sql)
            .bind(volume)
            .bind(sound_id)
            .execute(pool)
            .await?;
    }
    get_sound(pool, sound_id).await
}

/// Delete a sound. Returns the audio_path for file cleanup.
pub async fn delete_sound(pool: &AnyPool, sound_id: &str) -> Result<Option<String>, AppError> {
    let audio_path: Option<String> = sqlx::query_scalar(&super::q(
        "SELECT audio_path FROM soundboard_sounds WHERE id = ?",
    ))
    .bind(sound_id)
    .fetch_optional(pool)
    .await?
    .flatten();

    sqlx::query(&super::q("DELETE FROM soundboard_sounds WHERE id = ?"))
        .bind(sound_id)
        .execute(pool)
        .await?;

    Ok(audio_path)
}
