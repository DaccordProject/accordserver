CREATE TABLE attachment_deletions (url TEXT PRIMARY KEY);
CREATE FUNCTION queue_attachment_deletion() RETURNS trigger AS $$
BEGIN
    INSERT INTO attachment_deletions (url) VALUES (OLD.url) ON CONFLICT(url) DO NOTHING;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER queue_attachment_deletion AFTER DELETE ON attachments
FOR EACH ROW EXECUTE FUNCTION queue_attachment_deletion();
UPDATE plugins SET signed = FALSE;
CREATE TABLE voice_evictions (
    channel_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    PRIMARY KEY (channel_id, user_id)
);
