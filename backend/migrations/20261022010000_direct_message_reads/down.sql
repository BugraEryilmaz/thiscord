-- Rollback discards read positions but retains friendships and message history.
DROP INDEX direct_messages_unread;
ALTER TABLE friendships DROP COLUMN low_read_at, DROP COLUMN high_read_at;
