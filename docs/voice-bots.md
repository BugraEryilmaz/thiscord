# Owner voice test bots

Sign in as the **instance owner**, right-click a voice channel and choose
**Voice test bots…**. Choose a connection profile and microphone loop, then
**Add bot to this channel**. Repeat to add independent participants. The owner
must also belong to the guild and have ViewChannel, JoinVoice and Speak there.
Instance admins and guild owners alone cannot use these controls.

Join the channel with the desktop client to listen. Bots appear in the ordinary
voice roster, overlay, notifications and per-speaker volume controls. Right-click
a bot for its test audio settings, or click its name in the browser `/admin`
participant table as the instance owner. Open **Voice settings and diagnostics**
for the same Audio & voice panel used by real users. Changes save automatically:
voice activation/PTT, threshold, gain, volume, mute/deafen, Sonora/DeepFilterNet3,
automatic gain, echo cancellation, bundled neural echo processing, adaptive Opus
FEC, RED, DRED and per-speaker volume all run through the desktop `AudioEngine`.
The bot's volume affects its silent speakers; your participant slider controls
what **you** hear. Physical devices are replaced with virtual devices. Keyboard
shortcuts, overlays and arbitrary model-file overrides are unavailable on bots.

Use **Apply test conditions** separately to change connection profile, microphone
loop, acoustic scenario or the held virtual PTT key. Conditions do not overwrite
voice-panel edits. Choose speech (the existing synthetic DSP fixture), chimes,
a low pulse or melody. Speech includes a one-second quiet gap; tones have a quiet
gap in their three-second cycle. Each bot has its own clock, engine and settings.
Virtual capture and RTP timing follow sample counts rather than thread wake-up
times, so ordinary scheduling jitter does not invent missing audio. Catch-up is
bounded to 40 ms; longer stalls remain real capture gaps in diagnostics. Tone
endings fade out before each phase reset and quiet gap to avoid source clicks.
Clean, quiet (8% source level), noise, silence, speaker echo and double-talk
scenarios are deterministic. Echo returns the bot's actual speaker mix to its
microphone at 50% level after 80 ms. The normal render-reference/AEC path receives
the same mix. Another speaking bot or user supplies the far end for echo tests.

For quick loss comparisons, use the **Opus baseline**, **RED**, **DRED**, or
**RED + DRED** buttons, then select the same loss profile on each trial. These
buttons change only the two redundancy switches. DRED recovery requires enabling
DRED on both endpoints; a listening bot can use the Silence scenario. Advanced
voice diagnostics show RED history frames selected for playout, DRED attempts and
output frames, FEC attempts, PLC, buffering, processing deadlines, and clipping.
These same counters are available for human clients. Opus FEC calls can contain
PLC, so they are not reported as verified recovery. Diagnostic JSON downloads
contain technical data, never recorded media. Logs are opt-in and separate for
each companion, with fixed-size rotation; normal exits remove its temporary logs.

Refresh the manager for media counters and camera/screen resolution, decoded
frames, discarded/missing packets, decode errors, keyframe requests and requested
bitrate. Audio packet counters count received transport packets; native diagnostics
report decoding and recovery. Video counters reset when subscriptions or profiles
change. Conflicting condition revisions or voice-settings snapshots reject stale
edits. Remove stops the companion and releases its slot.

Connection presets apply independently to the microphone uplink and the aggregate
incoming audio downlink. A separate aggregate video downlink uses the same
delay/jitter/loss, with these budgets:

- Good: 1000 kbit/s media-link budget, 20 ms one-way delay, no injected loss; 32 Mbit/s video.
- Low bandwidth: 24 kbit/s, 120–150 ms delay; 500 kbit/s video.
- High packet loss: 1000 kbit/s, 60–80 ms delay, 25% deterministic packet loss; 32 Mbit/s video.
- Unstable: 48 kbit/s, 100–200 ms delay, 10% deterministic packet loss; 2 Mbit/s video.

All presets use the normal encoder: 32 kbit/s, or 40 kbit/s with DRED. RED adds
its normal history overhead; good/high-loss budgets accommodate multiple sources.
Low bandwidth deliberately competes with the encoder budget and adds drops. Budgets charge payload plus 60 bytes per packet
and permit at most 100 ms of burst credit; bandwidth exhaustion adds loss. Delay
queues hold at most 20 audio or 512 video packets (each at most 1500 payload
bytes). Variable delays can reorder packets. This is a
media-packet simulator around a real local WebRTC connection, not an OS network
emulator. ICE/DTLS/RTCP and signaling are not impaired. Admin ICE RTT therefore
describes the local transport, not the configured media delay. This does not
test physical microphones, NAT, TURN reachability or an ISP. It does exercise
the real native capture-processing, encoding, jitter, recovery and playback paths.

The bot sends valid mono 48 kHz Opus over DTLS-SRTP through the existing SFU and
receives/decrypts/decodes other speakers and stereo shared audio into a silent
output. Every bot automatically watches all active remote cameras and screen
shares through the normal SFU viewer subscriptions. It appears in the publisher's
viewer list and renews leases every 500 ms. It receives H.264 over DTLS-SRTP,
reassembles and software-decodes complete video frames with OpenH264, then discards
the pixels. Nothing is recorded or sent through HTTP/signaling except counters.
Deafen ends viewing and shared-audio reception; un-deafen resumes current streams.
Mute only stops the bot's microphone.

