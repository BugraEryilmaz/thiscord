# Unread query performance

Run from the repository root in WSL, with `TEST_DATABASE_URL` configured as in
[database.md](database.md) and a Linux `CARGO_TARGET_DIR`:

```sh
cargo test -p thiscord-backend --locked --test chat unread_ranges -- --ignored --nocapture
```

The test creates and drops its own schema in the test database. It populates
20 channels with 10,000 messages each, every one mentioning the reader, and
compares the original exact guild aggregation, capped at `UNREAD_COUNT_LIMIT`, with
the exact SQL used by the application. Scenarios cover 1,000 unread messages, a
200,000-message backlog, fully read channels and the same backlog after removing
every mention (vacuumed after that bulk update). Results must be identical.
Each scenario warms the query, then reports the median of five elapsed times,
including all round trips, query planning and result decoding. Separate
`EXPLAIN (ANALYZE, BUFFERS, TIMING OFF, FORMAT JSON)` plans report estimated query
cost, message rows examined and JIT compilation. Wall-clock thresholds are not CI
gates; range-scan row counts and absence of JIT compilation are checked instead.
The test runs with JIT enabled and without parallel workers for comparable row
accounting. The first three scenarios analyze freshly inserted data without
relying on a vacuum to make the index cover all reads without heap fetches.

## Uncapped range query (2026-10-10)

Measured on local PostgreSQL 17 in WSL (2026-10-10), with fresh heap pages and
JIT enabled. Times include the database round trip, planning and JSON decoding,
but not HTTP authentication or permission-state loading. They are local
measurements under a shared development workload, not latency guarantees.

| Backlog | Original median | Range median | Original message rows | Range message rows |
| --- | ---: | ---: | ---: | ---: |
| 1,000 unread | 24.160 ms | 6.166 ms | 200,000 | 1,000 |
| 200,000 unread | 87.021 ms | 77.114 ms | 200,000 | 200,000 |
| Fully read | 30.046 ms | 9.972 ms | 200,000 | 0 |

No scenario compiled JIT code. The range query's estimated cost was 127.37.

The application sends the authorized channel IDs as a bound UUID array. One SQL
statement looks up their read markers and performs a lateral aggregate for each
channel. The range predicate compares the composite `(guild_id, channel_id,
sequence)` key, with separate guild/channel equality predicates, making it
equivalent to an exclusive sequence bound within that channel. Keeping the bound
on the composite key avoids offering the global sequence index as a range access
path that must filter other channels afterward.

A trial lateral join with just `sequence > through` examined 1,901,000 rows in
this fixture, even with the partial covering index present. Its estimates can
also trigger JIT compilation on small actual backlogs. The benchmark checks both
the scan work and JIT with the final query; database-wide JIT settings are unchanged.

The covering index adds storage and write work for live messages; edits to mentions
and author anonymization update it, and soft deletion removes the live entry.
Rollback drops only the index. On a populated installation, apply the migration
in a maintenance window using Diesel before restarting the backend if building
the index would exceed startup's two-second statement timeout.

## Capped counts (2026-11-03)

The UI shows per-channel counts and an `@` flag, and the "New messages" divider
can only be placed within the first loaded page (at most 100 messages). Counts
beyond a few hundred therefore carry no additional information. `count` and
`mentions` are exact below `UNREAD_COUNT_LIMIT` (1,000, in `shared/src/chat.rs`)
and equal to it otherwise; the channel list shows "999+". Each channel's lateral
count subquery stops after 1,000 matching live rows (`LIMIT` inside the lateral),
so a completely unread channel costs at most that many index rows plus own messages
filtered on the way, instead of its whole backlog.

Mentions are counted by a second capped lateral over the new partial
`channel_unread_mentions` index (`WHERE NOT deleted AND mentions <> '[]'`, same key
and included columns). Its query repeats the `mentions <> '[]'` predicate so the
planner can prove the partial index applies. It visits only mention-bearing
messages in the range, so a large backlog without mentions is not scanned a second
time; messages mentioning other people are still visited and filtered. Mentions
stay exact below the cap and never exceed `count`. The mention lateral runs only
for channels with a non-zero count.

The benchmark asserts the visited message rows for the application query: 2,000
for 1,000 unread (count and mention scans), 40,000 for the 200,000 backlog
(20 × 1,000 per scan), 0 for fully read and 20,000 for the backlog without mentions.
These row bounds are regression gates.

Measured once on the same local PostgreSQL 17 in WSL (2026-11-04), JIT enabled,
under a shared development workload (other checkouts were building and testing).
Medians of five runs, same conditions as above; not latency guarantees.

| Backlog | Original median | Capped median | Original message rows | Capped message rows |
| --- | ---: | ---: | ---: | ---: |
| 1,000 unread | 23.697 ms | 12.255 ms | 200,000 | 2,000 |
| 200,000 unread | 87.381 ms | 11.369 ms | 200,000 | 40,000 |
| Fully read | 23.025 ms | 6.160 ms | 200,000 | 0 |
| 200,000 unread, no mentions | 85.144 ms | 2.478 ms | 200,000 | 20,000 |

No scenario compiled JIT code. The capped query's estimated cost was 252.07
(176.17 after removing mentions). The small-backlog case now visits each unread
row twice (count and mention scans), so it is slower than the uncapped range
query's 6.166 ms measured on 2026-10-10 under a different workload; the large
backlog no longer scales with its size. The last scenario's table was vacuumed,
so its time is not directly comparable to the fresh-insert rows above it.

The migration builds the partial index on startup within the usual two-second
statement timeout. On a large installation, apply it in a maintenance window with
`--migrate-only` first. Rollback drops only the index; capped counts then use the
general range index for mentions, as exact as before but scanning the backlog.

Older clients receive capped values as ordinary numbers and display "1000" for a
capped channel; the meaning of smaller values is unchanged.

## Large backlogs

Capped counts make each channel's scan constant-bounded, but notifications still
multiply that bounded work by refreshing subscribers and accessible channels.
Mentions of other people in a long unread range are still filtered row by row.

Maintained aggregates are a separate tradeoff: per-channel sequence buckets with
author and mention subtotals could reduce scans for large backlogs, with an exact
scan of the bucket containing the read marker. They would need transactional
updates for send, mention edits, deletion and account anonymization, as well as
backfill/reconciliation and current permission checks. Per-recipient counters
would instead amplify every write by guild membership. Neither is implemented
here; subscriber fan-out and realistic backlog distributions should determine
whether that added write cost and state are justified. With capped counts they
would only help channels with many unrelated mentions or very many subscribers,
so they are not implemented.
