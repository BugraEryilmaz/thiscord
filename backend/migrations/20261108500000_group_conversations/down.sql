-- Permanently deletes group conversations with their messages, private files,
-- calls and system notices, then restores the friendship-only foreign keys.
DROP TRIGGER group_conversations_deleted ON group_conversations;
DROP TRIGGER friendships_deleted ON friendships;
DROP FUNCTION private_conversation_deleted();
DROP TRIGGER direct_attachments_conversation ON direct_attachments;
DROP TRIGGER direct_messages_conversation ON direct_messages;
DROP FUNCTION private_conversation_exists();
DELETE FROM direct_attachments a WHERE NOT EXISTS (SELECT 1 FROM friendships f WHERE f.id = a.conversation_id);
DELETE FROM direct_messages m WHERE NOT EXISTS (SELECT 1 FROM friendships f WHERE f.id = m.conversation_id);
ALTER TABLE direct_attachments ADD CONSTRAINT direct_attachments_conversation_id_fkey
    FOREIGN KEY (conversation_id) REFERENCES friendships(id) ON DELETE CASCADE;
ALTER TABLE direct_messages ADD CONSTRAINT direct_messages_conversation_id_fkey
    FOREIGN KEY (conversation_id) REFERENCES friendships(id) ON DELETE CASCADE;
ALTER TABLE direct_messages DROP COLUMN system;
DROP TABLE group_call_members;
DROP TABLE group_calls;
DROP TRIGGER group_member_removed ON group_members;
DROP FUNCTION group_member_removed();
DROP TABLE group_members;
DROP TABLE group_conversations;
