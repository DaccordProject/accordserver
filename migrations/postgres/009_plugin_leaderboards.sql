CREATE TABLE plugin_leaderboard_records (
    id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    space_id TEXT NOT NULL,
    board_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    score DOUBLE PRECISION NOT NULL,
    metadata TEXT,
    period TEXT NOT NULL DEFAULT 'current',
    updated_at TEXT NOT NULL,
    UNIQUE(plugin_id, space_id, board_id, user_id, period)
);
