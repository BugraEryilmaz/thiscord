# Soundboard

Guild voice calls have a soundboard. The **Soundboard** button in the connected
voice dock opens a grid of the server's sounds; clicking one plays it for everyone
in the call, including you. Playing needs the desktop app, like the rest of voice.

## Built-in sounds

Every server has four built-in sounds: **cricket**, **clap**, **sad horn** (the
classic "wah wah" trombone) and **ba dum tss**. They are openly licensed (CC0)
recordings from BigSoundBank and Freesound, stored unmodified in
`backend/assets/soundboard/`. That folder's README lists each file's source, author,
license and SHA-256. They are not copied from Discord or other apps. The backend
embeds the files and runs them through the upload pipeline once per process, so
longer recordings are cut to their first 5 seconds. The results are never stored
in the database. They have fixed IDs, do not count toward a server's limit and cannot be
deleted; the same Use soundboard permission governs them. The UI translates their
names.

## Managing sounds

Every server can add its own sounds. Members with **Manage server** open **Server
settings > Soundboard** to upload or delete them. Each server holds up to 24 uploaded
sounds (`MAX_SOUNDS`) in addition to the built-ins, independent of instance roles. Uploads accept WAV, MP3, Ogg Vorbis or FLAC files up to 1 MiB with
a 1–32 character name and an optional emoji.

The backend decodes uploads with pure-Rust codecs (Symphonia) on a bounded blocking
worker, after authorizing the actor and applying a six-uploads-per-minute account
limit, and without holding a database connection while decoding. It downmixes to
mono, trims leading/trailing silence, resamples to 48 kHz, cuts at 5 seconds with a
10 ms fade, and normalizes loudness (about −18 dBFS RMS, peak ≤ −1 dBFS, at most
+18 dB gain). Only the normalized 16-bit PCM (≤ 480 KB) is stored in the
`guild_sounds` table, so sounds are part of normal database backups. Original files
are not kept. Writes lock the guild row, recheck Manage server and the viewed guild
revision (stale writes return 409), and enforce the count limit in the same transaction.

## Permissions

**Use soundboard** controls who can see the grid and play sounds in a voice
channel. New servers grant it to Everyone; the soundboard migration adds it to every
existing Everyone role. Remove it from Everyone, or deny it with a channel override,
to restrict it. Like Speak, it is removed when a member lacks View channel or Join
voice in that channel. Playing additionally requires Speak, not being server-muted,
timed out or deafened.

Listeners do not need Use soundboard to hear a sound: anyone currently allowed in
the voice channel (View channel + Join voice) may download the clips that are played
there. Instance administrators have no implicit access.

## Playback path

1. The UI sends `voice_soundboard_play(guild, channel, sound)` to the desktop
   process. The command is accepted only for the current negotiated guild call.
2. The desktop sends `ClientEvent::Soundboard` over the voice signaling socket.
   Servers advertise support with `Offer.soundboard`; clients opt in with
   `Answer.soundboard`. Sending the event without opting in ends the connection.
3. The SFU drops plays closer than one second apart per connection, checks the
   participant's live state, and authorizes the play against current database
   permissions (and that the sound belongs to the guild). After that check it
   confirms the connection's access epoch is unchanged before broadcasting.
4. Each opted-in, non-deafened room member with current access receives
   `ServerEvent::Soundboard { sound_id, account_id }`. Older clients never receive it.
5. The desktop downloads `GET /api/v1/soundboard/{guild}/{channel}/{sound}` with
   its session token (bounded to 480 KB, `no-store`), validates the PCM and keeps up
   to 32 clips in memory for the call. Clips are dropped when the call ends.
   A clip that arrives more than 3 seconds after its play is cached but not played.
6. The audio worker hands the clip to the output callback through a fixed-capacity
   SPSC queue. Up to four clips overlap; finished clips return through a second
   queue so memory is never freed in the callback. Sounds follow the output device,
   master volume and deafen, and are included in the echo reference. They are not
   inserted into anyone's microphone stream.

## Per-person soundboard volume

Right-click another participant in a voice channel to find a **Soundboard volume**
slider (0-200%, default 100%) below their voice volume. It sets how loud that
person's sounds play on this device only: other participants are unaffected, and
the person is not notified. At 0% the desktop ignores their plays before downloading
anything. Levels are stored per account ID in `soundboard-volumes.json` in the app
config directory (only non-default levels, at most 1000). They apply in every server,
persist across restarts and are shared by accounts using this OS profile, like
speaker volumes. Each queued clip carries its own gain, so overlapping sounds from
different people keep their own levels.

PCM never crosses Tauri IPC or WASM. Logs record that a sound was played, without
sound names or audio. Private calls and voice test bots have no soundboard.

## Limits and follow-ups

- The sound limit is a fixed constant; per-instance-role allowances are not
  implemented.
- There is no rename, per-sound volume, overall soundboard volume, local preview or
  way for a server to hide built-in sounds.
- Playback timing differs by each client's download latency on first use.
- `use_soundboard` is a new permission name, and older clients reject unknown
  names. Until they update, older desktops cannot load Roles/Channels & permissions,
  lose permission-based channel context-menu items (Mark as read, voice test bots)
  and cannot resolve channel reports. Voice, chat and joining are unaffected.
  Install the update on all clients.
- Physical Windows/macOS/Linux listening and UI acceptance are still required.
