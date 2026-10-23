# Scripting bots

Write a native Rust program, register async functions, and run it wherever you
host your bot. A message such as `@jukebox /play https://youtu.be/VIDEO_ID`
dispatches `play(context, args)`, with the argument text as a `String`.
No Tauri window, audio device, microphone processing, or knowledge of WebRTC is
needed. This is a Rust SDK, not an in-app interpreter or an untrusted-code sandbox.

The SDK and server source live here: `contracts.rs` is compiled by `shared`,
`server/` by `backend`, and `src/` plus `examples/` by `frontend` with its
`scripting-bot` feature. The management UI and native credential command live in
`frontend/src`; desktop and SDK share `src/credentials.rs`. There are still
exactly three Cargo packages. Existing
instance-owner diagnostic [voice test bots](../docs/voice-bots.md) are separate.

## Quick start

1. In the app, open your server's **Settings → Bots** and create `jukebox`.
   You need **Manage server** permission. On desktop, the token is automatically
   saved in your OS credential store for the examples on that computer. In a
   browser, copy the one-time token to your bot host's secret manager and inject
   it as `THISCORD_BOT_TOKEN`.
2. Give the bot access to a text channel such as `general` and a voice channel
   such as `Music`. **Manage roles** opens the bot's member settings; use
   **Channels & permissions** for overrides. It needs View channel, Read message
   history and Send messages for text, and View channel, Join voice and Speak
   for music.
3. From this repository, run the echo example first. PowerShell:

   ```powershell
   $env:THISCORD_BOT_SERVER = "https://thiscord.com.tr"
   $env:THISCORD_BOT_NAME = "jukebox"
   $env:THISCORD_BOT_TEXT_CHANNEL = "general"
   cargo run -p thiscord-frontend --features scripting-bot --example bot_echo
   ```

   Use your actual backend origin (for local development, usually
   `http://localhost:3000`). If its socket allowlist only includes Trunk, also set
   `$env:THISCORD_BOT_ORIGIN = "http://localhost:1420"`.
   In `general`, send `@jukebox /ping` or `@jukebox /echo hello`.
4. For music, install FFmpeg and yt-dlp on the bot host, stop the echo example,
   set `$env:THISCORD_BOT_VOICE_CHANNEL = "Music"`, and run:

   ```powershell
   cargo run -p thiscord-frontend --features scripting-bot --example bot_music
   ```

   Send `@jukebox /play https://youtu.be/VIDEO_ID`, then `@jukebox /stop` or
   `@jukebox /leave`. `/help` lists the available commands.

On macOS/Linux, use `export THISCORD_BOT_NAME="jukebox"` and the same form for
the other variables. Channel values are **names**, not IDs: names match exactly
ignoring case and surrounding input whitespace, within the requested text/voice
type. Only channels visible to the bot are searched. Duplicate names within the
same type produce an error; rename one or use its UUID to disambiguate. UUIDs
remain supported. The default names are `general` for text and `Music` for voice.

The app creates the bot's identity; the Rust program must stay running to respond.
Use **Rotate token** to replace a lost credential (and restart the program), or
**Delete bot** to revoke it permanently. Browser tokens are never persisted.

## Register commands

```rust,no_run
use thiscord_frontend::bots::{Bot, Context, Credentials, Error};

async fn echo(ctx: Context, args: String) -> Result<(), Error> {
    ctx.reply(args).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = "https://thiscord.com.tr";
    let bot = Bot::new(server, Credentials::load(server, "mybot")?)?
        .text_channel("general").await?
        .command("echo", "Repeat your text", echo)?;
    bot.run().await?;
    Ok(())
}
```

Commands start with an exact bot mention followed by a slash command. Bot names
are case-insensitive; command names are lowercase letters, digits or underscores,
up to 32 bytes. `/help` is built in. Leading whitespace before arguments is removed;
internal whitespace, Unicode, quotes and trailing whitespace are preserved. The
public `parse_command` helper returns borrowed `(&str, &str)` slices.

Register closures to capture your own application state. `Context` contains the
original message, author/channel IDs and a cloneable `Client`. `on_mention` handles
non-command mentions. For your own dispatcher, use `Subscription::connect`,
`client()` and `next()` directly (`connect_with_origin` accepts a custom Origin).
The builder's `.origin(...)` selects an explicitly
allowed server Origin; by default it uses the server URL's origin.

For a separate Rust project, depend on this repository's existing frontend
package with default features disabled and `features = ["scripting-bot"]`:

