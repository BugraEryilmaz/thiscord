CREATE TABLE instance_role_limits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 80),
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'user')),
    builtin BOOLEAN NOT NULL DEFAULT FALSE,
    owned_guilds INTEGER NOT NULL CHECK (owned_guilds BETWEEN 1 AND 100),
    text_channels INTEGER NOT NULL CHECK (text_channels BETWEEN 1 AND 100),
    voice_channels INTEGER NOT NULL CHECK (voice_channels BETWEEN 1 AND 100),
    members INTEGER NOT NULL CHECK (members BETWEEN 1 AND 500),
    roles INTEGER NOT NULL CHECK (roles BETWEEN 1 AND 100),
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    CHECK (role <> 'owner' OR builtin)
);
CREATE UNIQUE INDEX instance_role_names ON instance_role_limits(lower(name));
CREATE UNIQUE INDEX instance_builtin_roles ON instance_role_limits(role) WHERE builtin;
INSERT INTO instance_role_limits(name, role, builtin, owned_guilds, text_channels, voice_channels, members, roles)
VALUES ('Instance owner', 'owner', TRUE, 100, 100, 100, 500, 100),
       ('Administrator', 'admin', TRUE, 10, 25, 25, 250, 50),
       ('Free', 'user', TRUE, 1, 2, 2, 25, 10);
CREATE TABLE instance_role_assignments (
    account_id UUID PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    role_id UUID NOT NULL REFERENCES instance_role_limits(id) ON DELETE RESTRICT
);
CREATE INDEX instance_role_assignees ON instance_role_assignments(role_id);
