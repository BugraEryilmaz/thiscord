# Owner voice test bots

Sign in as the **instance owner**, right-click a voice channel and choose
**Voice test bots…**. Choose a connection profile and microphone loop, then
**Add bot to this channel**. Repeat to add independent participants. The owner
must also belong to the guild and have ViewChannel, JoinVoice and Speak there.
Instance admins and guild owners alone cannot use these controls.

Join the channel with the desktop client to listen. Bots appear in the ordinary
voice roster, overlay, notifications and per-speaker volume controls. Right-click
a bot for its test audio settings, or click its name in the browser `/admin`
participant table as the instance owner. Save changes to microphone gain, output
volume, mute/deafen, loop or connection profile without reconnecting. Output
volume affects the bot's silent decoded output and its numeric peak meter; the
ordinary participant volume slider controls what **you** hear. Refresh retrieves
current settings and counters, including received shared audio and per-camera/screen
resolution, decoded frames, discarded/missing packets, decode errors, keyframe
requests and requested bitrate. Video counters reset when the viewed stream set
or connection profile changes. Concurrent edits return a conflict; refresh before
retrying. Remove stops the peer and releases its channel slot.

The three original synthesized loops are chimes, a low pulse and a melody, each
three seconds long with a quiet gap. They contain no recordings or downloaded
audio. The add form cycles through loop choices after each addition. Multiple
bots may use the same loop; each has a separate identity, media clock and codec.

Connection presets apply independently to the microphone uplink and the aggregate
incoming audio downlink. A separate aggregate video downlink uses the same
delay/jitter/loss, with these budgets:

- Good: 96 kbit/s media-link budget, 20 ms one-way delay, no injected loss; 32 Mbit/s video.
- Low bandwidth: 24 kbit/s, 120–150 ms delay, 16 kbit/s Opus encoding; 500 kbit/s video.
- High packet loss: 96 kbit/s, 60–80 ms delay, 25% deterministic packet loss; 32 Mbit/s video.
- Unstable: 48 kbit/s, 100–200 ms delay, 10% deterministic packet loss; 2 Mbit/s video.

Other presets encode at 32 kbit/s. Budgets charge payload plus 60 bytes per packet
and permit at most 100 ms of burst credit; bandwidth exhaustion adds loss. Delay
queues hold at most 20 audio or 512 video packets (each at most 1500 payload
bytes). Variable delays can reorder packets. This is a
media-packet simulator around a real local WebRTC connection, not an OS network
emulator. ICE/DTLS/RTCP and signaling are not impaired. Admin ICE RTT therefore
describes the local transport, not the configured media delay. This does not
test physical microphones, NAT, TURN reachability, an ISP or native desktop DSP.

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
the native WebView/GPU renderer, physical devices or acoustic echo processing.
They do not publish cameras/screens, send text, manage friendships or have login
accounts. Video decoding consumes backend CPU and memory; multiple high-resolution
streams and bots can be expensive. Normal voice permission, mute/deafen, viewer
lease and forwarding fences still apply. The backend now compiles the same pinned
OpenH264 0.9.8 source dependency as the desktop client, requiring a C++ toolchain;
NASM enables x86 optimizations and is optional (the crate has a C++ fallback).

`POST /api/v1/voice/bots` uses the shared `voice_bots` commands and normal API
error/request-ID envelope. Each command reauthenticates and checks current
instance ownership and channel access. Settings updates carry the displayed
revision. Bot IDs are scoped to a guild/channel; they cannot be used to control
another room. Signaling uses bounded in-process queues, with a private synthetic
identity attached by the owner endpoint; clients cannot request that identity
through public WebSocket commands. No bot password, token or database row exists.

Bots inherit the creator's account/session and guild permission lifetime. Media
also holds the creator's instance-ownership delivery fence, so ownership transfer
drains bot packets before committing. Session revocation, membership changes,
Speak denial and channel deletion use existing media access invalidation. A
failed reauthorization shuts down the peer; no automatic reconnect occurs.
Bots expire after one hour and do not survive backend restart. Settings are
ephemeral. The process admits at most 16 bots and four simultaneous management
requests. Bots count toward the existing eight-participant room limit, so one
human can test with seven bots. Normal public voice connection limits are unchanged.

Verification covers decodable distinct loops, bounded deterministic impairment,
owner/admin/member authorization, empty-channel admission, real encrypted delivery,
downlink decoding, isolation, capacity, stale edits, invalid levels, mute/unmute,
removal, ownership transfer, Speak revocation and logout. Video tests cover real
camera/screen/shared-audio delivery to simultaneous good/slow/lossy bots, viewer
lease renewal, bitrate reduction, keyframe feedback, deafen, stream restart,
logout, stale owner/epoch rejection, reorder/wrap/loss recovery, malformed SPS
and bounded queues. Run:

```sh
cargo test -p thiscord-shared --locked voice_bots
cargo test -p thiscord-backend --lib --locked voice::bots
cargo test -p thiscord-backend --test voice --locked owner_bots -- --include-ignored
```

Physical listening and native UI acceptance on Windows, macOS and Linux remain
manual checks. PostgreSQL tests use a disposable schema as described in
[database.md](database.md).
