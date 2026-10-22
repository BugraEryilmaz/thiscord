# Notifications

Open **Settings → Notifications** for account-wide event defaults. Each event has
independent sound, banner and volume controls, plus All activity, Mentions only or
Nothing. Mentions only matches resolved guild-message mentions; events without
mentions, including calls, are suppressed in that mode.

Defaults:

| Event | Sound | Banner |
| --- | --- | --- |
| Direct message | On | On |
| Incoming direct call | On | On, with Accept and Decline |
| Server message | On | Off |
| Someone joins/leaves your voice room | On | Off |
| Application update | Off | On |

Each event has a distinct sound, shared by Settings previews, browser playback,
desktop alerts and the in-call mixer: a bright double chime for direct messages,
a short low knock for server messages, two pairs of ringing tones for calls,
a rising sweep for voice joins, a falling sweep for leaves, and a four-note chime
for updates. Sounds vary in rhythm, pitch movement and timbre, with smooth edges
and bounded levels. The longest is the 1.09-second incoming-call cue. Browser
previews replace the previous sound to avoid overlapping previews.

Right-click a channel or conversation for notification settings or hover **Mute**
to choose 15 minutes, 1 hour, 1 day, 30 days, until manually unmuted, or Unmute.
The submenu also opens with a click, Enter, Space or Right Arrow; Left Arrow or
Escape returns to the parent menu. It opens to the left near the window edge.
Banners include the same snooze choices. A source override applies to every event
from that source; **Use event defaults** restores separate message/call/voice defaults. Snoozing does
not change the underlying settings or remove message history. Own messages and
own voice transitions do not alert. Voice membership replacements/resumptions do
not produce join/leave pairs. Voice notifications go to other participants in the
room, not everyone looking at its sidebar roster. Deafen suppresses notification
audio locally.
Ending or declining a private call also uses the voice-leave preference for the
other participant, even though the media room has already been revoked.

Preferences and mute deadlines live on the server, scoped to the account. Updates
propagate to other connected devices. Edit commands use revisions; snooze commands
serialize on the account and calculate their deadlines with the server clock.
Thirty days is the explicitly labeled meaning of one month. The server checks
expiry when delivering each candidate, without a periodic unmute job. The UI uses
a one-shot deadline timer to refresh mute indicators.
Client preference writes use a bounded FIFO; snooze waits for earlier saves and
its banner is dismissed after the server confirms the mute. **Remove saved settings
and mute** deletes an account's override even after access to that source is lost.

## Push transport and access

`POST /api/v1/notifications` accepts the shared `notifications::Request` contract.
`/api/v1/notifications/socket` requires an allowed Origin, then a version-1
`connect` frame with the bearer token and an explicit `kinds` list. A `subscribe`
frame replaces that list; an empty list stops alerts. The app subscribes to all
six supported kinds. Source selection is governed by the account defaults and
saved subscriptions: accessible sources inherit defaults until overridden or
muted. Notification subscriptions are independent of the currently viewed chat.
They never grant access to a channel or a private conversation.

The socket returns `ready` with saved preferences and pushes `alert` events with
the server-selected delivery settings. There is no HTTP notification poll and no
server timer discovering notification activity. Guild messages publish only after
new sends commit; edits, deletions and exact send retries do not alert. Direct
messages and call invitation changes publish after commit on their blocking workers,
including when the initiating HTTP future was cancelled. Voice membership changes
publish from the SFU registry. Release publication is an explicit command.

Every delivery revalidates the session, current guild membership/channel grants
or accepted friendship, and current preferences in a locked database transaction.
Queued calls also require an unexpired ringing invitation addressed to the
recipient. Initial synchronization recovers still-ringing incoming calls. A `calls`
state frame clears accepted/ended invitations on all devices, independently of
notification preferences or selected kinds. A one-shot expiry deadline also clears
expired calls. PostgreSQL computes the remaining lifetime at statement time after
authorization, rounded up to milliseconds; the socket anchors that duration to its
monotonic clock after receiving the result. Lock waits and application/database
wall-clock differences cannot cause an early expiry refresh. No friends-snapshot
polling drives banner cleanup. Replayed calls
restore actionable banners without repeating sound. Call-state epochs fence
ringing delivery across acceptance/cancellation.
Account/session/guild/member epochs fence delivery across access
mutations. A separate preference epoch drains sends before mute changes commit,
without disconnecting media. Initial snapshots and subsequent preference updates
use the same fence. Backend authority remains mandatory even when the UI hides a
control. No message body or bearer token is included in diagnostic logging.

