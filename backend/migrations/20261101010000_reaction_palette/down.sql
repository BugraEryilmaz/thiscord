-- Rolling back discards reactions outside the original ten-emoji palette.
DELETE FROM message_reactions
WHERE emoji NOT IN ('👍','👎','❤️','😂','🎉','😮','😢','🔥','✅','👀');
ALTER TABLE message_reactions DROP CONSTRAINT message_reactions_emoji_check;
ALTER TABLE message_reactions ADD CONSTRAINT message_reactions_emoji_check
    CHECK (emoji IN ('👍','👎','❤️','😂','🎉','😮','😢','🔥','✅','👀'));
