CREATE TABLE account_avatars (
    account_id UUID PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    id UUID NOT NULL UNIQUE,
    png BYTEA NOT NULL CHECK (octet_length(png) BETWEEN 1 AND 300000)
);
