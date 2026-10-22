# Friends, private messages and calls

The home button opens Friends, with Online, All, Requests and Messages tabs. Add friend in
the top right searches anywhere within display names and usernames, ignoring case
(2–64 Unicode characters after trimming, no control characters, up to 20 results).
Spaces, accents and punctuation are accepted; `%` and `_` match literally.
Results show usernames to distinguish people with identical display names, and
friend requests target the selected account ID. Search returns only account ID,
username, display name and avatar;
it never returns email, identities, sessions or presence. Sending requests requires
a verified email. The recipient explicitly accepts; crossing requests do not
silently establish a friendship. Requests can be declined or cancelled, and either
friend can remove the relationship. Each account can have at most 200 accepted or
pending relationships, enforced while locking both accounts in UUID order.
The Home bubble totals incoming requests and unread direct messages. Requests
shows incoming requests only (outgoing requests still appear in the list).
Messages, the direct-message sidebar and individual conversations show unread
message bubbles. Zero counts are hidden; counts above 99 display `99+`.
Muting sounds/banners does not hide unread counts.
Right-click a voice participant, a message author's name, or a person in the home
friend list to add or unfriend them. Pending requests offer Accept/Decline or
Cancel instead; removing a friend requires confirmation. These actions follow
the live relationship state and are disabled while the friends socket reconnects.

Accepted friends can open a private conversation and start a voice call without
sharing a guild. Online means at least one authenticated friends socket is alive,
regardless of the selected guild/channel. Multiple devices count as one person;
pending requests do not disclose presence. Older clients without the friends socket
do not advertise this presence. Disconnect removes its in-memory lease; silent
connections are closed after 35 seconds. This remains a single-backend-process design.

## Contracts and storage

`shared/src/friends.rs` owns the request/response and version-1 socket contracts.
`POST /api/v1/friends` uses bearer authentication and the usual request-correlated
error envelope. Commands cover search, add, accept, remove, history, send, start
call, accept call, end call and read acknowledgements. Input bodies are limited to 32 KiB. Blocking workers
are bounded at eight; per-account HTTP budgets are 180 requests and 30 writes per
minute. Every action revalidates the session inside its transaction.

`friendships` has one canonical unordered pair and a persistent conversation UUID.
Account locks serialize request limits and call busy checks; relationship locks
serialize message ordering and authoritative membership checks. A pending or removed
relationship cannot read/send messages, retry a previously committed send, or join
a call. Instance/guild administrators have no implicit access to these operations.

`direct_messages` persists plain text (1–4000 Unicode characters; no controls except
newline/tab), server IDs and UTC timestamps. Client message UUIDs are unique per
author: an exact retry returns the original message, while reuse for different text
or a different conversation conflicts. Timestamps increase under the relationship
lock. History uses the shared reverse `(created_at,id)` keyset cursor, defaults to
50 messages and caps pages at 100. The frontend renders text nodes, keeps a live
50-message snapshot, and pages older history separately. Jump to latest restores
the current snapshot. Failed sends retain their UUID and content for explicit retry;
drafts and retries are in memory for the open conversation.
Each friendship stores separate read timestamps for its two participants. The
client acknowledges a displayed message ID only while the conversation is focused,
at the bottom of its latest history, and settings are closed. Merely selecting a
conversation on the socket or fetching history does not mark it read. The server
checks accepted membership, resolves that ID within the conversation and advances
only the caller's position under the relationship lock. Delayed/repeated reads
cannot move the position backward or consume newer messages. Read acknowledgements
use the ordinary HTTP request budget, not the message-send write budget.
Snapshots count incoming messages after that position using an indexed range
limited to 100 rows per conversation. Own messages and pending relationships never
contribute. Read state persists across devices/reconnects and follows the existing
one-second snapshot refresh. Older servers default to zero counts on new clients.
The read-state migration treats retained messages as unread until viewed; rollback
discards read positions and the new index while retaining messages/friendships.
Enter sends the draft; Shift+Enter inserts a newline. IME confirmation and held-key
repeats do not send messages, and sending waits until the friends socket is connected.

