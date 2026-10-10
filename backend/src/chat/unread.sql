SELECT jsonb_build_object('channel_id', ch.id, 'count', unread.count, 'mentions', unread.mentions) AS data
FROM unnest($3::uuid[]) AS ch(id)
LEFT JOIN channel_reads r
  ON r.guild_id = $1::uuid AND r.channel_id = ch.id AND r.account_id = $2::uuid
CROSS JOIN LATERAL (
  SELECT count(*) AS count,
         count(*) FILTER (WHERE m.mentions @> jsonb_build_array($2::uuid)) AS mentions
  FROM messages m
  WHERE m.guild_id = $1::uuid AND m.channel_id = ch.id
    -- With the equality predicates above this is exactly sequence > through.
    -- Keep the bound on the composite key: a standalone sequence predicate can
    -- favor the global sequence index and scan other channels' history.
    AND (m.guild_id, m.channel_id, m.sequence) > ($1::uuid, ch.id, COALESCE(r.through, 0))
    AND NOT m.deleted AND m.author_id IS DISTINCT FROM $2::uuid
  HAVING count(*) > 0
) unread
ORDER BY ch.id
