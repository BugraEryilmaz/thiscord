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

Friend rows and the direct-message header show an accepted friend's custom status
(`Person.custom_status`); search results and pending requests never include it.
Each snapshot also carries the connected account's own status as `own_status`,
which keeps the account ribbon synchronized live across that account's devices:
setting or clearing a status wakes the account's own friends sockets as well as
everyone listing it. Snapshots are rebuilt at the earliest listed status expiry,
like call expiry. Both fields are optional and omitted when unset. See
[accounts.md](accounts.md#custom-status).

Search and friend requests skip pending accounts (unverified email); they appear
once verified ([accounts.md](accounts.md)).

Accepted friends can open a private conversation and start a voice call without
sharing a guild. Online means at least one authenticated friends socket is alive,
regardless of the selected guild/channel. Multiple devices count as one person;
pending requests do not disclose presence. Older clients without the friends socket
do not advertise this presence. Disconnect removes its in-memory lease; silent
connections are closed after 35 seconds. This remains a single-backend-process design.
Online friends also carry the live game activity reported by one of their desktop
sockets (`Friend.activity`); see [activity.md](activity.md).

## Contracts and storage

`shared/src/friends.rs` owns the request/response and version-1 socket contracts.
`POST /api/v1/friends` uses bearer authentication and the usual request-correlated
error envelope. Commands cover search, add, accept, remove, history, send, start
call, accept call, end call, edit, delete and read acknowledgements. Input bodies are limited to 32 KiB. Blocking workers
are bounded at eight; per-account HTTP budgets are 180 requests and 30 writes per
minute. Every action revalidates the session inside its transaction.

`friendships` has one canonical unordered pair and a persistent conversation UUID.
Account locks serialize request limits and call busy checks; relationship locks
serialize message ordering and authoritative membership checks. A pending or removed
relationship cannot read/send messages, retry a previously committed send, or join
a call. Instance/guild administrators have no implicit access to these operations.

`direct_messages` persists text (up to 4000 Unicode characters; no controls except
newline/tab; nonblank unless attachments are present), server IDs and UTC timestamps. Client message UUIDs are unique per
author: an exact retry returns the current revision (including a tombstone), while reuse for different text
or a different conversation conflicts. Timestamps increase under the relationship
lock. History uses the shared reverse `(created_at,id)` keyset cursor, defaults to
50 messages and caps pages at 100. The frontend uses the shared safe message renderer, keeps a live
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
contribute. Read state persists across devices/reconnects; acknowledgements wake
the reader's other sockets immediately. Older servers default to zero counts on new clients.
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

`/api/v1/friends/socket` accepts only configured Origins, at most `THISCORD_MAX_SOCKETS` (default 512) sockets, and
16 KiB client frames. The first frame is a versioned `connect` carrying the bearer
token; credentials never appear in the URL. `select` chooses an optional conversation,
and `ping` keeps the socket alive. This separate account-wide socket leaves the
existing guild chat subscription intact. The client connects while logged in, uses
ten-second heartbeats, and reconnects with exponential backoff and jitter.

The server reads an authorized bounded friends/calls snapshot on connect and
`select`, after a relevant commit, when the earliest listed call expires, and at
least every ten seconds. Committed friend
commands wake both people's sockets (reads wake only the reader); display-name,
avatar and account-deletion commits wake everyone listing that account. The wake-up
is an in-process version bump without data or grants: sockets always re-read and
re-authorize from the database. The ten-second read also covers time-only changes
(session expiry) and writes outside these paths, such as direct SQL maintenance.
Presence and typing are in memory and refresh every second on the last database
snapshot, delivered under the account/session epochs captured before that read.
Profile lookup is batched. At most eight snapshot workers run simultaneously;
saturated ticks skip work and retry. Unchanged snapshots are suppressed except for
a ten-second refresh. This intentionally favors a small personal installation over
event-log/fanout complexity; wake-ups, like presence, reach only this process. The frontend rejects history
for a superseded selection and clears account state on logout/token replacement.

Each socket keeps the selected conversation's latest (or `before`) page between
reads. Every read still reauthenticates, locks relationships and re-checks that the
selection is an accessible friendship; only the history query is skipped unless the
selection or page changed, a send/edit/delete committed in that conversation since
the page was loaded, or the copy is 60 seconds old. Committed message writes bump a
fixed, lock-free per-conversation epoch (1,024 shared slots; a shared slot only
causes an extra reload) before waking sockets, and the epoch is read before the
snapshot transaction, so a concurrent write always forces a later reload. Lost
access drops the copy. The 60-second reload bounds staleness after writes outside
these paths, like the other ten-second checks. Epochs and copies are per process.

Clients opening the socket with `?history=retained` (`RETAINED_HISTORY_QUERY`)
keep the history last received on that socket for the same conversation and page.
The server then sends history only when it changed and on the first snapshot after
connect or `select`; other snapshots carry `conversation_id` with `history: null`.
Older clients do not send the query and still receive history in every snapshot;
older servers ignore it and always send history, which the new client accepts.

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
The `/app` web client supports friends, messaging and call audio through the browser's
WebRTC (see [browser voice](audio.md#browser-voice)), including cameras and, in desktop
browsers, screen sharing (see [browser screen sharing](screen-sharing.md#browser-screen-sharing)).
Calls use the existing server-terminated WebRTC encryption and STUN/TURN requirements
from [audio.md](audio.md), and are not end-to-end encrypted between accounts.

## Validation and remaining work

Shared contracts test bounds and unknown-field rejection. PostgreSQL/socket tests
cover consent, presence privacy and multiple devices, private history isolation,
concurrent deduplication, conflicting retries, pagination, call acceptance/end and
removal, and that commits and call expiry are pushed before the ten-second refresh. Loopback WebRTC tests exchange packets in both directions, reject premature
and outsider joins, and revoke both peers on hang-up or unfriend. Existing CI runs
these ignored database tests with `--include-ignored`, plus WASM and native builds.

Physical microphone/speaker acceptance on Windows/macOS/Linux remains necessary.
Direct-message and incoming-call alerts use the separate server subscription
stream; see [notifications.md](notifications.md). Typing
expiry, revision conflicts and retry-after-edit/delete are covered by private socket/database tests.
Cross-process presence/fanout remains unimplemented.


## Editing, deletion and typing

Only a message's author can edit/delete it, while both parties still consent to
private access. Commands submit the current `revision` (initially 1) and run under
the relationship write lock. Changed content increments revision and `edited_at`;
a stale different edit conflicts. An exact repeat of the immediately preceding
edit/delete is idempotent. The immutable internal `request_hash` fingerprints the
original text and attachment IDs, so a lost send response can be retried after
later edits/deletion without recreating content. Legacy messages acquire their
fingerprint before their first edit. Reusing the UUID for another payload conflicts.

Deletion persists an empty tombstone at the original history position and removes
private attachment bytes. Tombstones do not count as unread. The frontend refreshes
both latest and selected older pages over the existing bounded snapshot socket,
keys rows by message ID, applies revisions reactively without dropping edit drafts,
and ignores older revisions. Edits keep the last 20 replaced versions, readable
with `edit_history` under the same pair access as history and deleted with the
message (see [chat.md](chat.md#edit-history)). Reconnect reloads the selected page,
including missed deletions.

`select` optionally includes a `before` history cursor; older clients can omit it.
`typing { active }` is scoped to the socket's authorized selection. Each socket
owns a four-second in-memory lease; devices are combined per account. Changing
selection, disconnecting or stopping clears its own lease. The frontend refreshes
at most every two seconds while input is active, stops on send/blur/inactivity,
and clears typing on reconnect. Typing uses the same eight-worker socket DB budget.
Backend-first deployment is required for the new commands and select fields.

## Private attachments

`POST /api/v1/direct-attachments?conversation_id=…&filename=…&content_type=…`
accepts a binary body after accepted-friend/session authorization, then rechecks
those permissions before storing bytes. Stages belong to one account and one
conversation. Sending claims up to four distinct staged IDs atomically with the
message. Only the uploader can claim them; staged files cannot be downloaded.
The ordinary guild attachment API is unchanged.

`GET /api/v1/direct-attachments/{id}` requires a current accepted friendship, no
block in either direction, and an undeleted message. Knowing an ID, being an
instance admin, or having shared guild permissions does not grant private access.
Downloads capture account/session and message-deletion epochs and check every
64 KiB chunk. Block, unfriend, session revocation and deletion reject queued stale
chunks. Bytes already received or saved to disk cannot be recalled.

Each file is 1 byte–25 MiB, with at most 16 unclaimed files / 100 MiB per uploader
across conversations. Four shared upload/download slots bound buffers; uploads
expire after 30 seconds and download buffers after 60 seconds. Staged files expire
after one hour and cleanup deletes up to 64 rows per minute and per upload. Cleanup
rechecks that each row is still unclaimed and expired after waiting for concurrent
sends, preserving files claimed before expiry even if the send commits afterward.
Block also deletes that pair's unclaimed files. Published files remain until message or
account deletion; removing a friendship hides them while retaining the history.
Bytes are PostgreSQL `bytea`, included in DB backups. Download MIME is classified
from bytes: PNG/JPEG/GIF/WebP and MP4/WebM can be previewed, everything else is a
file download. Responses are `no-store`, `nosniff` and attachment disposition.

The private composer supports file selection, removing staged attachments,
spoiler marks, voice messages ([chat.md](chat.md#voice-messages); uploads with
`voice=true` follow the same consent and quota rules, at most 5 MiB),
attachment-only sends and retrying frozen text/file IDs and spoiler marks. View/Download fetch through
authenticated bounded browser transport (no token-bearing URLs). Images/video use
Blob URLs; each explicit View/Download reauthorizes. Closing, switching conversations,
deletion, logout or loss of the friends socket clears previews and aborts transfers.
Download Blob retention is capped at 60 seconds; saved copies are outside app control.
Physical codec playback/download acceptance on all three platforms remains pending.

## Blocking and reports

See [Safety and reports](safety.md) for persistent block semantics, report consent,
review authority and retention. Shared guild membership/permissions do not change.

## Group conversations

Group conversations extend private messages beyond a pair. They share the
direct-message storage, history, attachment, search, report and call machinery,
but authorize through **current group membership** instead of a friendship.
`shared/src/friends.rs` owns the additive contracts (`Group`, `GroupCall`,
`SystemEvent` and the group commands on `POST /api/v1/friends`); the friends
socket lists groups and their live calls in `Snapshot::groups`/`group_calls`.
Released clients ignore these fields and never see groups (see
[compatibility](#group-compatibility)).

### Membership rules

- A group holds **2–8 members including its creator** (`MAX_GROUP_MEMBERS`), the
  SFU's eight-person room capacity, so every member can always join its call.
  Each account belongs to at most 100 groups (`MAX_GROUPS`).
- Only **accepted friends of the adder** can be added, at creation or later; each
  addition rechecks that friendship under its row lock. Pending requests, removed
  friendships, bots and pending (unverified) accounts cannot be added. Once
  inside, membership no longer depends on friendship: unfriending the person who
  added you does not remove you.
- **Blocks between prospective members refuse the addition.** Nobody can add an
  account that they blocked or that blocked them (the friendship is gone), and an
  account cannot be added while a block exists in either direction between it and
  any current or co-added member. The refusal is generic ("can't be added") and
  never names the member or direction, so a blocker is never pulled into a room
  with the account they blocked.
- "Add friends" from a 1:1 conversation creates a **new** group with that friend
  preselected. A 1:1 conversation never turns into a group, and its history stays
  private to the pair.
- Any member can rename the group (up to 100 characters, no controls; an empty
  name falls back to the members' display names) and add friends while there is
  room. The **owner** (initially the creator) can remove members and transfer
  ownership to another member. Every member can leave. When the owner leaves or
  deletes their account, ownership passes to the longest-standing remaining
  member. A group whose last member leaves is deleted with its history and files.
  Group icons are not implemented; lists show the members' stacked avatars.
- Creation, renames, additions, removals, ownership transfers and departures post
  **system messages** into the history (content-free rows with a typed `system`
  event and the affected display names at that time). They cannot be edited,
  deleted, reported or searched and never count as unread.
- **New members see the full retained history**, as in other group chat apps; the
  add dialog says so. Members who leave or are removed lose access to all of it;
  being added again restores it.

### Locking, revisions and revocation

Group commands authenticate the session, then lock the group row (`FOR UPDATE` for
any write, `FOR SHARE` for reads), check current membership/ownership and run the
write in the same transaction. Metadata commands (rename, add, remove, transfer)
submit the group `revision` they viewed; every metadata change increments it and a
stale submission returns 409 without automatic retry. Creation and additions also
lock every involved account in UUID order (the order used by blocks and friend
requests), so a concurrent block or unfriend cannot interleave with the checks.
Creation carries a client-generated UUID: an exact retry returns the same group,
a different payload under the same UUID conflicts.

Leaving and removal run under the same access gate as friend removal: the leaving
account's socket deliveries, private downloads and media drain **before commit**
and are invalidated after it, so its friends socket reconnects without the group,
queued attachment chunks fail, and an ongoing group call drops that member. The
departed member's ringing and call lease rows and unclaimed uploads for that group
are deleted in the transaction. Remaining members are woken to rebuild their
snapshots. Instance administrators and guild roles grant no access.

Message writes in a group (send/edit/delete/read) hold the group row lock exactly
like a friendship's: per-author client UUID deduplication, revisioned edits,
tombstones, attachment claims, read positions (one per member) and keyset history
behave identically. Typing leases are scoped to the authorized selection.
`@username` mentions of members are passed to notifications, so a group set to
"Mentions only" still alerts when you are named.

### Blocking inside a group

A block never removes anyone from a group or lets one member evict another; groups
are shared spaces like guilds. Across a block in either direction, the two members
do not see each other's presence or typing, the other party's messages are
excluded from unread badges and notifications, and starting a call does not ring
the other party (they still see "call in progress" and may join). Messages stay
visible in the shared history so conversations remain coherent; leave the group to
stop seeing them. Reports work as for 1:1 messages.

### Group calls

Starting a call in a group creates one live call per group and rings every other
member for 45 seconds (except across a block). Members join or leave freely while
it runs; the conversation header, the conversation list and a "call in progress"
bar show who is in it and offer Join. Joining holds a 60-second lease that the
SFU's authoritative checks renew; the call ends when no joined lease remains, and
leaving or declining never ends it for others. Group calls reuse the direct-call
namespace (equal scope/room UUIDs from the call ID), the native CPAL/Opus path,
the browser WebRTC client, cameras and screen sharing. Every join, resume and
refresh rechecks current membership and a joined lease; leaving or removal drains
that member's media before commit. Group calls have no soundboard, like 1:1 calls.
Being in a group call does not make you busy for 1:1 calls; the single-connection
takeover moves you between rooms.

### Account deletion

Deleting an account removes its memberships, ringing/lease rows, and the messages
and files it authored in groups, and transfers or deletes groups as above.
Remaining members keep their own messages; system messages mentioning the deleted
account keep the display name recorded when they were posted.

### Group compatibility

Version 0.1.61 clients ignore `groups`/`group_calls` and are not offered group
notifications: the notification socket only routes group alerts to clients that
open it with `?groups=1` (`notifications::GROUPS_QUERY`), which older servers
ignore. New clients connected to older servers simply see no groups; the group
commands are backend-first.

Group storage is in migration `20261108300000_group_conversations`. Messages and
private attachments keep their tables; their conversation foreign key becomes a
trigger-maintained reference to either a friendship or a group (deleting either
cascades its messages and files). Rollback permanently deletes groups, their
messages, files and calls, then restores the friendship-only foreign keys.
