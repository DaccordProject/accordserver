-- Repair channels.last_message_id so it only references top-level messages.
-- Prior versions bumped this pointer on every message insert, including thread
-- replies. That caused get_unread_channels() to report channels as unread after
-- thread activity, even though the channel's main timeline had no new messages.
UPDATE channels
SET last_message_id = (
    SELECT m.id
    FROM messages m
    WHERE m.channel_id = channels.id
      AND m.thread_id IS NULL
    ORDER BY m.id DESC
    LIMIT 1
);
