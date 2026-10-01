CREATE TABLE e2ee_identities (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    exchange_key TEXT NOT NULL,
    signing_key TEXT NOT NULL,
    user_context TEXT NOT NULL
);
CREATE TABLE e2ee_message_tokens (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    PRIMARY KEY (user_id, token)
);
CREATE TABLE e2ee_chat_contexts (
    channel_id TEXT PRIMARY KEY REFERENCES channels(id) ON DELETE CASCADE,
    wire_id TEXT NOT NULL
);
