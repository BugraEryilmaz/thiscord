ALTER TABLE instance ADD COLUMN owner_account_id UUID REFERENCES accounts(id) ON DELETE RESTRICT;
CREATE TABLE instance_admins (account_id UUID PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE);
CREATE TABLE guilds (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    owner UUID NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE guild_members (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(guild_id, account_id)
);
ALTER TABLE guilds ADD CONSTRAINT guild_owner_member FOREIGN KEY (id, owner)
    REFERENCES guild_members(guild_id, account_id) DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE guild_roles (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 10000),
    everyone BOOLEAN NOT NULL DEFAULT FALSE,
    permissions JSONB NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(permissions) = 'array'),
    PRIMARY KEY(guild_id, id),
    CHECK (everyone = (position = 0))
);
CREATE UNIQUE INDEX guild_everyone ON guild_roles(guild_id) WHERE everyone;
CREATE TABLE guild_member_roles (
    guild_id UUID NOT NULL, account_id UUID NOT NULL, role_id UUID NOT NULL,
    PRIMARY KEY(guild_id, account_id, role_id),
    FOREIGN KEY(guild_id, account_id) REFERENCES guild_members ON DELETE CASCADE,
    FOREIGN KEY(guild_id, role_id) REFERENCES guild_roles ON DELETE CASCADE
);
CREATE TABLE channels (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    kind TEXT NOT NULL CHECK (kind IN ('text','voice')),
    PRIMARY KEY(guild_id, id)
);
CREATE TABLE channel_overrides (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    guild_id UUID NOT NULL, channel_id UUID NOT NULL,
    role_id UUID, account_id UUID,
    allow JSONB NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(allow) = 'array'),
    deny JSONB NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(deny) = 'array'),
    CHECK ((role_id IS NULL) <> (account_id IS NULL)),
    FOREIGN KEY(guild_id, channel_id) REFERENCES channels ON DELETE CASCADE,
    FOREIGN KEY(guild_id, role_id) REFERENCES guild_roles ON DELETE CASCADE,
    FOREIGN KEY(guild_id, account_id) REFERENCES guild_members ON DELETE CASCADE
);
CREATE UNIQUE INDEX channel_role_override ON channel_overrides(guild_id, channel_id, role_id) WHERE role_id IS NOT NULL;
CREATE UNIQUE INDEX channel_member_override ON channel_overrides(guild_id, channel_id, account_id) WHERE account_id IS NOT NULL;
CREATE INDEX member_guilds ON guild_members(account_id, guild_id);
