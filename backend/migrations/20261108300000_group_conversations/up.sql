-- Private group conversations. Messages and private attachments keep their
-- tables; their conversation reference may now name a friendship or a group.
CREATE TABLE group_conversations (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '' CHECK (char_length(name) <= 100),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    -- Lost-response creation retries: (creator, client UUID) is unique.
    created_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
    client_id UUID NOT NULL,
    request_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_message_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (created_by, client_id)
);
CREATE TABLE group_members (
    group_id UUID NOT NULL REFERENCES group_conversations(id) ON DELETE CASCADE,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    owner BOOLEAN NOT NULL DEFAULT false,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    read_at TIMESTAMPTZ,
    PRIMARY KEY (group_id, account_id)
);
CREATE UNIQUE INDEX group_members_owner ON group_members(group_id) WHERE owner;
CREATE INDEX group_members_account ON group_members(account_id, group_id);

-- Ownership passes to the longest-standing member; an empty group is deleted
-- (with its messages and files, below). Runs for leave, removal and the
-- account-deletion cascade alike.
CREATE FUNCTION group_member_removed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM group_members WHERE group_id = OLD.group_id) THEN
        DELETE FROM group_conversations WHERE id = OLD.group_id;
    ELSIF NOT EXISTS (SELECT 1 FROM group_members WHERE group_id = OLD.group_id AND owner) THEN
        UPDATE group_members SET owner = true
        WHERE group_id = OLD.group_id AND account_id = (
            SELECT account_id FROM group_members WHERE group_id = OLD.group_id
            ORDER BY joined_at, account_id LIMIT 1);
    END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER group_member_removed AFTER DELETE ON group_members
    FOR EACH ROW EXECUTE FUNCTION group_member_removed();

-- One live call per group. Members hold ringing or joined leases; the call is
-- live while state='active', expires_at>now() and a joined lease remains.
CREATE TABLE group_calls (
    id UUID PRIMARY KEY,
    group_id UUID NOT NULL REFERENCES group_conversations(id) ON DELETE CASCADE,
    started_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
    state TEXT NOT NULL CHECK (state IN ('active','ended')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '60 seconds')
);
CREATE UNIQUE INDEX group_calls_live ON group_calls(group_id) WHERE state = 'active';
CREATE TABLE group_call_members (
    call_id UUID NOT NULL REFERENCES group_calls(id) ON DELETE CASCADE,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('ringing','joined','declined','left')),
    until TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (call_id, account_id)
);
CREATE INDEX group_call_members_account ON group_call_members(account_id) WHERE state = 'ringing';

-- Group notices: content-free rows with a typed event.
ALTER TABLE direct_messages ADD COLUMN system JSONB
    CHECK (system IS NULL OR (jsonb_typeof(system) = 'object' AND content = ''));

-- Replace the friendship-only foreign keys with an equivalent reference to
-- either conversation kind. KEY SHARE matches the lock a foreign key takes.
ALTER TABLE direct_messages DROP CONSTRAINT direct_messages_conversation_id_fkey;
ALTER TABLE direct_attachments DROP CONSTRAINT direct_attachments_conversation_id_fkey;
CREATE FUNCTION private_conversation_exists() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1 FROM friendships WHERE id = NEW.conversation_id FOR KEY SHARE;
    IF NOT FOUND THEN
        PERFORM 1 FROM group_conversations WHERE id = NEW.conversation_id FOR KEY SHARE;
        IF NOT FOUND THEN
            RAISE foreign_key_violation USING MESSAGE = 'unknown private conversation';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER direct_messages_conversation BEFORE INSERT OR UPDATE OF conversation_id
    ON direct_messages FOR EACH ROW EXECUTE FUNCTION private_conversation_exists();
CREATE TRIGGER direct_attachments_conversation BEFORE INSERT OR UPDATE OF conversation_id
    ON direct_attachments FOR EACH ROW EXECUTE FUNCTION private_conversation_exists();
CREATE FUNCTION private_conversation_deleted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM direct_attachments WHERE conversation_id = OLD.id;
    DELETE FROM direct_messages WHERE conversation_id = OLD.id;
    RETURN NULL;
END $$;
CREATE TRIGGER friendships_deleted AFTER DELETE ON friendships
    FOR EACH ROW EXECUTE FUNCTION private_conversation_deleted();
CREATE TRIGGER group_conversations_deleted AFTER DELETE ON group_conversations
    FOR EACH ROW EXECUTE FUNCTION private_conversation_deleted();
