-- Add image storage columns to emojis
ALTER TABLE emojis ADD COLUMN image_path TEXT;
ALTER TABLE emojis ADD COLUMN image_content_type TEXT;
ALTER TABLE emojis ADD COLUMN image_size INTEGER;

-- Soundboard sounds table
CREATE TABLE IF NOT EXISTS soundboard_sounds (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    audio_path TEXT,
    audio_content_type TEXT,
    audio_size INTEGER,
    volume REAL NOT NULL DEFAULT 1.0,
    creator_id TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
