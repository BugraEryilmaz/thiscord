-- This file should undo anything in `up.sql`
DROP INDEX IF EXISTS idx_users_username;
DROP TABLE IF EXISTS users;