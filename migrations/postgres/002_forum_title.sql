-- Add title column to messages for forum post titles.
ALTER TABLE messages ADD COLUMN IF NOT EXISTS title TEXT;
