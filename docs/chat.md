# Text chat and sockets

Select a text channel from a server's channel list. Guild and private messages
automatically turn HTTP/HTTPS URLs and bare domains (using HTTPS) into clickable
links. Desktop links open in the default system browser; browser-preview links
open in a new tab. Surrounding punctuation stays outside the link. Markdown supports emphasis, lists, quotes, inline code and fenced code blocks.
Raw HTML is displayed as text; image Markdown displays its alt text without remote
image requests. Parsing is limited to 4000 characters, 16 levels and 16000 events. Non-web schemes and
URLs containing credentials are not linked or accepted by the desktop opener.
Type `@` in the guild chat composer to search members by public profile name or
username, including offline members. Suggestions show both the profile name and
unique `@username`; selecting one inserts the username used by existing mentions.
Use Up/Down and Enter/Tab, or click a suggestion; Escape dismisses the picker.
Shift+Enter still inserts a newline. Completion also works when editing messages
and at the caret in the middle of a draft. Server views show profile display names;
older server responses fall back to usernames.

The `mention_suggestions` chat action takes `guild_id`, `channel_id` and `query`
(at most 64 characters). It requires current guild membership and channel view/send
permissions. It returns at most eight public identities, matching literal,
case-insensitive substrings of profile names or usernames, with prefix matches
first. Queries are debounced and stale responses are discarded on draft, channel,
session or access changes. Deploy the backend before the new client.
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

## Attachments

Drop images, videos or other files onto an open text channel, or choose them with
the composer's attachment button. Review/remove queued files before sending;
message text is optional when files are attached. A message accepts up to four
files, each at most 25 MiB. Sending still requires the channel's SendMessages
permission. Attachments are currently for guild text channels, not private messages.

Supported pictures render as previews. Videos have a thumbnail with a play button;
opening one starts playback in a full-window viewer with native playback controls.
Pictures use the same viewer. Close it with Escape or its close button. Generic
files download when clicked, and media has a separate download control. Playback
depends on the browser/native WebView's installed codecs; unsupported media can
still be downloaded.

Desktop downloads go to the OS Downloads folder with a unique `thiscord-` prefix
to avoid overwriting existing files; the UI reports the native completion result.
Browser downloads use the browser's configured destination. Native download support
requires macOS 11.3 or newer. Desktop WebView drag interception is disabled for the
main window so external file drops reach the Rust/WASM composer.

`POST /api/v1/attachments` accepts a raw binary body and an `AttachmentUpload`
query containing `guild_id`, `channel_id`, `filename` and `content_type`. It returns
`ChatAttachment` metadata. The backend determines the stored media type; the client
hint does not permit HTML or SVG rendering. Uploads require bearer authentication
and bind the returned attachment ID to its uploader and channel. Send commands
carry `attachment_ids`; message/history/socket payloads carry `attachments`
metadata. Existing text-only sends omit the empty ID list. Deploy the backend
before the new client.

`GET /api/v1/attachments/{id}` requires bearer authentication and current channel
access to an undeleted message. There are no public attachment URLs or tokens in
URLs. The frontend fetches authorized bytes into temporary Blob URLs for viewing
and downloads; these are released when their UI owner closes or loses access.
Responses disable caching and content sniffing and use an attachment disposition.
Revocation prevents future downloads; bytes a recipient already downloaded cannot
be recalled.

Files and metadata live in PostgreSQL so ordinary database backups include them.
Removing a message removes its attachment data; channel/guild deletion also
cascades. Editing text preserves the attached files. Send deduplication includes
attachment IDs, so retrying a message does not attach the same files to a second
message. Abandoned staged uploads expire after one hour, with at most 16 staged
files / 100 MiB per account. This bounded, buffered implementation is intended for
the personal deployment; it does not transcode videos or provide resumable uploads.

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