Video receive work uses a separate bounded worker per bot so it cannot block the
audio codec. Input and decode queues hold at most 1024 and 512 RTP packets.
Per-stream reordering holds at most 256 packets for 40 ms; incomplete, malformed
or stale frames are discarded, with a 250 ms assembly/decode-queue age limit.
The desktop and bot receivers share the same portable 4 MiB frame bound, baseline
SPS dimension/reference-memory checks and bitrate controller. Each decoder is
scoped to a publisher, slot and stream epoch. Changing subscriptions discards
queued work and releases previous decoders, including when a share stops or a
participant leaves. Failed recovery requests a fresh keyframe at most every
500 ms per stream. Loss, stalled decoding and CPU pressure lower viewer bitrate;
sustained healthy reception lets it recover. Initial per-stream budgets divide
the profile's aggregate video allowance (with the protocol's 200 kbit/s minimum).
Shared-audio packets remain under the aggregate audio budget.

Bots exercise the real receiving protocol but are headless: they do not exercise
the native WebView/GPU renderer or physical devices/rooms. Echo is a synthetic
acoustic path through the real echo processor.
They do not publish cameras/screens, send text, manage friendships or have login
accounts. Companions consume server CPU and memory; multiple high-resolution
streams and bots can be expensive. Normal voice permission, mute/deafen, viewer
lease and forwarding fences still apply. The companion compiles the same pinned
OpenH264 0.9.8 source dependency as the desktop client, requiring a C++ toolchain;
NASM enables x86 optimizations and is optional (the crate has a C++ fallback).

`POST /api/v1/voice/bots` uses the shared `voice_bots` commands and normal API
error/request-ID envelope. Each command reauthenticates and checks current
instance ownership and channel access. Settings updates carry the displayed
revision. Bot IDs are scoped to a guild/channel; they cannot be used to control
another room. Voice settings use the existing `/api/v1/admin/audio` relay, with
an additional owner-only restriction for bot connections. Signaling bridges the
SFU internal socket to private length-prefixed stdio (256 KiB frames, 32 queued
messages), with a synthetic identity attached by the owner endpoint; clients cannot request that identity
through public WebSocket commands. No bot password, independent login token or database row exists. The creator
session is passed privately through stdin, never command arguments or log output.

Bots inherit the creator's account/session and guild permission lifetime. Media
also holds the creator's instance-ownership delivery fence, so ownership transfer
drains bot packets before committing. Session revocation, membership changes,
Speak denial and channel deletion use existing media access invalidation. A
failed reauthorization shuts down the peer; no automatic reconnect occurs.
Bots expire after one hour and do not survive backend restart. Settings are
ephemeral. The process admits at most 16 bots and four simultaneous management
requests. Bots count toward the existing eight-participant room limit, so one
human can test with seven bots. Normal public voice connection limits are unchanged.

## Build and run

The headless client is a second native binary in the existing **frontend** package;
there are still exactly three packages and the backend never imports frontend code.
Build/install it beside `thiscord-backend`, or set `THISCORD_VOICE_BOT_PATH` to its
absolute executable path. Backend `run.sh` (also used by `run-wsl.ps1`) builds it
automatically. Linux needs `libasound2-dev` to compile CPAL, although the companion
never opens hardware. No display, Tauri WebView or audio server is required.

```sh
cargo build -p thiscord-frontend --features voice-bot --bin thiscord-voice-bot --locked
cargo build -p thiscord-backend --locked
```

Use `--release` on both commands for a production installation. Keep both binaries
at the same revision; private IPC is not an independently versioned public API.
A missing/crashed companion fails bot admission; there is no simplified fallback.
Companions stop on parent EOF, removal, expiry or revocation; the parent kills a
stuck process after a bounded shutdown wait. Abrupt process termination can leave
the bot's bounded technical-log directory in the server temp folder.

Verification includes owner-only controls, empty-channel admission, encrypted
media, isolation, capacity, revision conflicts, mute/deafen, per-speaker volume,
virtual device swaps, neural DSP settings, real RED and DRED recovery between bots,
PTT release, cameras/screens/shared audio, leases, keyframes, stream replacement,
removal, ownership transfer, Speak revocation and logout. CI builds the companion
before database tests and compiles/tests it on the native platform matrix.

```sh
cargo test -p thiscord-shared --locked voice_bots
cargo test -p thiscord-frontend --lib --features voice-bot --locked bot::
cargo test -p thiscord-frontend --lib --features voice-bot --locked virtual_devices::
THISCORD_VOICE_BOT_PATH="$PWD/target/debug/thiscord-voice-bot" \
  cargo test -p thiscord-backend --test voice --locked owner_bots -- --include-ignored
```

Use your configured `CARGO_TARGET_DIR` in the companion path when it differs from
`target`. Physical listening and native UI acceptance on Windows, macOS and Linux
remain manual checks. PostgreSQL tests use disposable schemas as described in
[database.md](database.md).
