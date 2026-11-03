-- One optional custom status per account. Expired rows are treated as cleared by
-- every read; replacing or clearing the status rewrites/deletes the row.
CREATE TABLE account_custom_statuses (
    account_id UUID PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    emoji TEXT CHECK (emoji IS NULL OR char_length(emoji) BETWEEN 1 AND 16),
    status_text TEXT NOT NULL DEFAULT '' CHECK (char_length(status_text) <= 128),
    expires_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (emoji IS NOT NULL OR status_text <> '')
);