Clients may opt into sidebar voice rosters with `voice_roster: true` on `subscribe`.
Omitting it defaults to false, so existing clients receive no new event variants.
Opted-in guild subscriptions receive `update` / `voice_roster` with visible channel
IDs and live participant metadata, including empty visible rooms. Membership and
ViewChannel are checked in a locked transaction off the async worker on each
one-second refresh; JoinVoice is not required to view a roster. The SFU registry
is read only for authorized channels, excluding inactive or revoked peers. No media
connection is created for observers. Delivery uses the existing account/session/
guild/member access epochs and barrier, including the async registry read, so a
permission change cannot admit a stale snapshot. Unchanged snapshots are suppressed.
Steady state adds one authorization transaction per second per opted-in guild socket;
refreshes skip missed ticks. It remains a single-process, personal-deployment path.
The UI clears rosters on subscription changes, disconnects, revocation and logout.
Deploy the backend before this frontend: older backends reject the new subscribe
field. The account ribbon reports connecting, authenticated, reconnecting and
browser-reported offline states independently of the native voice connection.

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
close. Subscribers capture account/guild access epochs before querying authorized
state and validate those epochs when admitting delivery. No delivery permit is held
during database work. Event batches are capped at 100 and drained without waiting
for a polling interval. Join/leave/typing transitions
also wake presence delivery; unchanged snapshots are not resent. Initial history
and its event cursor are read under a shared channel lock that excludes channel writes. Reconnect subscribes again
and reloads a fresh snapshot, recovering missed sends, edits and deletions.

Heartbeats use the socket every ten seconds; clients abandon silent connections
after 25 seconds and reconnect with exponential backoff plus jitter (1-30 seconds).
An Unauthorized socket error triggers a bounded HTTP account check. Only a
confirmed HTTP 401 clears that session and returns to sign-in; transient failures
keep retrying. The check is tied to the socket's token so an old response cannot
clear a replacement login. Native credential/voice cleanup runs independently of
the socket task, which is cancelled when the token is cleared.
The server closes clients silent for 35 seconds. Ten-second maintenance checks
session/access expiry and refreshes/reaps presence leases, not message history or
unread counts. Maintenance also detects grants restored by timeout expiry and
reconnects that socket so the composer receives current permissions. Typing expires after four seconds. Disconnect removes presence and
notifies remaining clients; process-crash leftovers expire after 35 seconds.
Multiple devices are combined per user.

Compatibility: existing installed clients can still begin with `authenticate`
(token/guild/channel). That legacy path retains its per-channel socket and 750 ms
backend polling until those clients are upgraded. Deploy the updated backend before
the new frontend. Legacy text-only HTTP commands remain accepted; attachment
fields and upload/download routes are described above.

Guild-wide permission changes and channel deletion invalidate sockets subscribed to
that guild. Member changes invalidate only that account's subscriptions in that
guild. Rotation, replay, logout and session eviction invalidate only the affected
session; account-wide revocation and deletion invalidate all of its sessions,
including connections without a guild subscription. Instance roles grant no guild
access and invalidate only that account's diagnostics authorization.
Account/session/guild/member barriers drain already admitted socket sends before
an access mutation can commit, and reject snapshots read across that mutation.
Unrelated scopes keep serving HTTP and delivering chat. Socket sends remain bounded
by deadlines. Diagnostics captures account, session and instance-role epochs before
its authoritative database check and validates them again after async collection.

HTTP reads and chat writes do not acquire a process-wide access lock. Database
transactions retain their account/guild locks and authoritative permission/revision
checks. Access mutation guards and commit notifications live in blocking workers,
so cancelling an HTTP request cannot release protection or skip invalidation.
Rejected requests do not publish revocations, except real token replay revoking
an existing session. Barriers pause only after successful authorization/writes,
immediately before commit. A socket overlapping that commit may reconnect
conservatively even if the database ultimately fails to commit. Eligible clients
reconnect automatically. This remains a **single-backend-process** design; multiple replicas
need shared invalidation and are not supported.

## Bounds

- 4000 Unicode characters, excluding control codes other than newline/tab. Leptos
  inserts text nodes, never inner HTML.
- Chat JSON bodies: 32 KiB; binary attachment uploads: 25 MiB each. Socket
  input frames/messages: 16 KiB. Attachment bytes never pass through the socket.
