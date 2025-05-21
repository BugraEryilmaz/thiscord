-- This file should undo anything in `up.sql`
ALTER TABLE users DROP COLUMN activated;
DROP TABLE IF EXISTS user_activations;
DROP INDEX IF EXISTS idx_activation_code;