```toml
[dependencies]
thiscord-frontend = { git = "https://github.com/BugraEryilmaz/thiscord", default-features = false, features = ["scripting-bot"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Pin `rev` to the deployed server's matching commit in production. The repository's
Rust version and native Opus build prerequisites apply. Linux also needs
`pkg-config` and `libdbus-1-dev` for OS credential integration. This feature does not
enable CPAL, noise suppression, Tauri or the desktop audio pipeline.

## Command-line provisioning (optional)

Deploy the backend migration first. A human guild member with **ManageGuild** can
create a bot. The guild owner can manage all bots; other managers must outrank a
bot before rotating or deleting it. Issued credentials cannot delegate permissions
the issuing manager lacks, including per-channel overrides. Names are globally unique handles containing
3–32 lowercase letters, digits or underscores. A guild can have ten bots, subject
also to its member quota. Bots show a `[bot]` display-name suffix.

Set `THISCORD_BOT_SERVER` to the backend origin, `THISCORD_BOT_GUILD` to the guild
UUID. Sign in with the desktop app first: provisioning loads its session from
the OS credential store using the same exact server URL. Alternatively inject a
human session into `THISCORD_USER_TOKEN` through your private local environment.
No credentials belong in arguments, logs or source control.
The provisioning example automatically reads the guild revision before writes:

```sh
cargo run -p thiscord-frontend --features scripting-bot --example bot_manage -- list
cargo run -p thiscord-frontend --features scripting-bot --example bot_manage -- create jukebox
```

The token is saved in the OS credential store under its server origin and bot name, never printed
or written to a plaintext file. If saving fails, fix access to the credential
store and use `rotate BOT_ID` to issue a replacement. Linux credential storage
needs a running Secret Service; headless services can use `Manager` and their own
secret manager. Revoke with `delete BOT_ID`; `rotate BOT_ID` immediately invalidates
the previous token and its active sockets/media. Tokens otherwise remain valid
until revoked or the bot's guild membership is removed. Each name represents one
guild installation; create another bot for another guild.

Set these nonsecret variables for the runnable examples:

- `THISCORD_BOT_NAME`: the saved credential name (for example `jukebox`).
- `THISCORD_BOT_TEXT_CHANNEL`: a text-channel name or UUID (default `general`).
- `THISCORD_BOT_VOICE_CHANNEL`: a voice-channel name or UUID (default `Music`).
- `THISCORD_BOT_ORIGIN`: optional origin from the backend's allowlist, needed when
  the API origin is not itself allowed (for example the local Trunk origin).
- `THISCORD_BOT_AUDIO_FILE`: optional operator-selected local audio file.

A service may inject `THISCORD_BOT_TOKEN` instead of loading a saved credential.
The endpoint must use HTTPS, except for HTTP on localhost/loopback.

```sh
cargo run -p thiscord-frontend --features scripting-bot --example bot_echo
cargo run -p thiscord-frontend --features scripting-bot --example bot_music
```

Grant the bot **ViewChannel**, **ReadHistory**, and **SendMessages** in subscribed
text channels, plus **ViewChannel**, **JoinVoice**, and **Speak** in its destination
voice channel. Use the existing guild role/member controls and channel overrides.
The bot receives `@everyone` role permissions like other guild members. Its own
token cannot edit roles, manage accounts, send private messages or administer the
instance. Give a bot only the channel permissions its functions need.

## Text and voice actions

`Bot::text_channel("general").await?` resolves a text channel before subscribing.
`bot.voice_channel("Music").await?` resolves a voice destination before registering
handlers. `bot.channels().await?` and `ctx.client.channels().await?` list the bot's
currently visible channels, with names, kinds and IDs. For example, an existing
handler can resolve another destination with
`ctx.client.find_channel("Music", ChannelKind::Voice).await?`.
Names are resolved to stable IDs; renaming a channel does not retarget an already
running example. Visibility does not grant send/join permission: each action is
still authorized by the backend. Hidden channels are never returned.

`ctx.reply(text)` replies in the invoking text channel.
`ctx.client.send(channel_id, text)` sends to another authorized text channel in
the bot's guild. To retry an ambiguous network send, reuse a `ClientMessageId`
with `send_with_id`; repeated IDs with identical content deduplicate on the server.

`ctx.join_voice(channel_id).await?` returns a cloneable `Voice` handle. Keep it in
your application state; its last drop closes playback and the media connection.
`play_file(path)` and `play_youtube(url)` start or replace playback, `stop()` stops
and keeps the connection, and `leave()` stops and leaves. `status()` provides a
watch receiver for Idle/Starting/Playing/Finished/Stopped/Error/Left. A successful
play call accepts the request; later extractor/decoder errors appear in status.

The music example offers `/play URL`, `/local`, `/stop`, `/status`, `/leave`, and
`/help`. The voice destination and local-file path are fixed by the operator;
chat messages cannot choose arbitrary files. These controls are available to
humans able to post in the configured text channel. Add an author allowlist or
your own command policy when needed. It plays one track at a time; another play
replaces it. There is no playlist, search, queue persistence or automatic rejoin.

Install [FFmpeg](https://ffmpeg.org/download.html) on the bot host for playback,
and [yt-dlp](https://github.com/yt-dlp/yt-dlp#installation) for YouTube. These are
external media tools; bot logic, networking and Opus/WebRTC remain Rust. No Node
pipeline is introduced. Use current supported extractor tools; some YouTube
videos need upstream extractor prerequisites or are unavailable. Only single
HTTPS YouTube video links are accepted, with no cookies or account login. Live
streams and arbitrary remote URLs are unsupported. Use media you have permission
to share. The example does not launch a browser or capture system audio.

Audio is decoded to 48 kHz stereo PCM through bounded pipes, encoded as 20 ms
Opus frames, and paced into the existing SFU. No microphone, echo cancellation,
noise suppression, audio device or receive pipeline is created. The send-only
peer uses the existing server ICE/STUN/TURN configuration and DTLS-SRTP; the SFU
terminates each encrypted hop (not end-to-end encryption). The host needs network
access to those media endpoints. Playback has bounded startup/read/cleanup waits,
a six-hour maximum and no downloaded media files. Stop, leave, disconnect and
lost permissions terminate child processes. Trusted executables on PATH handle
media; they receive neither bot tokens nor the application's environment secrets.

## Delivery and permission semantics

Subscriptions cover 1–32 explicitly selected text channels. The server sends only
new committed human messages containing the bot's resolved mention ID. Old
history, edits, exact send retries, bot messages and unrelated chats do not invoke
commands. Commands execute serially with a 30-second handler deadline; heartbeats
and voice playback run independently. The client inbox is bounded to 32 events,
and the server's bounded live stream disconnects slow subscribers on overflow.
Revocation or disconnect discards queued commands; an already running handler
remains bounded by its deadline.

This is live best-effort delivery, without an offline command backlog or an
exactly-once execution guarantee across multiple bot processes. Run one dispatcher
per bot unless your application deduplicates message IDs. `run()` returns an error
on lost/revoked subscriptions rather than silently replaying commands. Supervisors
may restart after transient failures; fix credentials/permissions before restarting
revoked bots. Already delivered data and arbitrary external effects of a handler
cannot be recalled. Avoid detaching unbounded background tasks in handlers.

Backend permission checks remain authoritative for subscriptions, text actions,
voice joins and media ingress/egress. Rotation, deletion and guild/member changes
use the existing access barriers. Guild permission changes close subscriptions;
restart to establish a fresh authorized view. This uses the repository's existing
single-backend-process invalidation model. Bots are credential-bearing service
principals in the account/member tables, with no human login identity. The
prefixed, hashed bot token is rejected by human-only APIs, including when stripped
of its prefix. Bot management holds the guild lock and checks the current revision.
Channel discovery posts `{}` to `/api/v1/bots/channels` with a bot token; the
server derives the guild from that token and filters by current ViewChannel
permission under the guild lock. A request cannot select another guild.

## Checks

```sh
cargo test -p thiscord-shared --locked
cargo test -p thiscord-frontend --lib --features scripting-bot --locked bots::
cargo clippy -p thiscord-frontend --lib --example bot_echo --example bot_music --example bot_manage --features scripting-bot --locked -- -D warnings
# With FFmpeg installed:
cargo test -p thiscord-frontend --lib --features scripting-bot --locked ffmpeg_ -- --ignored
```

Backend PostgreSQL/live-socket coverage runs with the normal ignored integration
tests in [database.md](../docs/database.md). CI builds/lints/tests the SDK on
Windows, macOS and Linux. A native build alone does not establish real YouTube,
NAT/TURN, credential-store, or audible playback acceptance on all three hosts.

The familiar play/stop/leave controls were informed by
[Jockie Music's public command reference](https://www.jockiemusic.com/commands.html?q=help).
