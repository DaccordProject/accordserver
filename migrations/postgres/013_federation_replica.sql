-- Federation replica columns (Phases 2/3). PostgreSQL variant.
-- See migrations/028_federation_replica.sql for design notes.

ALTER TABLE spaces ADD COLUMN origin TEXT;
ALTER TABLE spaces ADD COLUMN federation_enabled INTEGER NOT NULL DEFAULT 0;

ALTER TABLE channels ADD COLUMN origin TEXT;
ALTER TABLE roles ADD COLUMN origin TEXT;
ALTER TABLE messages ADD COLUMN origin TEXT;
ALTER TABLE members ADD COLUMN origin TEXT;

CREATE INDEX IF NOT EXISTS idx_members_origin ON members(origin);
CREATE INDEX IF NOT EXISTS idx_spaces_origin ON spaces(origin);
CREATE INDEX IF NOT EXISTS idx_messages_origin ON messages(origin);
