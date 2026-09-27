-- Deployment identity survives process restarts; authentication tables come later.
CREATE TABLE instance (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    id UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO instance (singleton) VALUES (TRUE);
