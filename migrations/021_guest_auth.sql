-- Guest/anonymous read-only access support

-- Per-channel flag for anonymous read access
ALTER TABLE channels ADD COLUMN allow_anonymous_read INTEGER NOT NULL DEFAULT 0;

-- Guest tokens: short-lived, scoped to a single space, no user account
CREATE TABLE IF NOT EXISTS guest_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Index for cleanup of expired guest tokens
CREATE INDEX IF NOT EXISTS idx_guest_tokens_expires ON guest_tokens(expires_at);
