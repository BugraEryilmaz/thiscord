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
current settings and counters. Concurrent edits return a conflict; refresh before
retrying. Remove stops the peer and releases its channel slot.

The three original synthesized loops are chimes, a low pulse and a melody, each
three seconds long with a quiet gap. They contain no recordings or downloaded
audio. The add form cycles through loop choices after each addition. Multiple
bots may use the same loop; each has a separate identity, media clock and codec.

Connection presets apply independently to the microphone uplink and the aggregate
incoming audio downlink:

- Good: 96 kbit/s media-link budget, 20 ms one-way delay, no injected loss.
- Low bandwidth: 24 kbit/s, 120–150 ms delay, 16 kbit/s Opus encoding.
- High packet loss: 96 kbit/s, 60–80 ms delay, 25% deterministic packet loss.
- Unstable: 48 kbit/s, 100–200 ms delay, 10% deterministic packet loss.

Other presets encode at 32 kbit/s. Budgets charge payload plus 60 bytes per packet
and permit at most 100 ms of burst credit; bandwidth exhaustion adds loss. Delay
queues hold at most 20 packets. Variable delays can reorder packets. This is a
media-packet simulator around a real local WebRTC connection, not an OS network
emulator. ICE/DTLS/RTCP and signaling are not impaired. Admin ICE RTT therefore
describes the local transport, not the configured media delay. This does not
test physical microphones, NAT, TURN reachability, an ISP or native desktop DSP.

The bot sends valid mono 48 kHz Opus over DTLS-SRTP through the existing SFU and
receives/decrypts/decodes other speakers into a silent output. Codec work runs on
a bounded per-bot blocking worker; PCM never enters HTTP or signaling. Bots have
no hardware devices, acoustic echo processing, camera, screen publisher, text
chat, friendship or login account. Only implemented audio controls are shown.
They use the ordinary voice permission, mute/deafen and forwarding fences.

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
removal, ownership transfer, Speak revocation and logout. Run:

```sh
cargo test -p thiscord-shared --locked voice_bots
cargo test -p thiscord-backend --lib --locked voice::bots
cargo test -p thiscord-backend --test voice --locked owner_bots -- --include-ignored
```

Physical listening and native UI acceptance on Windows, macOS and Linux remain
manual checks. PostgreSQL tests use a disposable schema as described in
[database.md](database.md).
