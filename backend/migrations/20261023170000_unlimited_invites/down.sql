-- Older clients cannot represent unlimited invites. Revoke and bound them;
-- never turn an unlimited capability into an unexpectedly valid limited one.
UPDATE guild_invites SET revoked = TRUE, expires_at = COALESCE(expires_at, clock_timestamp()),
    max_uses = CASE WHEN max_uses = 0 THEN 500 ELSE max_uses END,
    uses = LEAST(uses, CASE WHEN max_uses = 0 THEN 500 ELSE max_uses END)
    WHERE expires_at IS NULL OR max_uses = 0;
ALTER TABLE guild_invites ALTER COLUMN expires_at SET NOT NULL;
ALTER TABLE guild_invites DROP CONSTRAINT guild_invites_max_uses_check;
ALTER TABLE guild_invites DROP CONSTRAINT guild_invites_check;
ALTER TABLE guild_invites ADD CONSTRAINT guild_invites_max_uses_check CHECK (max_uses BETWEEN 1 AND 500);
ALTER TABLE guild_invites ADD CONSTRAINT guild_invites_check CHECK (uses >= 0 AND uses <= max_uses);
