-- Per-user channel/category mute settings.
-- When a category is muted, all channels under it inherit the mute unless
-- the user has an explicit per-channel override.
CREATE TABLE IF NOT EXISTS channel_mutes (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, channel_id)
);

CREATE INDEX idx_channel_mutes_user ON channel_mutes(user_id);
