-- Zero uses means unlimited; NULL expiry means the invite does not expire.
ALTER TABLE guild_invites ALTER COLUMN expires_at DROP NOT NULL;
ALTER TABLE guild_invites DROP CONSTRAINT guild_invites_max_uses_check;
ALTER TABLE guild_invites DROP CONSTRAINT guild_invites_check;
ALTER TABLE guild_invites ADD CONSTRAINT guild_invites_max_uses_check CHECK (max_uses BETWEEN 0 AND 500);
ALTER TABLE guild_invites ADD CONSTRAINT guild_invites_check CHECK (uses >= 0 AND (max_uses = 0 OR uses <= max_uses));
