-- Rolling back discards boundary history; restart voice connections on rollback.
DROP TRIGGER moderation_publication_boundary ON guild_moderation;
DROP FUNCTION advance_publication_revision();
ALTER TABLE guild_moderation DROP COLUMN publication_revision;
