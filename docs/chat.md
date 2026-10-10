# Text chat and sockets

Select a text channel from a server's channel list. Messages render as plain text;
HTML, links and Markdown are not executed. Use `@username` to mention a guild
member. Server views show profile display names; usernames remain the handles for
mentions and member lookup. Older server responses fall back to usernames.
Mentioned messages are highlighted; channel badges show unread counts and
an `@` for unread mentions. Online/typing indicators describe people viewing the
same channel, not deployment-wide availability. Voice uses its separate media transport.

The composer shows sending/failed states. Retry reuses the original client UUID,
so a response lost after commit cannot create a duplicate. Drafts and pending sends
are held in memory for the open channel, not across channel changes/app restarts.
Edit/delete controls reflect current permissions; the backend checks them again.
Deletion requires confirmation and leaves a content-free tombstone. Loading older
history preserves scroll position; incoming messages follow only near the bottom.
The open channel retains at most 500 server messages in an ID-indexed, ordered
cache. The list renders keyed reactive rows for the viewport plus 400 pixels of
overscan on each side. Row heights are measured for wrapping, edits and resizing;
spacers represent unmounted rows, and history prepends preserve a message anchor.
Pages merge once and share deferred scroll work. Edits update existing row state.
Following live chat evicts the oldest rows and preserves a cursor to reload them.
Paging back evicts the newest rows; when a full window is being read, newer live
messages also wait outside that window. Jump to latest obtains a fresh socket
snapshot before acknowledging reads. Reconnect snapshots cancel stale history
requests. Only the selected channel has a resident cache.
Read acknowledgements require focus and the bottom of the view. Jump to latest
marks the channel read.

## Contracts and persistence

`POST /api/v1/chat` takes `ChatRequest` and returns `ChatResponse` from
`shared/src/chat.rs`. Commands cover send, edit, delete, history, read and unread.
They require bearer authentication and use the standard error envelope/request ID.
Message content, tokens and socket frames are never logged or derived as Debug.

The migration adds messages, a durable event log, read markers and short-lived
presence. Server UUIDs identify messages; server sequences order them. Channel
timestamps are monotonic under the channel write lock, with UUID as a stable tie-breaker.
Client UUIDs are unique per author across channels. An original-content hash detects
reuse with a different payload; an exact retry returns the current message,
including edits/deletion. Edits/deletes require the current revision; stale writes
return 409. Authors need EditOwnMessages/DeleteOwnMessages; ManageMessages permits
deleting others' messages, not impersonating them through edits.

History returns the latest 50 messages by default (maximum 100), ascending within
each page. Pass the shared `older` PageCursor as `before` for the preceding page.
The query uses a strict `(created_at,id) < cursor` boundary, fetches one extra row,
and reverses the descending query result. It never uses offsets.

Routine operations hold shared account/session and guild locks; guild permission
mutations retain exclusive guild locking and revision checks. Sends/edits/deletes
serialize on the requested channel, while the author/client unique key also guards
cross-channel retries. History snapshots take a shared channel lock; other channels
remain available. The existing in-process access gate still excludes delivery
while HTTP handlers can change access or revoke a replayed session.

Every operation checks membership, text-channel type, ViewChannel and its action
permission in the same locked transaction. History, unread counts and subscriptions
also require ReadHistory. Cross-channel/guild IDs are rejected. Read positions are
monotonic and clamped to the channel's latest sequence. Mentions resolve only
against current guild usernames, at most 20 per message. Unread counts/mentions
are filtered by the requesting account's current channel permissions.

Unread queries select authorized text channels before reading messages, then use
each channel's exclusive sequence range above its read marker (zero if absent).
The partial `channel_unread_range` index excludes tombstones and covers author and
mention checks. Own messages are excluded with `IS DISTINCT FROM`, so retained
messages from deleted accounts still count. Edits use current mentions; empty
counts are omitted. Results are ordered by channel ID to keep socket comparisons
stable. Both HTTP and socket updates share this query and the existing guild lock.

See [unread query measurements](unread-performance.md) for the reproducible
PostgreSQL benchmark and the remaining large-backlog tradeoff.

Account deletion anonymizes authors (Deleted account) but retains message content.
Guild/channel deletion cascades messages, events, read markers and presence.
Migration rollback permanently deletes chat data.

## Real-time transport

`/api/v1/socket` upgrades only an explicitly allowed Origin. The app opens one
socket after login/session restoration, even before a guild/channel is selected,
and closes it on logout/token replacement. Its first version-1 `connect` frame
contains the bearer token; tokens never appear in URLs. `authenticated` confirms
session validation. `subscribe` replaces the selected guild and optional text
channel without replacing the socket. Null guild/channel unsubscribes everything.
An incrementing subscription ID labels snapshots and updates; the UI ignores
late events from an old view. Every subscription is authorized server-side.

