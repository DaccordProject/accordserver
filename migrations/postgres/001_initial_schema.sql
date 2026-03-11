-- Postgres initial schema — equivalent of SQLite migrations 001–016.
-- Uses Postgres-native types: BOOLEAN, BIGSERIAL. Timestamps stored as TEXT
-- (matching SQLite behaviour) so sqlx::any can decode them as String without
-- a chrono feature. IDs (snowflakes) and JSON columns remain TEXT.

-- Helper expression used in DEFAULT clauses:
-- to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')
-- This matches the format produced by SQLite's datetime('now').

-- Users
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    avatar TEXT,
    banner TEXT,
    accent_color INTEGER,
    bio TEXT,
    bot BOOLEAN NOT NULL DEFAULT FALSE,
    system BOOLEAN NOT NULL DEFAULT FALSE,
    flags INTEGER NOT NULL DEFAULT 0,
    public_flags INTEGER NOT NULL DEFAULT 0,
    password_hash TEXT,
    is_admin BOOLEAN NOT NULL DEFAULT FALSE,
    disabled BOOLEAN NOT NULL DEFAULT FALSE,
    force_password_reset BOOLEAN NOT NULL DEFAULT FALSE,
    totp_secret TEXT,
    totp_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

-- Spaces (guilds)
CREATE TABLE IF NOT EXISTS spaces (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    icon TEXT,
    banner TEXT,
    splash TEXT,
    owner_id TEXT NOT NULL REFERENCES users(id),
    verification_level TEXT NOT NULL DEFAULT 'none',
    default_notifications TEXT NOT NULL DEFAULT 'all',
    explicit_content_filter TEXT NOT NULL DEFAULT 'disabled',
    vanity_url_code TEXT UNIQUE,
    preferred_locale TEXT NOT NULL DEFAULT 'en-US',
    afk_channel_id TEXT,
    afk_timeout INTEGER NOT NULL DEFAULT 300,
    system_channel_id TEXT,
    rules_channel_id TEXT,
    nsfw_level TEXT NOT NULL DEFAULT 'default',
    premium_tier TEXT NOT NULL DEFAULT 'none',
    premium_subscription_count INTEGER NOT NULL DEFAULT 0,
    max_members INTEGER NOT NULL DEFAULT 500000,
    public BOOLEAN NOT NULL DEFAULT FALSE,
    slug TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_spaces_slug ON spaces(slug);

-- Channels
CREATE TABLE IF NOT EXISTS channels (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    type TEXT NOT NULL DEFAULT 'text',
    space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE,
    topic TEXT,
    position INTEGER NOT NULL DEFAULT 0,
    parent_id TEXT REFERENCES channels(id) ON DELETE SET NULL,
    nsfw BOOLEAN NOT NULL DEFAULT FALSE,
    rate_limit INTEGER NOT NULL DEFAULT 0,
    bitrate INTEGER,
    user_limit INTEGER,
    owner_id TEXT REFERENCES users(id),
    last_message_id TEXT,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    auto_archive_after INTEGER,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_channels_space_id ON channels(space_id);
CREATE INDEX IF NOT EXISTS idx_channels_parent_id ON channels(parent_id);

-- Roles
CREATE TABLE IF NOT EXISTS roles (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    color INTEGER NOT NULL DEFAULT 0,
    hoist BOOLEAN NOT NULL DEFAULT FALSE,
    icon TEXT,
    position INTEGER NOT NULL DEFAULT 0,
    permissions TEXT NOT NULL DEFAULT '[]',
    managed BOOLEAN NOT NULL DEFAULT FALSE,
    mentionable BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_roles_space_id ON roles(space_id);

-- Members
CREATE TABLE IF NOT EXISTS members (
    user_id TEXT NOT NULL REFERENCES users(id),
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    nickname TEXT,
    avatar TEXT,
    joined_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    premium_since TEXT,
    deaf BOOLEAN NOT NULL DEFAULT FALSE,
    mute BOOLEAN NOT NULL DEFAULT FALSE,
    pending BOOLEAN NOT NULL DEFAULT FALSE,
    timed_out_until TEXT,
    PRIMARY KEY (user_id, space_id)
);

CREATE INDEX IF NOT EXISTS idx_members_space_id ON members(space_id);
CREATE INDEX IF NOT EXISTS idx_members_user_id ON members(user_id);

-- Member roles junction table
CREATE TABLE IF NOT EXISTS member_roles (
    user_id TEXT NOT NULL,
    space_id TEXT NOT NULL,
    role_id TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, space_id, role_id),
    FOREIGN KEY (user_id, space_id) REFERENCES members(user_id, space_id) ON DELETE CASCADE
);

-- Messages
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels(id),
    author_id TEXT NOT NULL REFERENCES users(id),
    content TEXT NOT NULL,
    space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE,
    type TEXT NOT NULL DEFAULT 'default',
    tts BOOLEAN NOT NULL DEFAULT FALSE,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    mention_everyone BOOLEAN NOT NULL DEFAULT FALSE,
    mentions TEXT NOT NULL DEFAULT '[]',
    mention_roles TEXT NOT NULL DEFAULT '[]',
    embeds TEXT NOT NULL DEFAULT '[]',
    reply_to TEXT REFERENCES messages(id) ON DELETE SET NULL,
    flags INTEGER NOT NULL DEFAULT 0,
    webhook_id TEXT,
    edited_at TEXT,
    thread_id TEXT REFERENCES messages(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id);
CREATE INDEX IF NOT EXISTS idx_messages_author_id ON messages(author_id);
CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
CREATE INDEX IF NOT EXISTS idx_messages_space_id ON messages(space_id);
CREATE INDEX IF NOT EXISTS idx_messages_thread_id ON messages(thread_id);

-- Attachments
CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    description TEXT,
    content_type TEXT,
    size INTEGER NOT NULL DEFAULT 0,
    url TEXT NOT NULL,
    width INTEGER,
    height INTEGER
);

CREATE INDEX IF NOT EXISTS idx_attachments_message_id ON attachments(message_id);

-- Reactions
CREATE TABLE IF NOT EXISTS reactions (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    emoji_id TEXT,
    emoji_name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    PRIMARY KEY (message_id, user_id, emoji_name)
);

CREATE INDEX IF NOT EXISTS idx_reactions_message_id ON reactions(message_id);

-- Bans
CREATE TABLE IF NOT EXISTS bans (
    user_id TEXT NOT NULL REFERENCES users(id),
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    reason TEXT,
    banned_by TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    PRIMARY KEY (user_id, space_id)
);

-- Invites
CREATE TABLE IF NOT EXISTS invites (
    code TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    channel_id TEXT REFERENCES channels(id) ON DELETE CASCADE,
    inviter_id TEXT REFERENCES users(id),
    max_uses INTEGER,
    uses INTEGER NOT NULL DEFAULT 0,
    max_age INTEGER,
    temporary BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    expires_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_invites_space_id ON invites(space_id);
CREATE INDEX IF NOT EXISTS idx_invites_channel_id ON invites(channel_id);

-- Emojis
CREATE TABLE IF NOT EXISTS emojis (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    animated BOOLEAN NOT NULL DEFAULT FALSE,
    managed BOOLEAN NOT NULL DEFAULT FALSE,
    available BOOLEAN NOT NULL DEFAULT TRUE,
    require_colons BOOLEAN NOT NULL DEFAULT TRUE,
    creator_id TEXT REFERENCES users(id),
    image_path TEXT,
    image_content_type TEXT,
    image_size INTEGER,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_emojis_space_id ON emojis(space_id);

-- Emoji role restrictions
CREATE TABLE IF NOT EXISTS emoji_roles (
    emoji_id TEXT NOT NULL REFERENCES emojis(id) ON DELETE CASCADE,
    role_id TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (emoji_id, role_id)
);

-- Applications (bots)
CREATE TABLE IF NOT EXISTS applications (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    icon TEXT,
    description TEXT NOT NULL DEFAULT '',
    bot_public BOOLEAN NOT NULL DEFAULT TRUE,
    owner_id TEXT NOT NULL REFERENCES users(id),
    flags INTEGER NOT NULL DEFAULT 0,
    bot_user_id TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

-- Bot tokens
CREATE TABLE IF NOT EXISTS bot_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_bot_tokens_user_id ON bot_tokens(user_id);

-- User tokens (bearer tokens)
CREATE TABLE IF NOT EXISTS user_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    refresh_token_hash TEXT,
    scopes TEXT NOT NULL DEFAULT '[]',
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_user_tokens_user_id ON user_tokens(user_id);

-- DM channel participants
CREATE TABLE IF NOT EXISTS dm_participants (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    PRIMARY KEY (channel_id, user_id)
);

-- Pinned messages
CREATE TABLE IF NOT EXISTS pinned_messages (
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    pinned_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    PRIMARY KEY (channel_id, message_id)
);

-- Permission overwrites
CREATE TABLE IF NOT EXISTS permission_overwrites (
    id TEXT NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK(type IN ('role', 'member')),
    allow TEXT NOT NULL DEFAULT '[]',
    deny TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (id, channel_id)
);

-- Soundboard sounds
CREATE TABLE IF NOT EXISTS soundboard_sounds (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    audio_path TEXT,
    audio_content_type TEXT,
    audio_size INTEGER,
    volume DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    creator_id TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    updated_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

-- Server settings (singleton row, id=1)
CREATE TABLE IF NOT EXISTS server_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    max_emoji_size INTEGER NOT NULL DEFAULT 262144,
    max_avatar_size INTEGER NOT NULL DEFAULT 2097152,
    max_sound_size INTEGER NOT NULL DEFAULT 2097152,
    max_attachment_size INTEGER NOT NULL DEFAULT 26214400,
    max_attachments_per_message INTEGER NOT NULL DEFAULT 10,
    server_name TEXT NOT NULL DEFAULT 'Accord Server',
    registration_policy TEXT NOT NULL DEFAULT 'open',
    max_spaces INTEGER NOT NULL DEFAULT 0,
    max_members_per_space INTEGER NOT NULL DEFAULT 0,
    motd TEXT,
    public_listing BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TEXT
);

INSERT INTO server_settings (id) VALUES (1) ON CONFLICT DO NOTHING;

-- Backup codes for 2FA
CREATE TABLE IF NOT EXISTS backup_codes (
    id BIGSERIAL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL,
    used BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'))
);

CREATE INDEX IF NOT EXISTS idx_backup_codes_user ON backup_codes(user_id);

-- Channel mutes
CREATE TABLE IF NOT EXISTS channel_mutes (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    PRIMARY KEY (user_id, channel_id)
);

CREATE INDEX IF NOT EXISTS idx_channel_mutes_user ON channel_mutes(user_id);

-- Reports
CREATE TABLE IF NOT EXISTS reports (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    reporter_id TEXT NOT NULL REFERENCES users(id),
    target_type TEXT NOT NULL CHECK (target_type IN ('message', 'user')),
    target_id TEXT NOT NULL,
    channel_id TEXT,
    category TEXT NOT NULL CHECK (category IN ('csam', 'terrorism', 'fraud', 'hate', 'violence', 'self_harm', 'other')),
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'actioned', 'dismissed')),
    actioned_by TEXT REFERENCES users(id),
    action_taken TEXT,
    created_at TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    resolved_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_reports_space_status ON reports(space_id, status);
CREATE INDEX IF NOT EXISTS idx_reports_space_created ON reports(space_id, created_at DESC);

-- Read states: tracks per-user, per-channel read position
CREATE TABLE IF NOT EXISTS read_states (
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_id   TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    last_read_message_id TEXT,
    mention_count INTEGER NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    PRIMARY KEY (user_id, channel_id)
);

-- Relationships between users
CREATE TABLE IF NOT EXISTS relationships (
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    type           INTEGER NOT NULL CHECK(type IN (1, 2, 3, 4)),
    created_at     TEXT NOT NULL DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')),
    PRIMARY KEY (user_id, target_user_id)
);

CREATE INDEX IF NOT EXISTS idx_relationships_user ON relationships(user_id, type);
CREATE INDEX IF NOT EXISTS idx_relationships_target ON relationships(target_user_id, type);
