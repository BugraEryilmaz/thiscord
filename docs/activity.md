# Game activity ("Playing …")

The desktop app detects known games running on the computer and shares them as a
live status line, for example **Playing Factorio** or **Playing Teamfight Tactics**.
Friends see it in friend rows and the direct-message header; fellow server members
see it in the member list, profile cards and voice-roster tooltips. Your account
ribbon shows your own. In compact status lines an activity takes precedence over
the custom status, which remains visible again once the game closes.

## Desktop detection

`native_activity.rs` scans process names every 15 seconds on a background thread
and matches them with the built-in table in `game_activity.rs` (lower-cased file
name without directories or `.exe`). Per platform:

- Windows: `CreateToolhelp32Snapshot` process entries.
- Linux: `/proc/<pid>/cmdline` first argument and the `exe` link. Wine/Proton games
  match through their Windows `.exe` path.
- macOS: `proc_listallpids`/`proc_pidpath` (libproc).

Process names never leave the device and are never logged; only the matched
display name reaches the UI and the server. A League of Legends match wins over the
League client, and otherwise the earliest table entry wins.

League of Legends and Teamfight Tactics use the same game executable. During a
match the app asks the game's local live client API
(`https://127.0.0.1:2999/liveclientdata/gamestats`, short timeout, no proxy) for
`gameMode`; `TFT` means Teamfight Tactics. The API serves Riot's self-signed
certificate, which is not verified: nothing is sent, and only one of two fixed
labels is derived from the reply. Until the match has loaded (and in the client,
lobbies or queues) the status is **Playing League of Legends**.

**Settings → App behavior → Activity status** turns detection off. The preference is
device-local (`activity.json` in the app config directory) and on by default.
Turning it off stops scanning and clears the shared activity immediately. The
browser `/app` client never detects or shares activities. The built-in table is
`GAMES` (tests enforce normalized, unique process names).

### Added and hidden games

The same page shows the game detected right now. A built-in game can be hidden with
**Hide this game**; **Hidden games** lists those names with **Show again**. Hiding
League of Legends or Teamfight Tactics hides that mode only.

**Your games → Add a game** opens a picker of programs running in this user's
session, with search and refresh, plus **Program** and **Shown as** fields. Choosing
a program fills both with its process name and a readable suggestion
("SonsOfTheForest.exe" → "Sons Of The Forest"); typing a program file name works for
games that are not running. Added games match before the built-in table, so adding
a known program under another name renames it. Up to 50 games; names follow the
activity rules, process names are normalized like the scanner's and must be unique.

The picker lists Windows programs with a visible, titled top-level window; on Linux,
this user's processes outside system library directories (Steam's runtime stays);
on macOS, executables outside the operating system's directories. Its process list
reaches the settings page over Tauri IPC only while the picker is open and is never
sent to the server or logged. `activity.json` stores `enabled`, `games` and `hidden`
as JSON; older files containing only `true`/`false` still load, and a damaged list
falls back to the defaults. Saving rescans immediately.

## Wire and server

`Activity { kind: "playing", name }` lives in `shared/src/account.rs`. Names are
client-reported plain text: trimmed, 1–64 characters, no control characters, and
rendered as text nodes only.

The friends socket carries it. Snapshots from servers that support activities set
`accepts_activity`; only then does the client send
`{"type":"activity","activity":…|null}` when its detection changes (older servers
close sockets that send unknown events). A malformed name closes the socket.

The server keeps each socket's activity in the in-memory presence registry and
never stores it. It ends when that socket disconnects; with several devices the
most recently reported activity wins. Its audience is exactly online presence:

- `Friend.activity` for online accepted friends (never pending requests or across
  a block, which removes the relationship from the list);
- `GuildMember.activity` in member lists and the additive `activities` field of
  `member_presence` (only for members in `online`, never across a block).

Presence ticks (one second) deliver changes; no database writes or wake-ups are
involved. Like presence, this is per backend process.

## Limitations

- The game table is curated by hand; process names of new releases or launchers
  may differ (add them under **Your games**). Games running as another user or
  inside sandboxes may be invisible.
- Added and hidden games are per device; they do not follow the account.
- The macOS and Linux program pickers are unverified on hardware.
- Teamfight Tactics is recognized only inside a loaded match.
- macOS detection is unverified on hardware; Windows and Linux process listing is
  covered by tests on those hosts.
