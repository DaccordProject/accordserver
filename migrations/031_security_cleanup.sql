-- Durable cleanup includes cascaded message, channel, space and account deletion.
CREATE TABLE attachment_deletions (url TEXT PRIMARY KEY);
CREATE TRIGGER queue_attachment_deletion AFTER DELETE ON attachments
BEGIN
    INSERT INTO attachment_deletions (url) VALUES (OLD.url) ON CONFLICT(url) DO NOTHING;
END;
-- Earlier releases accepted unverified, uploader-controlled signature metadata.
UPDATE plugins SET signed = FALSE;
CREATE TABLE voice_evictions (
    channel_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    PRIMARY KEY (channel_id, user_id)
);
