-- Read states: tracks per-user, per-channel read position
CREATE TABLE IF NOT EXISTS read_states (
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id   TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    last_read_message_id TEXT,
    mention_count INTEGER NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, channel_id)
);
