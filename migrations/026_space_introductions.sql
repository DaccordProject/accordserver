-- Tracks which users have already received a "joined the server" welcome
-- message in each space. Prevents duplicate introduction messages when a
-- user leaves and rejoins (or otherwise triggers a join flow more than once).
CREATE TABLE IF NOT EXISTS space_introductions (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    introduced_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (space_id, user_id)
);

-- Backfill from existing members so currently-joined accounts are not
-- re-introduced if they leave and rejoin after this migration runs.
INSERT OR IGNORE INTO space_introductions (space_id, user_id, introduced_at)
SELECT space_id, user_id, joined_at FROM members;