`subscribed` returns the channel history snapshot and effective permissions.
`update` wraps unread counts, message events and presence for that subscription.
Guild-only subscriptions receive permission-filtered unread/mention counts even
when no text channel is open. Read acknowledgements update other devices' badges.
There is no five-second HTTP unread poll or per-channel permissions-preview fetch.
User commands (send/edit/delete/read and loading older history) still use HTTP.

Committed chat changes use bounded in-process watches registered per subscription.
The blocking database worker publishes after commit, even if the HTTP handler was
cancelled while the transaction was running. Failed transactions publish nothing.
Message changes wake message delivery only in that channel and unread delivery in
that guild. Presence/typing changes wake only that channel's presence readers.
Read acknowledgements wake only that account's subscriptions in the affected guild.
Unread work is coalesced over a fixed 75 ms window; message/presence delivery does
not wait for that window and never performs an unread query. Unchanged read markers,
exact send retries and unchanged edits do not publish notifications. Presence writers
compare combined account state, suppressing unchanged heartbeats/typing refreshes;
snapshot readers never publish notifications. Subscriptions unregister on switch or
close. Subscribers recheck session/access under the access gate before reading and
sending authorized state. Event batches are capped at 100
and drained without waiting for a polling interval. Join/leave/typing transitions
also wake presence delivery; unchanged snapshots are not resent. Initial history
and its event cursor are read under a shared channel lock that excludes channel writes. Reconnect subscribes again
and reloads a fresh snapshot, recovering missed sends, edits and deletions.

Heartbeats use the socket every ten seconds; clients abandon silent connections
after 25 seconds and reconnect with exponential backoff plus jitter (1-30 seconds).
The server closes clients silent for 35 seconds. Ten-second maintenance checks
session/access expiry and refreshes/reaps presence leases, not message history or
unread counts. Maintenance also detects grants restored by timeout expiry and
reconnects that socket so the composer receives current permissions. Typing expires after four seconds. Disconnect removes presence and
notifies remaining clients; process-crash leftovers expire after 35 seconds.
Multiple devices are combined per user.

Compatibility: existing installed clients can still begin with `authenticate`
(token/guild/channel). That legacy path retains its per-channel socket and 750 ms
backend polling until those clients are upgraded. Deploy the updated backend before
the new frontend. This change does not alter the HTTP message contracts.

Successful permission/membership changes, channel deletion, session revocation,
account deletion and rotation invalidate sockets immediately after commit. An
access read/write gate prevents delivery racing these mutations. For this initial
**single-backend-process** deployment, invalidation conservatively closes all
sockets, including unaffected channels; eligible clients reconnect automatically.
Rejected requests do not trigger invalidation, except a real token replay revoking
an existing session. Multiple replicas need shared invalidation and are not supported.

## Bounds

- 4000 Unicode characters, excluding control codes other than newline/tab. Leptos
  inserts text nodes, never inner HTML.
- HTTP bodies: 32 KiB. Socket frames/messages: 16 KiB.
- Per-account chat HTTP budget: 180 requests/minute, at most 30 writes.
- 30 socket events per ten seconds; 128 live/pending sockets per process.
- Database checkout/queries and socket writes have deadlines. Blocking Diesel
  runs off async workers. Slow sockets close rather than buffer without a bound.
- Frontend HTTP requests time out after 15 seconds; failed sends retain retry IDs.
- Frontend history: 500 resident messages; pending sends: 20. Failed sends can be
  retried or discarded; hitting the pending limit leaves the draft intact.

This targets a personal deployment, not measured large-installation capacity.
Authorization loads only the actor, Everyone/assigned roles and applicable channel
overrides, reusing the authoritative evaluator. Unread counts load the actor's
channel grants across the guild; presence checks batch only members with active
leases in the requested channel. Mentions query matching guild usernames directly.
Each socket refresh shares one session/guild authorization transaction between
unread counts and channel events. Commit notifications are scoped to the subscribed
guild, channel and account.
Event-log compaction, cross-process fanout, full-guild presence and load tests are
future work. Attachments, search and desktop notifications remain pending.
Native voice uses a separate signaling socket and the same access gate; see [audio.md](audio.md).

## Checks

Backend tests use random schemas in TEST_DATABASE_URL and cover concurrent send
deduplication, revisions, pagination, mentions/read markers, isolation, content
transport, socket authentication, origins, rate limits, heartbeats, live updates,
reconnect snapshots, membership/channel revocation and logout. Regression checks
also cover scoped notification routing, unchanged read-marker rows, duplicate send/
edit suppression, multiple-device read delivery, and presence/typing start, stop,
expiry and disconnect without notification echoes. A cancellation regression pauses
a transaction at commit, cancels its HTTP handler and verifies durable message and
unread delivery after the detached worker commits. Run with
`--include-ignored`. Shared tests cover wire versions and bounds. Browser smoke
tests use isolated fixtures; native compilation does not establish cross-platform
WebView runtime acceptance.
`cargo test -p thiscord-frontend --lib --locked` also checks bounded history over
long sessions, revision/deduplication rules, eviction cleanup, old-history/live
isolation and variable-height viewport/anchor calculations without a browser.
