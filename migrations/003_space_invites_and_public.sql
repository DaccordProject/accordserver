-- Make invites.channel_id nullable (space-level invites) and add spaces.public column.

PRAGMA foreign_keys = OFF;

ALTER TABLE invites RENAME TO _invites_old;

CREATE TABLE invites (
    code TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    channel_id TEXT REFERENCES channels(id) ON DELETE CASCADE,
    inviter_id TEXT REFERENCES users(id),
    max_uses INTEGER,
    uses INTEGER NOT NULL DEFAULT 0,
    max_age INTEGER,
    temporary INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT
);

INSERT INTO invites SELECT * FROM _invites_old;
DROP TABLE _invites_old;

CREATE INDEX idx_invites_space_id ON invites(space_id);
CREATE INDEX idx_invites_channel_id ON invites(channel_id);

PRAGMA foreign_keys = ON;

ALTER TABLE spaces ADD COLUMN public INTEGER NOT NULL DEFAULT 0;
