-- Federate custom emoji (Phase 4). PostgreSQL variant of 029_federation_emoji.
--
-- `origin` marks an emoji row as a mirror of a remote space's emoji (NULL =
-- local, otherwise the home domain). Mirrored emoji use qualified IDs and store
-- an absolute home-server image URL in `image_path`.
ALTER TABLE emojis ADD COLUMN IF NOT EXISTS origin TEXT;

CREATE INDEX IF NOT EXISTS idx_emojis_origin ON emojis(origin);
