CREATE TABLE IF NOT EXISTS server_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    max_emoji_size INTEGER NOT NULL DEFAULT 262144,
    max_avatar_size INTEGER NOT NULL DEFAULT 2097152,
    max_sound_size INTEGER NOT NULL DEFAULT 2097152,
    max_attachment_size INTEGER NOT NULL DEFAULT 26214400,
    max_attachments_per_message INTEGER NOT NULL DEFAULT 10,
    updated_at TEXT
);

INSERT OR IGNORE INTO server_settings (id) VALUES (1);
