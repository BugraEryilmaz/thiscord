ALTER TABLE messages ADD COLUMN attachments JSONB NOT NULL DEFAULT '[]'
    CHECK(jsonb_typeof(attachments) = 'array' AND jsonb_array_length(attachments) <= 4);

CREATE TABLE attachments (
    id UUID PRIMARY KEY,
    guild_id UUID NOT NULL,
    channel_id UUID NOT NULL,
    owner_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
    message_id UUID REFERENCES messages(id) ON DELETE CASCADE,
    filename TEXT NOT NULL CHECK(octet_length(filename) BETWEEN 1 AND 255),
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK(size_bytes BETWEEN 1 AND 26214400),
    data BYTEA NOT NULL CHECK(octet_length(data) = size_bytes),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '1 hour',
    FOREIGN KEY(guild_id,channel_id) REFERENCES channels ON DELETE CASCADE
);
CREATE INDEX attachments_message ON attachments(message_id) WHERE message_id IS NOT NULL;
CREATE INDEX attachments_staged_owner ON attachments(owner_id) WHERE message_id IS NULL;
CREATE INDEX attachments_expiration ON attachments(expires_at) WHERE message_id IS NULL;
