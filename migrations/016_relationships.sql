-- Relationships between users (per-server friend/block/pending system)
-- type values:
--   1 = FRIEND
--   2 = BLOCKED
--   3 = PENDING_INCOMING  (target received a request from user_id)
--   4 = PENDING_OUTGOING  (user_id sent a request to target_user_id)
-- Each directed relationship is stored as its own row from the actor's perspective.
CREATE TABLE IF NOT EXISTS relationships (
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type           INTEGER NOT NULL CHECK(type IN (1, 2, 3, 4)),
    created_at     TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, target_user_id)
);

CREATE INDEX idx_relationships_user ON relationships(user_id, type);
CREATE INDEX idx_relationships_target ON relationships(target_user_id, type);
