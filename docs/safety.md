# Blocking and reports

`shared/src/safety.rs` owns the commands for `POST /api/v1/safety`.
Bodies are capped at 32 KiB; authenticated blocking workers are capped at four,
with the usual 180 requests / 30 writes per account per minute budget. No report
reason, message body, attachment content or token is logged.

## Persistent blocks

Block is directional and private to the blocker, with at most 1000 blocked accounts.
Either direction denies friendship requests, acceptance, private history, send/edit/
delete/retry/read, typing, private attachments and private voice admission. The block
transaction locks both accounts in UUID order, removes accepted/pending friendship
consent, ends active/ringing calls and deletes unclaimed uploads for that pair.
Both accounts' delivery/media gates drain before commit; the worker retains them
through commit even if its HTTP caller disconnects. Queued notifications and SFU
ingress/egress must reauthorize. Requests and calls cannot bypass a block via unfriend.

Unblock removes only the caller's directional block. It never restores a friendship,
request or call. A new request must be explicitly accepted after **both** blocks are
absent before retained private history/files become accessible. Removing a friendship
does not remove either block. Neither party receives presence, typing, private unread
badges, private notifications or call invitations from that pair while blocked.
The other party sees the removed relationship, not the block list or its reason.
Public account search still returns names/IDs, allowing reports and identification.

Shared guilds deliberately retain their existing behavior: messages, mentions,
guild notifications, voice rooms/rosters and public profiles remain visible under
normal guild permissions. Blocking does not ban, mute, hide a moderator, change a
channel override or remove anyone from a shared room. Existing guild sockets may
conservatively reconnect and guild media reauthorizes after account gate invalidation.
Use guild moderation and personal voice controls for shared-room behavior. The UI
explains this before blocking and in Settings → Safety & reports.

## Explicit reporting and evidence

Report account is available in user context menus and Friends. Report appears on
received private/guild messages. The confirmation dialog explains who will receive
the evidence; submitting is explicit consent. An account report contains the target
account ID and the reporter's 1–2000 character explanation. A message report copies
only the selected message's author ID, content, message ID, revision, UTC timestamp
and attachment metadata. Evidence is read from storage after current authorization;
clients cannot submit fabricated evidence fields or choose a different author.
Deleted and self-authored messages cannot be reported. A private-message report
requires current private access, so report the message before blocking. A subsequent
account report can still describe concerns in the reporter's own words.

Report creation has an immutable client-generated report UUID for lost-response
retries. Exact retries return the same report; changed target/reason conflicts.
Creation holds the reporter account lock and permits at most five new reports in
24 hours. This quota is persistent and concurrent requests cannot exceed it.

Evidence is an immutable snapshot. Later edits, deletion, blocking or revoked
friendship do not change the submitted report. Reviewers receive no attachment byte
capability, conversation history, neighboring messages, or source-message lookup API.
Private reports do not grant instance administrators private conversation access.
The user's own report inbox contains only their reports and the review decision.

## Review boundaries

Settings → Safety & reports offers My reports, Instance review, and Selected channel
review. Select a server text channel before opening its inbox. Each bounded page is
authorized anew and uses forward `(created_at,id)` keyset pagination (default 50,
maximum 100). The frontend keeps one page at a time.

Instance owners/administrators can review **only** account and private-message
reports submitted to the instance. Guild reports are not included in that inbox.
Guild message reports require current membership plus View channel, Read history
and Manage messages in the report's original channel. Instance status grants none
of these guild rights. Changing roles or channel access removes review access on
the next request; the reporter's own submitted copy remains visible in My reports.

Resolution requires an open report, its current revision, a resolved/dismissed
state and a 1–2000 character explanation. The decision records reviewer identity
internally and a UTC timestamp. Guild resolution additionally locks the guild,
checks its submitted revision and current channel permissions, then advances the
guild revision in the same transaction. Instance resolution retains the instance
lock through authorization and write. Replaying the immediately preceding identical
instance decision is idempotent. Guild retries must also carry a current guild
revision. Review decisions do not automatically ban users or mutate messages;
reviewers use the separately authorized moderation controls where appropriate.

## Retention, rollback and validation

Blocks disappear with account deletion. Private conversations/attachments still
follow the existing account-deletion cascade. Submitted report reasons/evidence
remain for review after reporter or subject deletion; their account references are
set to null. Guild/channel deletion cascades that scope's reports. There is no
automatic report-retention purge or report export in this implementation. Report
data is sensitive and belongs in the backend's private database/backup policy.

Migration `20261023140000_private_safety` adds the DM revision/tombstone/fingerprint
fields, private attachments, blocks and reports. Rollback permanently discards block
and report records, file bytes and attachment metadata; removes tombstones and
attachment-only messages; and discards DM revision/fingerprint state. It does not
restore a friendship removed by blocking or recover deleted message text.

Tests cover author/revision authorization, exact retries after edit/delete, unread
tombstones, typing expiry/disconnect, live/history snapshots, private file ownership,
staging quotas/expiry, competing claims, stale download revocation, block/unblock
races, call/socket revocation, forged report rejection, report quotas and isolated
instance/guild review. Loopback media tests check both directions after blocking.
Physical desktop/WebView acceptance remains separate from these automated checks.
