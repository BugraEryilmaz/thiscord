-- Removing this column removes password protection from existing guilds.
ALTER TABLE guilds DROP COLUMN password_hash;
