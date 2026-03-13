-- Plugin system: installed plugins, activity sessions, and session participants.

CREATE TABLE plugins (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    plugin_type TEXT NOT NULL DEFAULT 'activity',
    runtime TEXT NOT NULL CHECK (runtime IN ('scripted', 'native')),
    description TEXT NOT NULL DEFAULT '',
    version TEXT NOT NULL DEFAULT '1.0.0',
    manifest_json TEXT NOT NULL DEFAULT '{}',
    elf_blob BYTEA,
    bundle_blob BYTEA,
    icon_blob BYTEA,
    bundle_hash TEXT NOT NULL DEFAULT '',
    signed BOOLEAN NOT NULL DEFAULT false,
    creator_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at TEXT NOT NULL DEFAULT to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);

CREATE INDEX idx_plugins_space_id ON plugins(space_id);

CREATE TABLE plugin_sessions (
    id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL REFERENCES plugins(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    host_user_id TEXT NOT NULL REFERENCES users(id),
    state TEXT NOT NULL DEFAULT 'lobby' CHECK (state IN ('lobby', 'running', 'ended')),
    created_at TEXT NOT NULL DEFAULT to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    updated_at TEXT NOT NULL DEFAULT to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')
);

CREATE INDEX idx_plugin_sessions_plugin_id ON plugin_sessions(plugin_id);
CREATE INDEX idx_plugin_sessions_channel_id ON plugin_sessions(channel_id);

CREATE TABLE plugin_session_participants (
    session_id TEXT NOT NULL REFERENCES plugin_sessions(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    role TEXT NOT NULL DEFAULT 'spectator' CHECK (role IN ('player', 'spectator')),
    slot_index INTEGER,
    joined_at TEXT NOT NULL DEFAULT to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    PRIMARY KEY (session_id, user_id)
);
