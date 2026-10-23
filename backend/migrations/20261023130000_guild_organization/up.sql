CREATE TABLE channel_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    position INTEGER NOT NULL DEFAULT 0 CHECK (position BETWEEN 0 AND 10000),
    UNIQUE(guild_id,id)
);
ALTER TABLE channels ADD COLUMN category_id UUID,
    ADD COLUMN position INTEGER NOT NULL DEFAULT 0 CHECK (position BETWEEN 0 AND 10000),
    ADD CONSTRAINT channels_category_guild_fk FOREIGN KEY(guild_id,category_id)
        REFERENCES channel_categories(guild_id,id);
CREATE INDEX channels_organization ON channels(guild_id,position,id);
CREATE TABLE guild_invites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL,
    max_uses INTEGER NOT NULL CHECK (max_uses BETWEEN 1 AND 500),
    uses INTEGER NOT NULL DEFAULT 0 CHECK (uses >= 0 AND uses <= max_uses),
    revoked BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX guild_invites_guild ON guild_invites(guild_id,created_at,id);
CREATE TABLE guild_audit_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    actor UUID REFERENCES accounts(id) ON DELETE SET NULL,
    target UUID REFERENCES accounts(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (char_length(reason) <= 500),
    details TEXT NOT NULL CHECK (char_length(details) <= 1000)
);
CREATE INDEX guild_audit_order ON guild_audit_log(guild_id,created_at,id);
ALTER TABLE guild_moderation ADD COLUMN server_muted BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN server_deafened BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN move_source UUID,
    ADD COLUMN move_destination UUID,
    ADD COLUMN move_revision BIGINT;
