-- Keep cooldowns after message deletion and across process restarts.
CREATE TABLE message_cooldowns (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_sent_ms BIGINT NOT NULL,
    PRIMARY KEY (channel_id, user_id)
);
INSERT INTO message_cooldowns (channel_id, user_id, last_sent_ms)
SELECT channel_id, author_id, MAX(CAST(EXTRACT(EPOCH FROM CAST(created_at AS TIMESTAMPTZ)) * 1000 AS BIGINT)) FROM messages
GROUP BY channel_id, author_id;
ALTER TABLE server_settings ADD COLUMN upload_requests_per_minute BIGINT NOT NULL DEFAULT 6 CHECK (upload_requests_per_minute BETWEEN 1 AND 600);
ALTER TABLE server_settings ADD COLUMN upload_bytes_per_minute BIGINT NOT NULL DEFAULT 52428800 CHECK (upload_bytes_per_minute BETWEEN 1 AND 1099511627776);
