-- Federate custom emoji (Phase 4).
--
-- `origin` marks an emoji row as a mirror of a remote space's emoji (NULL =
-- local, otherwise the home domain). Mirrored emoji use qualified IDs
-- (`<snowflake>@<domain>`) and store an absolute home-server image URL in
-- `image_path` (the image itself is not mirrored — see attachments/CDN).
ALTER TABLE emojis ADD COLUMN origin TEXT;

CREATE INDEX IF NOT EXISTS idx_emojis_origin ON emojis(origin);
