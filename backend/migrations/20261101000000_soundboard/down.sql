DROP TABLE guild_sounds;
-- Older binaries reject unknown permission names.
UPDATE guild_roles SET permissions = permissions - 'use_soundboard'
WHERE permissions ? 'use_soundboard';
UPDATE channel_overrides
SET allow = allow - 'use_soundboard', deny = deny - 'use_soundboard'
WHERE allow ? 'use_soundboard' OR deny ? 'use_soundboard';
