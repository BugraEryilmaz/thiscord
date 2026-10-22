-- Rollback permanently removes attachment bytes and metadata.
DROP TABLE attachments;
ALTER TABLE messages DROP COLUMN attachments;
