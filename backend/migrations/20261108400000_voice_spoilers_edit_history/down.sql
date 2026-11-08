-- Permanently discards edit history and the voice/spoiler flags. Voice messages
-- remain as ordinary Ogg audio attachments; message text and files survive.
DROP TABLE direct_message_edits;
DROP TABLE message_edits;

UPDATE messages SET attachments = (
    SELECT COALESCE(jsonb_agg((a - 'voice') - 'spoiler' ORDER BY i), '[]'::jsonb)
    FROM jsonb_array_elements(attachments) WITH ORDINALITY AS e(a, i))
WHERE jsonb_path_exists(attachments, '$[*] ? (exists(@.voice) || exists(@.spoiler))');
UPDATE direct_messages SET attachments = (
    SELECT COALESCE(jsonb_agg((a - 'voice') - 'spoiler' ORDER BY i), '[]'::jsonb)
    FROM jsonb_array_elements(attachments) WITH ORDINALITY AS e(a, i))
WHERE jsonb_path_exists(attachments, '$[*] ? (exists(@.voice) || exists(@.spoiler))');

ALTER TABLE direct_attachments
    DROP CONSTRAINT direct_attachments_voice_metadata,
    DROP COLUMN spoiler,
    DROP COLUMN voice_waveform,
    DROP COLUMN voice_duration_ms;
ALTER TABLE attachments
    DROP CONSTRAINT attachments_voice_metadata,
    DROP COLUMN spoiler,
    DROP COLUMN voice_waveform,
    DROP COLUMN voice_duration_ms;
