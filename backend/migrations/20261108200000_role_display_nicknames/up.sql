-- Role display settings and per-server nicknames.
-- Colours are opaque sRGB integers (0xRRGGBB); NULL means no colour.
ALTER TABLE guild_roles
    ADD COLUMN color INTEGER CHECK (color BETWEEN 0 AND 16777215),
    ADD COLUMN hoist BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT guild_roles_everyone_plain CHECK (NOT (everyone AND (hoist OR color IS NOT NULL)));
-- Before this release every assigned role formed its own member-list section.
-- Existing roles keep showing separately; new roles start in Online/Offline.
UPDATE guild_roles SET hoist = TRUE WHERE NOT everyone;

-- A nickname belongs to one membership: leaving or being removed drops it.
-- Validation matches display names (trimmed, 1-64 characters, no control
-- characters); the backend enforces the same rule before writing.
ALTER TABLE guild_members
    ADD COLUMN nickname TEXT CHECK (
        nickname IS NULL
        OR (char_length(nickname) BETWEEN 1 AND 64 AND nickname = btrim(nickname) AND nickname !~ '[[:cntrl:]]')
    );

-- Members may set their own nickname by default, like other everyday actions.
-- Managing other members' nicknames is not granted by default.
UPDATE guild_roles SET permissions = permissions || '["change_nickname"]'::jsonb
WHERE everyone AND NOT permissions ? 'change_nickname';
