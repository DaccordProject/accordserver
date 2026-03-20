-- Fix guest_tokens columns to use TEXT instead of TIMESTAMPTZ
-- for consistency with the rest of the schema (all timestamps are stored as text).

ALTER TABLE guest_tokens
    ALTER COLUMN expires_at TYPE TEXT USING to_char(expires_at at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    ALTER COLUMN created_at TYPE TEXT USING to_char(created_at at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'),
    ALTER COLUMN created_at SET DEFAULT (to_char(now() at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS'));
