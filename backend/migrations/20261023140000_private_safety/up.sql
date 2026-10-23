ALTER TABLE direct_messages
    DROP CONSTRAINT direct_messages_content_check,
    ADD CONSTRAINT direct_messages_content_check CHECK (char_length(content) <= 4000),
    ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    ADD COLUMN deleted BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN edited_at TIMESTAMPTZ,
    ADD COLUMN attachments JSONB NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(attachments)='array' AND jsonb_array_length(attachments)<=4),
    ADD COLUMN request_hash TEXT;
CREATE INDEX direct_messages_unread_live
    ON direct_messages(conversation_id,author_id,created_at) WHERE NOT deleted;
CREATE TABLE account_blocks (
    blocker_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    blocked_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (blocker_id,blocked_id),
    CHECK (blocker_id<>blocked_id)
);
CREATE INDEX account_blocks_incoming ON account_blocks(blocked_id,blocker_id);
CREATE TABLE direct_attachments (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES friendships(id) ON DELETE CASCADE,
    owner_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id UUID REFERENCES direct_messages(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 1 AND 26214400),
    data BYTEA NOT NULL CHECK (octet_length(data)=size_bytes),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now()+interval '1 hour')
);
CREATE INDEX direct_attachments_owner ON direct_attachments(owner_id) WHERE message_id IS NULL;
CREATE INDEX direct_attachments_message ON direct_attachments(message_id);
CREATE INDEX direct_attachments_expiry ON direct_attachments(expires_at) WHERE message_id IS NULL;
CREATE TABLE safety_reports (
    id UUID PRIMARY KEY,
    reporter_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
    subject_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
    guild_id UUID REFERENCES guilds(id) ON DELETE CASCADE,
    channel_id UUID,
    target JSONB NOT NULL,
    reason TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 2000),
    evidence JSONB,
    state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','resolved','dismissed')),
    resolution TEXT NOT NULL DEFAULT '' CHECK (char_length(resolution)<=2000),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision>0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    resolved_at TIMESTAMPTZ,
    reviewer_id UUID REFERENCES accounts(id) ON DELETE SET NULL,
    CHECK ((guild_id IS NULL)=(channel_id IS NULL)),
    FOREIGN KEY (guild_id,channel_id) REFERENCES channels(guild_id,id) ON DELETE CASCADE
);
CREATE INDEX safety_reports_inbox ON safety_reports(guild_id,channel_id,created_at,id);
CREATE INDEX safety_reports_instance ON safety_reports(created_at,id) WHERE guild_id IS NULL;
CREATE INDEX safety_reports_reporter ON safety_reports(reporter_id,created_at,id);
