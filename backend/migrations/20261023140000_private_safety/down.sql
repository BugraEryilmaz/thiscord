-- Permanently discards blocks, reports and private attachment bytes. Consent
-- removed by a block remains removed. Deleted message text cannot be restored.
DROP TABLE safety_reports;
DROP TABLE direct_attachments;
DROP TABLE account_blocks;
DROP INDEX direct_messages_unread_live;
DELETE FROM direct_messages WHERE deleted OR content='';
ALTER TABLE direct_messages DROP COLUMN revision, DROP COLUMN deleted,
    DROP COLUMN edited_at, DROP COLUMN attachments, DROP COLUMN request_hash,
    DROP CONSTRAINT direct_messages_content_check,
    ADD CONSTRAINT direct_messages_content_check CHECK (char_length(content) BETWEEN 1 AND 4000);
