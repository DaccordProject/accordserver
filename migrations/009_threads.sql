-- Add thread_id column to messages for thread support.
-- A message with thread_id = NULL is a normal channel message.
-- A message with thread_id set is a reply within the thread started by that parent message.
ALTER TABLE messages ADD COLUMN thread_id TEXT REFERENCES messages(id) ON DELETE CASCADE;
CREATE INDEX idx_messages_thread_id ON messages(thread_id);
