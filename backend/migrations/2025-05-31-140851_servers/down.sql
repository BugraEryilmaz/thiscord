-- This file should undo anything in `up.sql`
DROP INDEX IF EXISTS idx_joined_users_user_id;
DROP INDEX IF EXISTS idx_joined_users_server_id;
DROP TABLE IF EXISTS joined_users;

DROP INDEX IF EXISTS idx_servers_id;
DROP INDEX IF EXISTS idx_servers_connection_string;
DROP TABLE IF EXISTS servers;