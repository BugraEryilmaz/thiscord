-- Per-account published attachment quota (per guild and per private conversation).
-- Index-only sums over size_bytes; neither scans the table nor reads `data`.
-- Startup connections cap statements at 2s; allow a bounded build on large tables.
SET LOCAL statement_timeout = '60s';
CREATE INDEX attachments_published_owner
    ON attachments(owner_id, guild_id) INCLUDE (size_bytes) WHERE message_id IS NOT NULL;
CREATE INDEX direct_attachments_published_owner
    ON direct_attachments(owner_id, conversation_id) INCLUDE (size_bytes) WHERE message_id IS NOT NULL;
