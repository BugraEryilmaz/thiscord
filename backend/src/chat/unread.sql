SELECT jsonb_build_object('channel_id', ch.id, 'count', unread.count, 'mentions', mentioned.count) AS data
FROM unnest($3::uuid[]) AS ch(id)
LEFT JOIN channel_reads r
  ON r.guild_id = $1::uuid AND r.channel_id = ch.id AND r.account_id = $2::uuid
-- Counts are exact below UNREAD_COUNT_LIMIT (1000) and equal to it for larger
-- backlogs, so each channel visits at most that many live unread rows.
CROSS JOIN LATERAL (
  SELECT count(*) AS count
  FROM (
    SELECT 1
    FROM messages m
    WHERE m.guild_id = $1::uuid AND m.channel_id = ch.id
      -- With the equality predicates above this is exactly sequence > through.
      -- Keep the bound on the composite key: a standalone sequence predicate can
      -- favor the global sequence index and scan other channels' history.
      AND (m.guild_id, m.channel_id, m.sequence) > ($1::uuid, ch.id, COALESCE(r.through, 0))
      AND NOT m.deleted AND m.author_id IS DISTINCT FROM $2::uuid
    LIMIT 1000
  ) capped
) unread
-- Mentions use the partial index of mention-bearing messages, so a large backlog
-- without mentions is not scanned again. Same range, filters and limit.
CROSS JOIN LATERAL (
  SELECT count(*) AS count
  FROM (
    SELECT 1
    FROM messages m
    WHERE m.guild_id = $1::uuid AND m.channel_id = ch.id
      AND (m.guild_id, m.channel_id, m.sequence) > ($1::uuid, ch.id, COALESCE(r.through, 0))
      AND NOT m.deleted AND m.mentions <> '[]'::jsonb
      AND m.author_id IS DISTINCT FROM $2::uuid
      AND m.mentions @> jsonb_build_array($2::uuid)
    LIMIT 1000
  ) capped
) mentioned
WHERE unread.count > 0
ORDER BY ch.id
