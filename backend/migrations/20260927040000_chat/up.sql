CREATE TABLE messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL, channel_id UUID NOT NULL,
    author_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
    client_id UUID NOT NULL,
    request_hash TEXT NOT NULL,
    content TEXT NOT NULL CHECK(length(content)<=4000),
    mentions JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    edited_at TIMESTAMPTZ,
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    revision INTEGER NOT NULL DEFAULT 0,
    sequence BIGSERIAL NOT NULL UNIQUE,
    FOREIGN KEY(guild_id,channel_id) REFERENCES channels ON DELETE CASCADE,
    UNIQUE(author_id,client_id)
);
CREATE INDEX channel_history ON messages(guild_id,channel_id,created_at,id);
CREATE TABLE message_events (
    sequence BIGSERIAL PRIMARY KEY,
    guild_id UUID NOT NULL, channel_id UUID NOT NULL,
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    FOREIGN KEY(guild_id,channel_id) REFERENCES channels ON DELETE CASCADE
);
CREATE INDEX channel_events ON message_events(guild_id,channel_id,sequence);
CREATE TABLE channel_reads (
    guild_id UUID NOT NULL,channel_id UUID NOT NULL,account_id UUID NOT NULL,
    through BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY(guild_id,channel_id,account_id),
    FOREIGN KEY(guild_id,channel_id) REFERENCES channels ON DELETE CASCADE,
    FOREIGN KEY(guild_id,account_id) REFERENCES guild_members ON DELETE CASCADE
);
CREATE TABLE chat_presence (
    id UUID PRIMARY KEY,
    guild_id UUID NOT NULL,channel_id UUID NOT NULL,account_id UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '35 seconds',
    typing_until TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY(guild_id,channel_id) REFERENCES channels ON DELETE CASCADE,
    FOREIGN KEY(guild_id,account_id) REFERENCES guild_members ON DELETE CASCADE
);
