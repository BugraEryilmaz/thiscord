ALTER TABLE accounts ADD COLUMN is_bot BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE accounts ADD CONSTRAINT bot_unverified CHECK (NOT is_bot OR NOT email_verified);

CREATE TABLE scripting_bots (
    account_id UUID PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    creator_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX scripting_bots_guild ON scripting_bots(guild_id);

-- Guild deletion also removes its nonhuman accounts and credentials. Removing
-- membership alone retains the installation for explicit management/deletion.
CREATE FUNCTION delete_scripting_bot_account() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM accounts WHERE id=OLD.account_id AND is_bot;
    RETURN OLD;
END;
$$;
CREATE TRIGGER scripting_bot_cleanup AFTER DELETE ON scripting_bots
    FOR EACH ROW EXECUTE FUNCTION delete_scripting_bot_account();
