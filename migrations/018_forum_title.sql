-- Add title column to messages for forum post titles.
-- Only forum channel messages use this; it is NULL for regular messages.
ALTER TABLE messages ADD COLUMN title TEXT;
