-- Older binaries reject unknown permission names.
UPDATE guild_roles SET permissions = permissions - 'change_nickname' - 'manage_nicknames'
WHERE permissions ?| ARRAY['change_nickname', 'manage_nicknames'];
UPDATE channel_overrides
SET allow = allow - 'change_nickname' - 'manage_nicknames',
    deny = deny - 'change_nickname' - 'manage_nicknames'
WHERE allow ?| ARRAY['change_nickname', 'manage_nicknames']
   OR deny ?| ARRAY['change_nickname', 'manage_nicknames'];
-- Nicknames and role colours are permanently removed.
ALTER TABLE guild_members DROP COLUMN nickname;
ALTER TABLE guild_roles
    DROP CONSTRAINT guild_roles_everyone_plain,
    DROP COLUMN hoist,
    DROP COLUMN color;
