-- The backend validates reactions against the shared emoji palette, which now
-- grows without schema changes. Keep a database bound on stored values.
ALTER TABLE message_reactions DROP CONSTRAINT message_reactions_emoji_check;
ALTER TABLE message_reactions ADD CONSTRAINT message_reactions_emoji_check
    CHECK (char_length(emoji) BETWEEN 1 AND 16);
