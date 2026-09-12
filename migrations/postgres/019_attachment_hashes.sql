-- Older local attachments are hashed lazily when a moderator blocks them.
ALTER TABLE attachments ADD COLUMN content_hash TEXT;
CREATE INDEX attachments_content_hash ON attachments(content_hash);
ALTER TABLE automod_hashes ADD COLUMN added_by TEXT;
ALTER TABLE automod_hashes ADD COLUMN created_at BIGINT;
