-- Opaque per-server device fingerprints (64 lowercase hex chars, computed by
-- the client) seen for each account, and instance-wide bans on them.
CREATE TABLE IF NOT EXISTS user_devices (
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    first_seen  TEXT NOT NULL,
    last_seen   TEXT NOT NULL,
    PRIMARY KEY (user_id, fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_user_devices_fingerprint ON user_devices(fingerprint);

-- user_id deliberately has no foreign key: a device ban must outlive the
-- account it was issued against.
CREATE TABLE IF NOT EXISTS device_bans (
    id          TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL UNIQUE,
    user_id     TEXT,
    username    TEXT,
    reason      TEXT,
    banned_by   TEXT,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_device_bans_created ON device_bans(created_at);
