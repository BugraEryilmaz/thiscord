-- Ready rows hold an identity that was never applied; the previous code would
-- treat them as completed, so in-flight attempts are discarded again.
DELETE FROM oauth_attempts;
ALTER TABLE oauth_attempts
    DROP CONSTRAINT oauth_attempts_ready_bound,
    DROP CONSTRAINT oauth_attempts_one_binding,
    DROP COLUMN email,
    DROP COLUMN subject,
    DROP COLUMN completion_hash,
    DROP COLUMN browser_hash;
