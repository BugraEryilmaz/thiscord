-- Preserve moderation edges when an access watch coalesces committed changes.
ALTER TABLE guild_moderation ADD COLUMN publication_revision bigint NOT NULL DEFAULT 0
    CHECK (publication_revision >= 0);
CREATE FUNCTION advance_publication_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.publication_revision := CASE WHEN NEW.server_muted OR NEW.server_deafened THEN 1 ELSE 0 END;
    ELSIF NEW.server_muted IS DISTINCT FROM OLD.server_muted
       OR NEW.server_deafened IS DISTINCT FROM OLD.server_deafened THEN
        NEW.publication_revision := OLD.publication_revision + 1;
    ELSE
        NEW.publication_revision := OLD.publication_revision;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER moderation_publication_boundary BEFORE INSERT OR UPDATE ON guild_moderation
    FOR EACH ROW EXECUTE FUNCTION advance_publication_revision();
