CREATE TABLE space_arcades (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    enabled BIGINT NOT NULL DEFAULT 1
);
CREATE TABLE space_experiences (
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    game_id TEXT NOT NULL,
    release_json TEXT NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1,
    enabled BIGINT NOT NULL DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (space_id, game_id)
);
CREATE TABLE experience_sessions (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    game_id TEXT NOT NULL,
    revision BIGINT NOT NULL,
    state TEXT NOT NULL,
    deadline BIGINT,
    session_json TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE INDEX experience_sessions_space ON experience_sessions(space_id, updated_at);
-- Legacy tables are retained as an inert archive for operator export. Their
-- executable routes are retired; no legacy package is converted or enabled.

CREATE INDEX experience_sessions_cleanup ON experience_sessions(state, deadline, updated_at);
