-- Allow a report that belongs to no space.
--
-- Reports filed from a direct message, or against a user from outside any
-- space, have no moderators to route to — they belong to the instance
-- operator. `POST /api/v1/reports` stores those with a NULL space_id, and
-- `GET /api/v1/admin/reports` is the queue that serves them.
--
-- SQLite cannot drop a NOT NULL in place, so the table is rebuilt the same way
-- 030 rebuilt it. Nothing references reports(id), so the rename/copy/drop
-- dance is safe.
ALTER TABLE reports RENAME TO reports_old;

CREATE TABLE reports (
    id TEXT PRIMARY KEY,
    space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE,
    reporter_id TEXT NOT NULL REFERENCES users(id),
    target_type TEXT NOT NULL CHECK (target_type IN ('message', 'user')),
    target_id TEXT NOT NULL,
    channel_id TEXT,
    category TEXT NOT NULL CHECK (category IN (
        'spam', 'harassment', 'hate', 'nsfw', 'violence',
        'self_harm', 'csam', 'terrorism', 'fraud', 'other'
    )),
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'actioned', 'dismissed')),
    actioned_by TEXT REFERENCES users(id),
    action_taken TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);

INSERT INTO reports (
    id, space_id, reporter_id, target_type, target_id, channel_id,
    category, description, status, actioned_by, action_taken,
    created_at, resolved_at
)
SELECT
    id, space_id, reporter_id, target_type, target_id, channel_id,
    category, description, status, actioned_by, action_taken,
    created_at, resolved_at
FROM reports_old;

-- Dropping the old table also drops the indexes still attached to it, freeing
-- their names for the recreated table.
DROP TABLE reports_old;

CREATE INDEX idx_reports_space_status ON reports(space_id, status);
CREATE INDEX idx_reports_space_created ON reports(space_id, created_at DESC);
-- The instance-wide operator queue orders across every space, and the common
-- case is "the ones with no space at all".
CREATE INDEX idx_reports_created ON reports(created_at DESC);
