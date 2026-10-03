-- Arcade has ordinary channel positioning, but only one entry per space.
CREATE UNIQUE INDEX channels_one_arcade_per_space ON channels(space_id) WHERE type = 'arcade';
INSERT INTO channels (id, name, type, space_id, position)
SELECT 'arcade-' || e.space_id, 'arcade', 'arcade', e.space_id,
       COALESCE((SELECT MAX(c.position) + 1 FROM channels c WHERE c.space_id = e.space_id AND c.parent_id IS NULL), 0)
FROM space_experiences e
WHERE e.enabled = 1
  AND NOT EXISTS (SELECT 1 FROM space_arcades a WHERE a.space_id = e.space_id AND a.enabled = 0)
  AND NOT EXISTS (SELECT 1 FROM channels c WHERE c.space_id = e.space_id AND c.type = 'arcade')
GROUP BY e.space_id;

-- Player activity is separate from live simulation ticks and spectator reads.
ALTER TABLE experience_sessions ADD COLUMN last_activity_at BIGINT NOT NULL DEFAULT 0;
UPDATE experience_sessions SET last_activity_at = updated_at;
CREATE INDEX experience_sessions_idle_cleanup ON experience_sessions(state, last_activity_at);
