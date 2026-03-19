-- Guest/anonymous read-only access support

-- Per-channel flag for anonymous read access
ALTER TABLE channels ADD COLUMN allow_anonymous_read BOOLEAN NOT NULL DEFAULT FALSE;

-- Guest tokens: short-lived, scoped to a single space, no user account
CREATE TABLE IF NOT EXISTS guest_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for cleanup of expired guest tokens
CREATE INDEX IF NOT EXISTS idx_guest_tokens_expires ON guest_tokens(expires_at);
