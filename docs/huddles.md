# Voice huddles and moving people between channels

## Huddles

A huddle is a private sub-call inside a voice channel. Plan up to four huddles,
then **split** the room: each huddle hears only itself, and people not in a huddle
stay together in the **main room**. **Bring everyone back** (or the shortcut again)
ends the split. Nobody reconnects or changes channel, so a split takes effect at
once and costs nothing to undo. Made for moments like a duo game's "what should I
send my teammate" phase, where three pairs need to talk at the same time.

Only voices are separated: microphones, shared screen/system audio and soundboard
plays. Screens and cameras stay watchable from every huddle. Self mute/deafen,
server mute/deafen and speaking indicators work as usual.

### Using it

- **Plan.** In a connected server voice channel, the voice dock shows a **Huddles**
  button to people who may manage huddles there. It opens a planner with the main
  room and four huddle lanes. Drag people between lanes; or click someone and then
  **Move here** on a lane; or focus someone and press 1–4 (0 for the main room).
  Focus starts on the first person, stays on whoever you just moved, and each
  change is announced to screen readers.
  The plan shows in the sidebar as a colored number next to each person.
- **Split or regroup.** The planner's main button, or the **Split or regroup
  huddles** shortcut, recorded right in the planner (or in Settings > Shortcuts). The shortcut is off until you record
  one, like the overlay shortcuts. On desktop it is a global shortcut, so it works
  while a game is focused; in the browser it works while the tab is focused. With
  no plan yet, the shortcut explains that one is needed instead of doing nothing.
- **While split** the sidebar groups the channel by huddle with a colored rail and
  marks people you cannot hear (dimmed avatar, muted name and a speaker-off icon),
  and the voice dock says which huddle you are in and who you hear; hovering it
  shows who split the room. The Huddles button turns amber.
- **Everyone is told.** Any change to who hears whom plays a short cue for the
  people in the call, distinct from the join/leave sounds: falling for a split,
  rising for a regroup, a double blip for someone moved to another huddle while
  split. Whoever pressed the shortcut from a full-screen game hears it too (not
  while deafened), and a shortcut that fails (no plan, not in a server call, no
  permission, rate limit) plays a low buzz. Screen readers hear "Alice split the
  room. You are in Huddle 2 with Carol.", "Bob moved you to another huddle…" or
  "Alice brought everyone back together.", and the dock line stays readable.
  Joining a room that is already split describes it ("This room is split…")
  without a cue, and reconnecting announces nothing.
- **Clear huddles** forgets the plan and regroups.

### Rules

- **ManageHuddles** ("Split voice into huddles") is needed to change the plan or
  split/regroup. It is not granted to Everyone; give it to a role, or as a channel
  override. Like other voice permissions it requires ViewChannel and JoinVoice in
  that channel (no JoinVoice removes it), owners and Administrator have it, and a
  timeout removes it. There is no hierarchy check: a split takes no access away from
  anyone, and everyone stays in the call.
- Requests go to `POST /api/v1/voice/huddles` (`shared/src/huddles.rs`). Each one
  rechecks the session, membership and current channel permissions under a shared
  guild lock (no cached grants and no client snapshot) and applies the change
  before that transaction ends, so a permission change, which locks the guild
  exclusively, cannot commit between the check and the change. Before any
  database work each client address may make 120 requests per 10 seconds
  (taken from the connection, like the account limiter; proxy headers are
  ignored), so floods of invalid or refused requests stay bounded, and each
  account may make 30 changes per 10 seconds after authentication (HTTP 429
  beyond either, in memory per backend process). Changes are idempotent
  (assign, split on/off, clear), so two people pressing the shortcut together
  converge instead of toggling back. They need no guild revision: a plan is not
  guild configuration and grants nothing.
- Only server voice channels have huddles; private and group calls do not. Only
  people currently in the channel can be planned, and huddles are numbered 1–4
  (`MAX_HUDDLES`; a full room of eight fits four pairs). A request for an empty
  channel fails with "Nobody is in this voice channel".

