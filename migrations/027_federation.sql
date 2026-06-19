-- Peer-to-peer federation foundations (Phase 0).
--
-- Remote entities are stored in the existing tables using qualified IDs
-- ("<snowflake>@<domain>") and distinguished by the new nullable `origin`
-- column (NULL = local to this server, otherwise the entity's home domain).
--
-- Note on usernames: the existing global `UNIQUE` on users.username is left in
-- place. Remote users are inserted with their fully-qualified handle
-- ("<username>@<domain>") in the `username` column, which is inherently unique
-- across servers and never collides with a local bare username. This avoids a
-- risky in-place SQLite table rebuild to drop the column constraint.

ALTER TABLE users ADD COLUMN origin TEXT;
CREATE INDEX IF NOT EXISTS idx_users_origin ON users(origin);

-- Known federation peers and their published Ed25519 public keys.
-- trust_state: 'pending' (key pinned, no content exchanged) or 'trusted'.
CREATE TABLE IF NOT EXISTS federation_peers (
    domain      TEXT PRIMARY KEY NOT NULL,
    public_key  TEXT NOT NULL,
    inbox_url   TEXT NOT NULL,
    trust_state TEXT NOT NULL DEFAULT 'pending',
    created_at  TEXT NOT NULL
);

-- At-least-once inbound delivery is deduplicated on (event_id, origin).
CREATE TABLE IF NOT EXISTS federation_inbox_dedup (
    event_id    TEXT NOT NULL,
    origin      TEXT NOT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (event_id, origin)
);

-- Durable outbound queue so fanout survives restarts.
CREATE TABLE IF NOT EXISTS federation_outbox (
    id              TEXT PRIMARY KEY NOT NULL,
    target_domain   TEXT NOT NULL,
    payload         TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL,
    created_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_federation_outbox_next ON federation_outbox(next_attempt_at);