- Admission precedes database work and lock waits: 8 chat, 8 permission and 4
  local account operations per process. Google discovery has a separate 4-request
  budget and holds no access barrier during provider I/O. Excess requests return 429.
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
future work. Message search is described in [search.md](search.md). Customizable alert subscriptions
and desktop banners are described in [notifications.md](notifications.md).
Native voice uses a separate signaling socket and media admission/drain mechanism;
see [audio.md](audio.md).

## Checks

Backend tests use random schemas in TEST_DATABASE_URL and cover concurrent send
deduplication, revisions, pagination, mentions/read markers, isolation, content
transport, socket authentication, origins, rate limits, heartbeats, live updates,
reconnect snapshots, membership/channel revocation and logout. Regression checks
also cover scoped notification routing, unchanged read-marker rows, duplicate send/
edit suppression, multiple-device read delivery, and presence/typing start, stop,
expiry and disconnect without notification echoes. A cancellation regression pauses
a transaction at commit, cancels its HTTP handler and verifies durable message and
unread delivery after the detached worker commits. Concurrency tests hold account/
guild rows to verify unrelated HTTP/socket progress, early overload rejection and
revocation after HTTP cancellation, same-account session isolation, member isolation
and subscription scope changes. Run with
`--include-ignored`. Shared tests cover wire versions and bounds. Browser smoke
tests use isolated fixtures; native compilation does not establish cross-platform
WebView runtime acceptance.
`cargo test -p thiscord-frontend --lib --locked` also checks bounded history over
long sessions, revision/deduplication rules, eviction cleanup, old-history/live
isolation and variable-height viewport/anchor calculations without a browser.


## Replies, reactions and pinned messages

Guild message rows provide Reply, ten emoji reactions, and Pin/Unpin for moderators.
Replies reference an existing undeleted message in the same channel. They require
ReadHistory as well as SendMessages. The 160-character preview is read from the
current target; deleted targets expose no content. Clicking a preview loads a
bounded context window and highlights the original within the virtual list.
Each preview carries the target's `target_revision`, independently of the reply's
own revision. Clients compare these revisions across HTTP and socket snapshots,
including when the target is outside retained history. Missing legacy revisions
deserialize as unknown and cannot replace a known revision; two unknown revisions
retain the existing preview because their freshness cannot be established.
Unavailable previews remain hidden until the chat state resets. Responses to
recipients without ReadHistory omit the target revision as well as its content.
Reply IDs are frozen into pending sends and included in deduplication hashes.
Existing sends without replies retain their original hash scheme.

Reactions use `react { message_id, emoji, present }`, an explicit desired state.
The unique message/account/emoji key and channel lock make concurrent retries
idempotent. ReadHistory and SendMessages are required. Counts and the requesting
account's `mine` flag are hydrated for history, replies and each socket recipient.
The fixed shared palette bounds reaction groups to ten per message.

Message pins are shared channel data, distinct from personal sidebar channel pins.
Pin changes require ReadHistory, ManageMessages and the current message revision.
A channel holds at most 50 pins. Pins load automatically with the channel and stay
fixed above the scrolling history, showing authors, timestamps, Markdown bodies
and attachment filenames. Multiple pins share a height-limited area so history
remains usable. Jump controls navigate to the original without dismissing pins;
moderators can unpin directly there. Empty channels show no pinned area. Search
temporarily hides the area so its history space contains only matching messages.
Edits, unpins and deletions update the fixed area through normal socket events;
reconnects reload it and access resets discard it. Pin/reaction changes increment
message revision and publish existing message events after commit; they do not
change message timestamps or unread positions.
Deleting a message clears its pin, reply and visible reactions. Migration rollback
removes all reply references, reactions and message pins, retaining message text.

`ChatMessage.reply`, `.reactions` and `.pinned` are additive fields with defaults.
Old text-only sends still work. Deploy the backend before clients using new commands.
The new context command returns up to 51 messages; its event cursor is not a socket
subscription checkpoint. Jumping back keeps the current socket and marks the cache
as having newer history until Jump to latest refreshes its snapshot.
