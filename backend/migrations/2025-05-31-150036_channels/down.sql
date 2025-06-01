-- This file should undo anything in `up.sql`
DROP INDEX IF EXISTS idx_channels_server_id;
DROP INDEX IF EXISTS idx_channels_hidden_server_id;
DROP TABLE IF EXISTS channels;
DROP TYPE IF EXISTS channel_type;