-- Destructive only for scripting bots. Authored messages retain anonymized text.
DELETE FROM accounts WHERE is_bot;
DROP TABLE scripting_bots;
DROP FUNCTION delete_scripting_bot_account();
ALTER TABLE accounts DROP CONSTRAINT bot_unverified;
ALTER TABLE accounts DROP COLUMN is_bot;
