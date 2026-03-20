CREATE TABLE IF NOT EXISTS audit_log (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    action_type TEXT NOT NULL,
    target_id TEXT,
    target_type TEXT,
    reason TEXT,
    changes TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_audit_log_space ON audit_log(space_id);
CREATE INDEX idx_audit_log_action ON audit_log(space_id, action_type);
