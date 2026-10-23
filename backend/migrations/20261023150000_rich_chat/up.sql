ALTER TABLE messages ADD COLUMN reply_to UUID REFERENCES messages(id) ON DELETE SET NULL;
ALTER TABLE messages ADD COLUMN pinned BOOLEAN NOT NULL DEFAULT false;
CREATE INDEX message_replies ON messages(reply_to) WHERE reply_to IS NOT NULL;
CREATE INDEX channel_pins ON messages(guild_id,channel_id,created_at,id) WHERE pinned AND NOT deleted;
CREATE TABLE message_reactions (
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL CHECK (emoji IN ('👍','👎','❤️','😂','🎉','😮','😢','🔥','✅','👀')),
    PRIMARY KEY(message_id, account_id, emoji)
);
CREATE INDEX guild_message_search ON messages USING gin(to_tsvector('simple',content)) WHERE NOT deleted;
CREATE INDEX direct_message_search ON direct_messages USING gin(to_tsvector('simple',content));
