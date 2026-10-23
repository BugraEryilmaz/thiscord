ALTER TABLE guild_moderation DROP COLUMN move_revision, DROP COLUMN move_destination,
    DROP COLUMN move_source, DROP COLUMN server_deafened, DROP COLUMN server_muted;
DROP TABLE guild_audit_log;
DROP TABLE guild_invites;
ALTER TABLE channels DROP CONSTRAINT channels_category_guild_fk,
    DROP COLUMN category_id, DROP COLUMN position;
DROP TABLE channel_categories;
