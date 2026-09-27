-- NULL means anyone with a verified account and the guild UUID may join.
ALTER TABLE guilds ADD COLUMN password_hash TEXT;
