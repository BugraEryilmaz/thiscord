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
  The plan shows in the sidebar as a colored number next to each person.
- **Split or regroup.** The planner's main button, or the **Split or regroup
  huddles** shortcut (Settings > Shortcuts). The shortcut is off until you record
  one, like the overlay shortcuts. On desktop it is a global shortcut, so it works
  while a game is focused; in the browser it works while the tab is focused. With
  no plan yet, the shortcut explains that one is needed instead of doing nothing.
- **While split** the sidebar groups the channel by huddle with a colored rail,
  dims people you cannot hear, and the voice dock says which huddle you are in and
  who you hear. The Huddles button turns amber.
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
  guild lock (no cached grants and no client snapshot). Changes are idempotent
  (assign, split on/off, clear), so two people pressing the shortcut together
  converge instead of toggling back. They need no guild revision: a plan is not
  guild configuration and grants nothing.
- Only server voice channels have huddles; private and group calls do not. Only
  people currently in the channel can be planned, and huddles are numbered 1–4
  (`MAX_HUDDLES`; a full room of eight fits four pairs). A request for an empty
  channel fails with "Nobody is in this voice channel".

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

### Tests

`viewing.rs` unit tests cover separation of microphone and screen audio, video
staying watchable, idempotent splits, regrouping, unknown people and clearing;
`membership.rs` covers rejoining into the same huddle. The PostgreSQL voice suite
(`backend/tests/voice/huddles.rs`, run under both transports in CI) covers the
permission (default deny, channel override grant and revocation), validation, the
live-room requirement and real-peer forwarding before, during and after a split.

## Moving people by dragging

People with **MoveMembers** can drag someone from a sidebar voice roster onto
another voice channel of the same server. Valid channels get an outline while
dragging and highlight under the pointer. The drop uses the existing moderator move
(`GuildChange::MoveVoice`, see [permissions.md](permissions.md)) with fresh
permissions and the current guild revision: the target must rank below you, both
of you need access to both channels, and the destination needs room. The status bar
confirms the move or explains why it failed. Your own row is never draggable.
Keyboard and touch users can do the same from the member's **Moderate** panel.
