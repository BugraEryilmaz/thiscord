DROP INDEX direct_message_search;
DROP INDEX guild_message_search;
DROP TABLE message_reactions;
DROP INDEX channel_pins;
DROP INDEX message_replies;
ALTER TABLE messages DROP COLUMN pinned;
ALTER TABLE messages DROP COLUMN reply_to;
