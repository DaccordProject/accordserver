-- Spaces (guilds equivalent)
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
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Roles
CREATE TABLE IF NOT EXISTS roles (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    color INTEGER NOT NULL DEFAULT 0,
    hoist INTEGER NOT NULL DEFAULT 0,
    icon TEXT,
    position INTEGER NOT NULL DEFAULT 0,
    permissions TEXT NOT NULL DEFAULT '[]',  -- JSON array of permission strings
    managed INTEGER NOT NULL DEFAULT 0,
    mentionable INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_roles_space_id ON roles(space_id);

-- Members
CREATE TABLE IF NOT EXISTS members (
    user_id TEXT NOT NULL REFERENCES users(id),
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    nickname TEXT,
    avatar TEXT,
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    premium_since TEXT,
    deaf INTEGER NOT NULL DEFAULT 0,
    mute INTEGER NOT NULL DEFAULT 0,
    pending INTEGER NOT NULL DEFAULT 0,
    timed_out_until TEXT,
    PRIMARY KEY (user_id, space_id)
);

CREATE INDEX idx_members_space_id ON members(space_id);
CREATE INDEX idx_members_user_id ON members(user_id);

-- Member roles junction table
CREATE TABLE IF NOT EXISTS member_roles (
    user_id TEXT NOT NULL,
    space_id TEXT NOT NULL,
    role_id TEXT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, space_id, role_id),
    FOREIGN KEY (user_id, space_id) REFERENCES members(user_id, space_id) ON DELETE CASCADE
);

-- Expand users table
ALTER TABLE users ADD COLUMN avatar TEXT;
ALTER TABLE users ADD COLUMN banner TEXT;
ALTER TABLE users ADD COLUMN accent_color INTEGER;
ALTER TABLE users ADD COLUMN bio TEXT;
ALTER TABLE users ADD COLUMN bot INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN system INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN flags INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN public_flags INTEGER NOT NULL DEFAULT 0;

-- Expand channels table
ALTER TABLE channels ADD COLUMN type TEXT NOT NULL DEFAULT 'text';
ALTER TABLE channels ADD COLUMN space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE;
ALTER TABLE channels ADD COLUMN topic TEXT;
ALTER TABLE channels ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN parent_id TEXT REFERENCES channels(id) ON DELETE SET NULL;
ALTER TABLE channels ADD COLUMN nsfw INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN rate_limit INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN bitrate INTEGER;
ALTER TABLE channels ADD COLUMN user_limit INTEGER;
ALTER TABLE channels ADD COLUMN owner_id TEXT REFERENCES users(id);
ALTER TABLE channels ADD COLUMN last_message_id TEXT;
ALTER TABLE channels ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN auto_archive_after INTEGER;

CREATE INDEX idx_channels_space_id ON channels(space_id);
CREATE INDEX idx_channels_parent_id ON channels(parent_id);

-- Permission overwrites
CREATE TABLE IF NOT EXISTS permission_overwrites (
    id TEXT NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK(type IN ('role', 'member')),
    allow TEXT NOT NULL DEFAULT '[]',  -- JSON array
    deny TEXT NOT NULL DEFAULT '[]',   -- JSON array
    PRIMARY KEY (id, channel_id)
);

-- Expand messages table
ALTER TABLE messages ADD COLUMN space_id TEXT REFERENCES spaces(id) ON DELETE CASCADE;
ALTER TABLE messages ADD COLUMN type TEXT NOT NULL DEFAULT 'default';
ALTER TABLE messages ADD COLUMN tts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN mention_everyone INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN mentions TEXT NOT NULL DEFAULT '[]';       -- JSON array of user IDs
ALTER TABLE messages ADD COLUMN mention_roles TEXT NOT NULL DEFAULT '[]';  -- JSON array of role IDs
ALTER TABLE messages ADD COLUMN embeds TEXT NOT NULL DEFAULT '[]';         -- JSON array of embed objects
ALTER TABLE messages ADD COLUMN reply_to TEXT REFERENCES messages(id) ON DELETE SET NULL;
ALTER TABLE messages ADD COLUMN flags INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN webhook_id TEXT;
ALTER TABLE messages ADD COLUMN edited_at TEXT;

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

CREATE INDEX idx_attachments_message_id ON attachments(message_id);

-- Reactions
CREATE TABLE IF NOT EXISTS reactions (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    emoji_id TEXT,
    emoji_name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (message_id, user_id, emoji_name)
);

CREATE INDEX idx_reactions_message_id ON reactions(message_id);

-- Bans
CREATE TABLE IF NOT EXISTS bans (
    user_id TEXT NOT NULL REFERENCES users(id),
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    reason TEXT,
    banned_by TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, space_id)
);

-- Invites
CREATE TABLE IF NOT EXISTS invites (
    code TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    inviter_id TEXT REFERENCES users(id),
    max_uses INTEGER,
    uses INTEGER NOT NULL DEFAULT 0,
    max_age INTEGER,
    temporary INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT
);

CREATE INDEX idx_invites_space_id ON invites(space_id);
CREATE INDEX idx_invites_channel_id ON invites(channel_id);

-- Emojis
CREATE TABLE IF NOT EXISTS emojis (
    id TEXT PRIMARY KEY NOT NULL,
    space_id TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    animated INTEGER NOT NULL DEFAULT 0,
    managed INTEGER NOT NULL DEFAULT 0,
    available INTEGER NOT NULL DEFAULT 1,
    require_colons INTEGER NOT NULL DEFAULT 1,
    creator_id TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_emojis_space_id ON emojis(space_id);

-- Emoji role restrictions junction table
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
    bot_public INTEGER NOT NULL DEFAULT 1,
    owner_id TEXT NOT NULL REFERENCES users(id),
    flags INTEGER NOT NULL DEFAULT 0,
    bot_user_id TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Bot tokens
CREATE TABLE IF NOT EXISTS bot_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    application_id TEXT NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_bot_tokens_user_id ON bot_tokens(user_id);

-- User tokens (OAuth2 access tokens)
CREATE TABLE IF NOT EXISTS user_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    refresh_token_hash TEXT,
    scopes TEXT NOT NULL DEFAULT '[]',
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_user_tokens_user_id ON user_tokens(user_id);

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
    pinned_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (channel_id, message_id)
);
