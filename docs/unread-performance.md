# Unread query performance

Run from the repository root in WSL, with `TEST_DATABASE_URL` configured as in
[database.md](database.md) and a Linux `CARGO_TARGET_DIR`:

```sh
cargo test -p thiscord-backend --locked --test chat unread_ranges -- --ignored --nocapture
```

The test creates and drops its own schema in the test database. It populates
20 channels with 10,000 messages each and compares the original guild aggregation
with the exact SQL used by the application. Scenarios cover 1,000 unread messages,
a 200,000-message backlog, and fully read channels. Results must be identical.
Each scenario warms the query, then reports the median of five elapsed times,
including all round trips, query planning and result decoding. Separate
`EXPLAIN (ANALYZE, BUFFERS, TIMING OFF, FORMAT JSON)` plans report estimated query
cost, message rows examined and JIT compilation. Wall-clock thresholds are not CI
gates; range-scan row counts and absence of JIT compilation are checked instead.
The test runs with JIT enabled and without parallel workers for comparable row
accounting. It analyzes freshly inserted data without relying on a vacuum to make
the index cover all reads without heap fetches.

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

## Large backlogs

Exact counts still require work proportional to live messages above the marker,
including own messages that must be filtered out. A completely unread channel
therefore still scans its backlog, and global socket notifications multiply that
work by active subscribers. This change does not provide constant-time counts.

Maintained aggregates are a separate tradeoff: per-channel sequence buckets with
author and mention subtotals could reduce scans for large backlogs, with an exact
scan of the bucket containing the read marker. They would need transactional
updates for send, mention edits, deletion and account anonymization, as well as
backfill/reconciliation and current permission checks. Per-recipient counters
would instead amplify every write by guild membership. Neither is implemented
here; subscriber fan-out and realistic backlog distributions should determine
whether that added write cost and state are justified.
