-- Restrictions survive leaving/rejoining; account/guild deletion removes them.
CREATE TABLE guild_moderation (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    banned BOOLEAN NOT NULL DEFAULT FALSE,
    timeout_until TIMESTAMPTZ,
    voice_revision BIGINT NOT NULL DEFAULT 0 CHECK (voice_revision >= 0),
    PRIMARY KEY (guild_id, account_id)
);
