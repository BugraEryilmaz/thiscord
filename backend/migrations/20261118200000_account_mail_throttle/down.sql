-- Discards the per-recipient counters; queued mail keeps its rows.
DROP INDEX mail_outbox_account_purpose;
ALTER TABLE mail_outbox DROP COLUMN purpose;
DROP TABLE mail_recipient_limits;
