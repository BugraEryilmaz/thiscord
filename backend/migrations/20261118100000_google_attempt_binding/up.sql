-- Bind every Google sign-in attempt to the user agent that started it
-- (docs/accounts.md#google-sign-in-setup). Browser attempts store the hash of
-- their HttpOnly attempt cookie; desktop (loopback) attempts store the hash of a
-- one-time completion secret generated when the callback succeeds. The verified
-- Google identity waits on the attempt until a bound completion applies it.
-- Attempts live five minutes and in-flight rows carry no binding, so they are
-- discarded rather than backfilled: their holders start again.
DELETE FROM oauth_attempts;
ALTER TABLE oauth_attempts
    ADD COLUMN browser_hash TEXT,
    ADD COLUMN completion_hash TEXT,
    ADD COLUMN subject TEXT,
    ADD COLUMN email TEXT,
    -- Exactly one binding: a loopback callback or a browser cookie.
    ADD CONSTRAINT oauth_attempts_one_binding
        CHECK ((callback IS NULL) <> (browser_hash IS NULL)),
    -- Only loopback attempts carry a completion secret, and every ready one does.
    ADD CONSTRAINT oauth_attempts_ready_bound
        CHECK ((callback IS NOT NULL OR completion_hash IS NULL)
            AND (status <> 'ready' OR (subject IS NOT NULL AND email IS NOT NULL
                AND (callback IS NULL OR completion_hash IS NOT NULL))));