### Attribution and abuse

There is deliberately no hierarchy check, so someone with ManageHuddles can put
anyone, including the owner, in a huddle of their own and split the room. Nobody
loses access: the isolated person still sees the split and who did it (the dock
and rosters carry `split_by`, the last person to split, regroup or move someone
while split), can leave,
and anyone with the permission can regroup or clear. Owners and role managers
can take the permission away; the next request is refused. The backend logs every
change at info level with the request ID, the actor's account ID, the guild and
channel IDs and the action (assign, split, regroup, clear), but no names or
other content. Give the permission only to people you trust to run the room, as
with Move members.

### Lifecycle

The plan lives in the backend's in-memory room, not the database. It survives
people leaving and rejoining (including automatic reconnects and moderator moves
back) while anyone remains in the channel, and is forgotten when the room empties
or the backend restarts (the room then comes back together). At most 16
assignments are kept per room; past that, assignments of people who left are
dropped.

### How the SFU enforces it

`voice/huddles.rs` mirrors the plan into each member's `Participant::{huddle,
split}` under the room write lock, including members admitted later. Both SFU
transports call `viewing::receives` before admitting a packet and again right
before writing it, and it refuses non-video media between different huddles of a
split room, so packets queued before a split are dropped too. This is the one
forwarding rule both transports share by construction (see
[sfu-transport.md](sfu-transport.md)). The same fields flow to rosters (sidebar and
call participants), which is how clients show the plan. Soundboard plays carry the
player's huddle and are only delivered within it.

A split is a pause in the forwarded streams, like a listener deafening, so a very
long split (more than roughly ten minutes) leaves a large RTP sequence gap when it
ends; clients treat it like any other gap.

Clients older than this feature ignore the new roster fields: in a split room
they simply stop hearing other huddles, with no explanation. Update every client
before using huddles.

### Tests

`huddles.rs` covers the per-account and per-address limits, and the voice suite
checks that a flood of fresh invalid credentials from one address gets 429 before
authentication; `shared/src/huddles.rs` covers the
wire format, bounds and soundboard filtering; `frontend/src/huddles.rs` covers the
optimistic overlay (shown only until the roster changes, so a stale overlay never
decides what the shortcut does, and chained changes do not bounce back), the
toggle direction, companions, which changes are announced (join, reconnect,
split, regroup, moved) and drop-target filtering.
`viewing.rs` unit tests cover separation of microphone and screen audio, video
staying watchable, idempotent splits, regrouping, unknown people and clearing;
`membership.rs` covers rejoining into the same huddle. The PostgreSQL voice suite
(`backend/tests/voice/huddles.rs`, run under both transports in CI) covers the
permission (default deny, channel override grant and revocation), validation, the
live-room requirement and real-peer forwarding before, during and after a split.

## Moving people by dragging

People with **MoveMembers** can drag someone from a sidebar voice roster onto
another voice channel of the same server, or onto that channel's list of people.
A row is draggable only when the moderation view (loaded when the server view
opens, then at most every ten seconds) says you may move that person. Only
channels a move could use are offered, with a dashed outline while dragging: with
room, and where both of you have ViewChannel and JoinVoice (the moderation view's
`move_channels`). The status bar shows "Moving Frank to AFK…", Frank appears in
AFK marked "Moving…" while he reconnects, and the status bar says "Moved Frank
to AFK" once he is there (or that he has not arrived yet, after ten seconds). The drop uses the existing moderator move
(`GuildChange::MoveVoice`, see [permissions.md](permissions.md)) with fresh
permissions and the current guild revision: the target must rank below you, both
of you need access to both channels, and the destination needs room; a failure is
explained in the status bar. Keyboard and touch users can do the same from the participant's right-click (or
Shift+F10) menu: **Move to <channel>** for each channel a move could use, when
you may move that person, with the same progress and result as dragging.