Removal retains private history but denies access to both parties. Accepting a new
request between the same accounts restores that history. Account deletion cascades
its friendships, private conversations and calls, deleting the shared DM history
for both parties. Reverting the friends migration permanently deletes all social
data. Startup applies the migration; `schema.rs` is generated with Diesel.

## Sockets and authorization

`/api/v1/friends/socket` accepts only configured Origins, at most 128 sockets, and
16 KiB client frames. The first frame is a versioned `connect` carrying the bearer
token; credentials never appear in the URL. `select` chooses an optional conversation,
and `ping` keeps the socket alive. This separate account-wide socket leaves the
existing guild chat subscription intact. The client connects while logged in, uses
ten-second heartbeats, and reconnects with exponential backoff and jitter.

Once per second the server reads an authorized bounded friends/calls snapshot and
the selected conversation's latest page. Profile lookup is batched. At most eight
snapshot workers run simultaneously; saturated ticks skip work. Unchanged snapshots
are suppressed except for a ten-second refresh. This intentionally favors a small
personal installation over event-log/fanout complexity. The frontend rejects history
for a superseded selection and clears account state on logout/token replacement.

Socket delivery captures account/session authorization epochs before database work
and validates them when admitting bounded sends. Removal drains both accounts'
socket deliveries and media before commit, and invalidates them after successful
commit. These guards live in the blocking worker and survive HTTP cancellation.
Guild chat sockets may conservatively reconnect following friend removal; still
authorized guild media revalidates. Session revocation and account deletion also use
the existing barriers. No frame bodies, private messages or tokens are logged.

## Calls

Calls ring for 45 seconds. Only the callee can accept, and only the two participants
can end or join. Both accounts are locked for call creation/acceptance, preventing
overlapping direct calls. Active calls lease each participant for 60 seconds; the
SFU's authoritative checks renew only that participant's lease. Abandoned peers or
a backend restart cannot leave either account permanently busy.

The existing native CPAL/Opus/WebRTC transport carries audio. No PCM traverses Tauri
IPC and no new media pipeline is introduced. An equal scope/room UUID pair reserves
a direct-call namespace in the existing signaling transport: both UUIDs come from
the call ID, and they are never actual guild/channel rows or membership grants.
Every join/resume/refresh checks the active call, its two identities and the accepted
friendship. Ending a call drains that scope before commit; removing a friendship
drains both accounts. Existing media ingress/egress epoch checks reject queued data
after revocation. The SFU admits only the two account identities, with the existing
single-connection-per-account takeover behavior.

Incoming/outgoing call controls appear throughout the app. Once joined, call status
and hang-up live in the bottom-left voice dock; the duplicate floating card is hidden.
Outgoing ringing and unjoined calls retain their cancel/join controls. Acceptance
and automatic joining apply to the device that explicitly started/accepted the
call; other devices may explicitly join. The existing mute, deafen, devices and
hang-up controls apply.
Voice requires the desktop app; browser preview supports friends and messaging.
Calls use the existing server-terminated WebRTC encryption and STUN/TURN requirements
from [audio.md](audio.md), and are not end-to-end encrypted between accounts.

## Validation and remaining work

Shared contracts test bounds and unknown-field rejection. PostgreSQL/socket tests
cover consent, presence privacy and multiple devices, private history isolation,
concurrent deduplication, conflicting retries, pagination, call acceptance/end and
removal. Loopback WebRTC tests exchange packets in both directions, reject premature
and outsider joins, and revoke both peers on hang-up or unfriend. Existing CI runs
these ignored database tests with `--include-ignored`, plus WASM and native builds.

Physical microphone/speaker acceptance on Windows/macOS/Linux remains necessary.
Direct-message and incoming-call alerts use the separate server subscription
stream; see [notifications.md](notifications.md). Typing
indicators, message editing/deletion, blocking/reporting and cross-process
presence/fanout are not implemented.
