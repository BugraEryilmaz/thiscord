-- Your SQL goes here
CREATE TABLE IF NOT EXISTS servers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    connection_string TEXT UNIQUE NOT NULL,
    image_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_servers_id ON servers(id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_servers_connection_string ON servers(connection_string);

CREATE TABLE IF NOT EXISTS joined_users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL,
    server_id UUID NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id) REFERENCES servers(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_joined_users_user_id ON joined_users(user_id);
CREATE INDEX IF NOT EXISTS idx_joined_users_server_id ON joined_users(server_id);

