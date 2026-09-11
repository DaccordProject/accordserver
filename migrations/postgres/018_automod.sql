-- Withheld uploads never have a row in attachments or a public CDN file.
CREATE TABLE automod_policies (scope_id TEXT PRIMARY KEY, policy TEXT NOT NULL);
CREATE TABLE automod_hashes (
    scope_id TEXT NOT NULL, hash TEXT NOT NULL, reason TEXT NOT NULL,
    PRIMARY KEY (scope_id, hash)
);
-- IDs are intentionally not cascading FKs: quarantine evidence survives deletion
-- of the original message/member/channel. Publication revalidates live metadata.
CREATE TABLE automod_uploads (
    id TEXT PRIMARY KEY, message_id TEXT NOT NULL, channel_id TEXT NOT NULL,
    space_id TEXT, author_id TEXT NOT NULL, filename TEXT NOT NULL,
    content_type TEXT NOT NULL, size BIGINT NOT NULL, hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','quarantined','published','rejected','removed')),
    reason TEXT NOT NULL DEFAULT '', result TEXT, rule_id TEXT,
    created_at BIGINT NOT NULL, expires_at BIGINT NOT NULL, next_attempt BIGINT NOT NULL,
    attempts BIGINT NOT NULL DEFAULT 0, file_removed BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX automod_queue ON automod_uploads(status, next_attempt);
CREATE INDEX automod_expiry ON automod_uploads(status, expires_at);
CREATE INDEX automod_unlink ON automod_uploads(file_removed, status);
CREATE INDEX automod_space ON automod_uploads(space_id, id);
CREATE TABLE automod_cache (
    hash TEXT NOT NULL, scanner_version TEXT NOT NULL, result TEXT NOT NULL,
    expires_at BIGINT NOT NULL, PRIMARY KEY (hash, scanner_version)
);
CREATE TABLE automod_events (
    id TEXT PRIMARY KEY, upload_id TEXT, scope_id TEXT NOT NULL,
    actor_id TEXT, action TEXT NOT NULL, details TEXT NOT NULL, created_at BIGINT NOT NULL
);
