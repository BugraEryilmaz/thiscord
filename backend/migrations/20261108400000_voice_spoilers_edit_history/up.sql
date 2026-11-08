-- Voice messages and spoiler covers are attachment metadata; edit history keeps
-- the last 20 replaced versions of a message's text (enforced by the backend).
ALTER TABLE attachments
    ADD COLUMN voice_duration_ms INTEGER CHECK (voice_duration_ms BETWEEN 1 AND 301000),
    ADD COLUMN voice_waveform TEXT CHECK (char_length(voice_waveform) <= 128),
    ADD COLUMN spoiler BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT attachments_voice_metadata
        CHECK ((voice_duration_ms IS NULL) = (voice_waveform IS NULL));

ALTER TABLE direct_attachments
    ADD COLUMN voice_duration_ms INTEGER CHECK (voice_duration_ms BETWEEN 1 AND 301000),
    ADD COLUMN voice_waveform TEXT CHECK (char_length(voice_waveform) <= 128),
    ADD COLUMN spoiler BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT direct_attachments_voice_metadata
        CHECK ((voice_duration_ms IS NULL) = (voice_waveform IS NULL));

-- Deleting a message (tombstone) deletes its rows explicitly; removing the
-- message row (channel/guild/account cascades) removes them with it.
CREATE TABLE message_edits (
    id BIGSERIAL PRIMARY KEY,
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    content TEXT NOT NULL CHECK (char_length(content) <= 4000),
    created_at TIMESTAMPTZ NOT NULL,
    replaced_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX message_edits_message ON message_edits(message_id, id DESC);

CREATE TABLE direct_message_edits (
    id BIGSERIAL PRIMARY KEY,
    message_id UUID NOT NULL REFERENCES direct_messages(id) ON DELETE CASCADE,
    content TEXT NOT NULL CHECK (char_length(content) <= 4000),
    created_at TIMESTAMPTZ NOT NULL,
    replaced_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX direct_message_edits_message ON direct_message_edits(message_id, id DESC);
