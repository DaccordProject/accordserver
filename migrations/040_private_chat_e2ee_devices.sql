-- Private chat encryption v2: versioned account keys and per-device keys.
-- Version 1 of every account is the existing v1 identity.
ALTER TABLE e2ee_identities ADD COLUMN account_version BIGINT NOT NULL DEFAULT 1;
-- Milliseconds since the epoch of the last identity refresh from a remote home.
ALTER TABLE e2ee_identities ADD COLUMN refreshed_at BIGINT;
CREATE TABLE e2ee_account_versions (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    version BIGINT NOT NULL,
    exchange_key TEXT NOT NULL,
    signing_key TEXT NOT NULL,
    -- Succession signature by the previous version; NULL for version 1.
    signature TEXT,
    -- Back-signature by this version over the previous version's keys; NULL for version 1.
    back_signature TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, version)
);
INSERT INTO e2ee_account_versions (user_id, version, exchange_key, signing_key, signature, back_signature, created_at)
SELECT user_id, 1, exchange_key, signing_key, NULL, NULL, '1970-01-01T00:00:00.000Z' FROM e2ee_identities;
-- status: pending | active | revoked. Revoked rows are permanent tombstones so
-- history from removed devices still verifies and the ID cannot be reused.
-- token_hash binds a local device to the user_tokens session that last
-- registered it; remote (cached) devices have no binding.
CREATE TABLE e2ee_devices (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    name TEXT NOT NULL,
    exchange_key TEXT NOT NULL,
    signing_key TEXT NOT NULL,
    certificate TEXT,
    account_version BIGINT,
    status TEXT NOT NULL,
    token_hash TEXT,
    key_package TEXT,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    revoked_at TEXT,
    PRIMARY KEY (user_id, device_id)
);
CREATE INDEX e2ee_devices_token ON e2ee_devices(token_hash);
