-- No read state existed before this migration. Retained incoming messages are
-- unread until the recipient views the conversation.
ALTER TABLE friendships
    ADD COLUMN low_read_at timestamptz,
    ADD COLUMN high_read_at timestamptz;

-- Bound each badge query to 100 incoming rows without scanning own messages.
CREATE INDEX direct_messages_unread
    ON direct_messages (conversation_id, author_id, created_at);
