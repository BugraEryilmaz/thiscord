# Text chat and sockets

Select a text channel from a server's channel list. Messages render as plain text;
HTML, links and Markdown are not executed. Use `@username` to mention a guild
member. Mentioned messages are highlighted; channel badges show unread counts and
an `@` for unread mentions. Online/typing indicators describe people viewing the
same channel, not deployment-wide availability. Voice uses its separate media transport.

The composer shows sending/failed states. Retry reuses the original client UUID,
so a response lost after commit cannot create a duplicate. Drafts and pending sends
are held in memory for the open channel, not across channel changes/app restarts.
Edit/delete controls reflect current permissions; the backend checks them again.
Deletion requires confirmation and leaves a content-free tombstone. Loading older
history preserves scroll position; incoming messages follow only near the bottom.
Read acknowledgements require focus and the bottom of the view. Jump to latest
marks the channel read.

## Contracts and persistence

`POST /api/v1/chat` takes `ChatRequest` and returns `ChatResponse` from
`shared/src/chat.rs`. Commands cover send, edit, delete, history, read and unread.
They require bearer authentication and use the standard error envelope/request ID.
Message content, tokens and socket frames are never logged or derived as Debug.

The migration adds messages, a durable event log, read markers and short-lived
presence. Server UUIDs identify messages; server sequences order them. Channel
timestamps are monotonic under the guild lock, with UUID as a stable tie-breaker.
Client UUIDs are unique per author across channels. An original-content hash detects
reuse with a different payload; an exact retry returns the current message,
including edits/deletion. Edits/deletes require the current revision; stale writes
return 409. Authors need EditOwnMessages/DeleteOwnMessages; ManageMessages permits
deleting others' messages, not impersonating them through edits.

History returns the latest 50 messages by default (maximum 100), ascending within
each page. Pass the shared `older` PageCursor as `before` for the preceding page.
The query uses a strict `(created_at,id) < cursor` boundary, fetches one extra row,
and reverses the descending query result. It never uses offsets.

Every operation checks membership, text-channel type, ViewChannel and its action
permission in the same locked transaction. History, unread counts and subscriptions
also require ReadHistory. Cross-channel/guild IDs are rejected. Read positions are
monotonic and clamped to the channel's latest sequence. Mentions resolve only
against current guild usernames, at most 20 per message. Unread counts/mentions
are filtered by the requesting account's current channel permissions.

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

Committed chat mutations wake a bounded/coalesced in-process watch channel.
Subscribers recheck session/access under the access gate before reading and sending
current unread counts and durable message events. Event batches are capped at 100
and drained without waiting for a polling interval. Join/leave/typing transitions
also wake presence delivery; unchanged snapshots are not resent. Initial history
and its event cursor are read under the same guild lock. Reconnect subscribes again
and reloads a fresh snapshot, recovering missed sends, edits and deletions.

Heartbeats use the socket every ten seconds; clients abandon silent connections
after 25 seconds and reconnect with exponential backoff plus jitter (1-30 seconds).
The server closes clients silent for 35 seconds. Ten-second maintenance checks
session/access expiry and refreshes/reaps presence leases, not message history or
unread counts. Typing expires after four seconds. Disconnect removes presence and
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

This targets a personal deployment, not measured large-installation capacity.
Full guild evaluation reuses the current permission policy; commit notifications
currently wake all session sockets, which filter their subscribed state.
Event-log compaction, cross-process fanout, full-guild presence and load tests are
future work. Attachments, search and desktop notifications remain pending.
Native voice uses a separate signaling socket and the same access gate; see [audio.md](audio.md).

## Checks

Backend tests use random schemas in TEST_DATABASE_URL and cover concurrent send
deduplication, revisions, pagination, mentions/read markers, isolation, content
transport, socket authentication, origins, rate limits, heartbeats, live updates,
reconnect snapshots, membership/channel revocation and logout. Run with
`--include-ignored`. Shared tests cover wire versions and bounds. Browser smoke
tests use isolated fixtures; native compilation does not establish cross-platform
WebView runtime acceptance.
