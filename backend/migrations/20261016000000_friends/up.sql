CREATE TABLE friendships (
    id UUID PRIMARY KEY,
    low_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    high_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    requester UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('pending','accepted','removed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_message_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (low_id < high_id),
    CHECK (requester IN (low_id, high_id)),
    UNIQUE (low_id, high_id)
);
CREATE INDEX friendships_high ON friendships(high_id);
CREATE TABLE direct_messages (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES friendships(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    client_id UUID NOT NULL,
    content TEXT NOT NULL CHECK (char_length(content) BETWEEN 1 AND 4000),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (author_id, client_id)
);
CREATE INDEX direct_history ON direct_messages(conversation_id, created_at DESC, id DESC);
CREATE TABLE friend_calls (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES friendships(id) ON DELETE CASCADE,
    caller UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    callee UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('ringing','active','ended')),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '45 seconds'),
    caller_until TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '60 seconds'),
    callee_until TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '60 seconds'),
    CHECK (caller <> callee)
);
CREATE INDEX friend_calls_conversation ON friend_calls(conversation_id);
