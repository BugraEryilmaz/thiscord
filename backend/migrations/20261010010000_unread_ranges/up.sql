CREATE INDEX channel_unread_range ON messages(guild_id, channel_id, sequence)
    INCLUDE (author_id, mentions)
    WHERE NOT deleted;
