CREATE TABLE IF NOT EXISTS reports (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    reporter_id TEXT NOT NULL REFERENCES users(id),
    target_type TEXT NOT NULL CHECK (target_type IN ('message', 'user')),
    target_id TEXT NOT NULL,
    channel_id TEXT,
    category TEXT NOT NULL CHECK (category IN ('csam', 'terrorism', 'fraud', 'hate', 'violence', 'self_harm', 'other')),
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'actioned', 'dismissed')),
    actioned_by TEXT REFERENCES users(id),
    action_taken TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);

CREATE INDEX idx_reports_space_status ON reports(space_id, status);
CREATE INDEX idx_reports_space_created ON reports(space_id, created_at DESC);
