CREATE TABLE accounts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username TEXT NOT NULL UNIQUE CHECK (username = lower(username) AND username ~ '^[a-z0-9_]{3,32}$'),
    email TEXT NOT NULL UNIQUE CHECK (email = lower(email) AND length(email) <= 254),
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 64),
    bio TEXT NOT NULL DEFAULT '' CHECK (length(bio) <= 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE identities (
    account_id UUID NOT NULL REFERENCES accounts ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider IN ('password','google')),
    subject TEXT NOT NULL,
    password_hash TEXT,
    PRIMARY KEY (provider, subject),
    UNIQUE(account_id, provider),
    CHECK ((provider = 'password') = (password_hash IS NOT NULL))
);
CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES accounts ON DELETE CASCADE,
    device TEXT NOT NULL CHECK (length(device) BETWEEN 1 AND 80),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '30 days',
    reauthenticated_at TIMESTAMPTZ,
    revoked BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE INDEX sessions_account ON sessions(account_id);
CREATE TABLE session_tokens (
    token_hash TEXT PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions ON DELETE CASCADE,
    active BOOLEAN NOT NULL DEFAULT TRUE
);
CREATE UNIQUE INDEX session_current_token ON session_tokens(session_id) WHERE active;
CREATE TABLE account_codes (
    token_hash TEXT PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES accounts ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('verify','reset')),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '30 minutes',
    UNIQUE (account_id, purpose)
);
CREATE TABLE mail_outbox (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id UUID NOT NULL REFERENCES accounts ON DELETE CASCADE,
    recipient TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE auth_limits (
    key TEXT PRIMARY KEY,
    window_start TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE oauth_attempts (
    state_hash TEXT PRIMARY KEY,
    ticket_hash TEXT NOT NULL UNIQUE,
    verifier TEXT NOT NULL,
    nonce TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('login','link','reauthenticate')),
    session_id UUID REFERENCES sessions ON DELETE CASCADE,
    account_id UUID REFERENCES accounts ON DELETE CASCADE,
    device TEXT NOT NULL,
    callback TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','processing','ready','failed')),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '5 minutes'
);
