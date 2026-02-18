-- Index for space-scoped message search queries
CREATE INDEX IF NOT EXISTS idx_messages_space_id ON messages(space_id);
