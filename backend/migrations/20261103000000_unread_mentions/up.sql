-- Bounded unread mentions scan only messages that mention someone (see
-- docs/unread-performance.md). Same key and covered columns as channel_unread_range.
CREATE INDEX channel_unread_mentions ON messages(guild_id, channel_id, sequence)
    INCLUDE (author_id, mentions)
    WHERE NOT deleted AND mentions <> '[]'::jsonb;
