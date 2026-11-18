-- Account mail (verification, email-change and reset links) is limited per
-- recipient inbox: one message a minute and five an hour. Only a hash of the
-- normalized address is kept, and the worker drops rows once they are idle.
CREATE TABLE mail_recipient_limits (
    recipient_hash TEXT PRIMARY KEY,
    window_start TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent INTEGER NOT NULL DEFAULT 1 CHECK (sent > 0),
    last_sent_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Queued mail records its kind, so a newer message of the same kind for the
-- same account withdraws the older unsent one instead of being appended.
ALTER TABLE mail_outbox ADD COLUMN purpose TEXT;
UPDATE mail_outbox SET purpose = CASE
    WHEN body LIKE 'Thiscord verify email%' THEN 'verify'
    WHEN body LIKE 'Thiscord confirm your new email%' THEN 'email'
    WHEN body LIKE 'Thiscord reset password%' THEN 'reset'
    ELSE 'notice'
END;
ALTER TABLE mail_outbox
    ALTER COLUMN purpose SET NOT NULL,
    ADD CONSTRAINT mail_outbox_purpose_check
        CHECK (purpose IN ('verify', 'email', 'reset', 'notice'));
CREATE INDEX mail_outbox_account_purpose ON mail_outbox(account_id, purpose);
