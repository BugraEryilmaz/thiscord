-- Normalized mono 48 kHz s16le clips, at most five seconds. The per-guild count
-- is enforced by the backend under the guild row lock.
CREATE TABLE guild_sounds (
    guild_id UUID NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 32),
    emoji TEXT CHECK (emoji IS NULL OR char_length(emoji) BETWEEN 1 AND 8),
    pcm BYTEA NOT NULL CHECK (octet_length(pcm) BETWEEN 2 AND 480000 AND octet_length(pcm) % 2 = 0),
    duration_ms INTEGER NOT NULL CHECK (duration_ms BETWEEN 0 AND 5000),
    created_by UUID REFERENCES accounts(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, id)
);
-- Existing communities get the same default as new ones: everyone may use sounds.
UPDATE guild_roles SET permissions = permissions || '["use_soundboard"]'::jsonb
WHERE everyone AND NOT permissions ? 'use_soundboard';
