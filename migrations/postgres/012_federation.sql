-- Peer-to-peer federation foundations (Phase 0). PostgreSQL variant.
-- See migrations/027_federation.sql for design notes.

ALTER TABLE users ADD COLUMN origin TEXT;
CREATE INDEX IF NOT EXISTS idx_users_origin ON users(origin);

CREATE TABLE IF NOT EXISTS federation_peers (
    domain      TEXT PRIMARY KEY NOT NULL,
    public_key  TEXT NOT NULL,
    inbox_url   TEXT NOT NULL,
    trust_state TEXT NOT NULL DEFAULT 'pending',
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS federation_inbox_dedup (
    event_id    TEXT NOT NULL,
    origin      TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (event_id, origin)
);

CREATE TABLE IF NOT EXISTS federation_outbox (
    id              TEXT PRIMARY KEY NOT NULL,
    target_domain   TEXT NOT NULL,
    payload         TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    created_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_federation_outbox_next ON federation_outbox(next_attempt_at);