Limits: 128 notification sockets, 64 queued candidates per socket, eight blocking
notification workers, 512 source overrides per account, 4 KiB socket frames and
16 KiB HTTP requests. A slow/full subscriber disconnects rather than accumulating
an unlimited backlog. Established sockets wait fairly for a worker (one waiter
per socket, cancelled on revocation, with a ten-second overload deadline).
Guild sends narrow routing to connected guild members before per-recipient
authorization; unrelated guilds do not consume their delivery queues.
Heartbeats and session-expiry checks are maintenance, not
notification polling. Clients reconnect with bounded exponential backoff/jitter,
restore preferences and deduplicate up to 256 recent event IDs. Missed transient
message/voice alerts are not replayed; chat history remains the durable source of messages.
The latest release announcement is durable and offered on reconnect. Like chat
invalidation, this fanout supports one backend process.

Existing friends snapshots and sidebar voice-roster refreshes are separate view
transports; neither drives notification sound/banner discovery.

## Desktop and browser presentation

The desktop uses a compact, always-on-top Tauri banner window, including while the
main app is hidden in the tray. The transparent window fits the rendered cards and
stays inside the monitor's work area above the taskbar. Cards show an event icon,
source title, dismiss and snooze controls, with Accept/Decline for calls.
Its capability only permits reading presentation state, reporting bounded layout
height for the current revision, and acting on an existing banner. It has no
credential, account, network or media command access. Accept/Decline are forwarded
to the main window and then to the authenticated, authoritative call API. Clicking
a banner opens its source.
Up to three banners are retained, with 12-second lifetimes (30 seconds for calls).
Logout clears the native banner state and advances its session generation. Delayed
presentation/actions from the previous session are rejected; popup revisions prevent
an older initial snapshot from replacing a newer clear. In-app fallback also checks
the current session and ringing calls before displaying a delayed banner.
This is an app-owned window, not an OS
notification-center integration; there are no notifications after the app exits.

During voice, notification tones go through the native mixer and echo-reference
path, using bounded atomic requests and precomputed PCM. Outside voice, a bounded
native worker plays short tones on the default output device. Audio callbacks do
not allocate, lock, synthesize tones or use networking. Browser previews use
in-page banners and Web Audio; browser autoplay policy can require a user gesture
(the Preview sound button provides one). There is no service worker/mobile push.
OS focus/fullscreen/compositor behavior still requires physical acceptance on all
three desktop platforms.

## Release announcements

An instance owner/admin sends `{"action":"publish_update","version":"1.2.3"}`
to `/api/v1/notifications` using their backend session. This stores the latest
numeric release version and wakes subscribers after commit. Repeating the same
version is idempotent. Regular users cannot publish releases. Announcements never
contain download URLs or signing keys.

The release workflow can announce automatically **after** all signed platform
artifacts and the complete feed are published. Configure the repository variable
`THISCORD_NOTIFICATION_URL` to the backend HTTPS `/api/v1/notifications` endpoint
and secret `THISCORD_NOTIFICATION_TOKEN` to a valid instance-admin session. Session
expiry/revocation still applies; rotate it through normal account management. If
either is absent the release script reports that announcement is unconfigured.
An announcement failure after release publication must be retried as this API
command, not by republishing the release. Deploy this backend before enabling the
workflow step. No production configuration or release is changed by this feature.

The desktop checks the signed feed once when a newer announcement arrives, or
when the user explicitly chooses Check for updates. There is no launch/six-hour
feed polling or one-second UI status polling: native updater status is pushed via
Tauri events. Announcements cannot install software. Existing signature, signed
version, download-origin and explicit install/restart checks remain in force;
installation is still blocked during voice. Muting update alerts does not disable
the manual check in Settings → Updates.

## Verification

Shared tests cover defaults, filtering, stable wire forms and exact snooze expiry.
PostgreSQL/socket tests cover delivery without selecting a channel, self/outsider
suppression, duplicate sends and edits, current access, subscription changes,
cross-device preference propagation, revision conflicts, DM/call defaults, release
authorization/idempotency, snooze expiry and logout. Regression tests also cover
16-subscriber fanout, waiting-worker cancellation, call replay and pushed cleanup,
call delivery epochs, and removal of inaccessible-source settings. The frontend
queue test covers snoozing during a save and rebasing subsequent revisions. Run:

```sh
cargo test -p thiscord-shared -p thiscord-backend --locked -- --include-ignored
```

The notification migration has an explicit destructive rollback of preferences
and release announcements. Message/call records are unaffected.
