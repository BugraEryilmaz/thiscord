CREATE TABLE notification_preferences (
    account_id uuid PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    settings jsonb NOT NULL CHECK (jsonb_typeof(settings) = 'object')
);
CREATE TABLE notification_release (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    version text NOT NULL CHECK (length(version) BETWEEN 5 AND 64)
);
