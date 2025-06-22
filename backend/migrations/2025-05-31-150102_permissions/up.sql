-- Your SQL goes here

CREATE TABLE IF NOT EXISTS roles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    server_id UUID NOT NULL,
    UNIQUE (name, server_id),
    FOREIGN KEY (server_id) REFERENCES servers(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS user_roles (
    user_id UUID NOT NULL,
    role_id UUID NOT NULL,
    server_id UUID NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    FOREIGN KEY (role_id) REFERENCES roles(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id) REFERENCES servers(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, server_id)
);

CREATE TYPE permission_type AS ENUM (
    'DeleteServer',
    'CreateChannel',
    'DeleteChannel',
    'CreateHiddenChannel',
    'DeleteHiddenChannel',
    'ListHiddenChannels',
    'AdjustChannelPermissions',
    'ListChannels', 
    'ListUsersInServer',
    'JoinAudioChannel', 
    'JoinAudioChannelInHiddenChannels', 
    'SendMessages', 
    'SendMessagesInHiddenChannels', 
    'DeleteMessages',
    'DeleteMessagesSelf'
);

CREATE TABLE IF NOT EXISTS permissions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_id UUID NOT NULL,
    type permission_type NOT NULL,
    FOREIGN KEY (role_id) REFERENCES roles(id) ON DELETE CASCADE,
    UNIQUE (role_id, type)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_permissions_role_id_type ON permissions(role_id, type);
CREATE INDEX IF NOT EXISTS idx_permissions_role_id ON permissions(role_id);