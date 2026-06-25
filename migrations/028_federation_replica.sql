-- Federation replica columns (Phases 2/3).
--
-- `origin` marks a row as a mirror of a remote entity (NULL = local, otherwise
-- the home domain). Mirrored rows use qualified IDs (`<snowflake>@<domain>`).
-- `members.origin` is the source of the "interested servers" fanout query.

ALTER TABLE spaces ADD COLUMN origin TEXT;
-- Per-space federation opt-in (S9). Off by default so private spaces never
-- federate accidentally; the home server refuses join handshakes when off.
ALTER TABLE spaces ADD COLUMN federation_enabled INTEGER NOT NULL DEFAULT 0;

ALTER TABLE channels ADD COLUMN origin TEXT;
ALTER TABLE roles ADD COLUMN origin TEXT;
ALTER TABLE messages ADD COLUMN origin TEXT;
ALTER TABLE members ADD COLUMN origin TEXT;

CREATE INDEX IF NOT EXISTS idx_members_origin ON members(origin);
CREATE INDEX IF NOT EXISTS idx_spaces_origin ON spaces(origin);
CREATE INDEX IF NOT EXISTS idx_messages_origin ON messages(origin);
