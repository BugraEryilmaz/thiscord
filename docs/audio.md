# Native audio and voice

Voice channels use native Rust capture/playback and a Rust SFU in the backend.
Only settings, levels and participant metadata cross Tauri IPC. PCM and encoded
audio never pass through WASM or handwritten JavaScript. Browser preview supports
text chat; voice requires the desktop executable. Exactly three packages remain.

## Use

Restart the backend and desktop app after building. In Settings → Audio & voice,
select devices, adjust output level and choose voice activation or push-to-talk.
Device lists refresh every three seconds. Open a voice channel and click Join
voice. Select another voice channel and join to move yourself. The voice bar stays
available while reading text channels. Disconnect releases the devices.

Each speaker has an independent 0–200% volume slider. These gains last for that
voice connection; device IDs, master gain, activation threshold, mute/deafen,
transmit mode and processing choices persist in the OS app config directory's
`audio.json`. No tokens or recorded audio go there. Stop audio before changing
devices. A vanished device stops capture/playback and displays an error; it does
not silently switch to a different microphone. Select devices and join again.

Tests in settings include five seconds of two independently adjustable tones,
a microphone/Opus loopback limited to 60 seconds, and a synthetic encrypted WebRTC
forwarding probe. Tests do not save recordings. Use headphones for microphone
loopback. Microphone access is requested by opening the native input stream;
permission/device errors point to OS privacy settings. The macOS bundle includes
`NSMicrophoneUsageDescription`. Packaged/signed macOS permission behavior still
requires acceptance on a Mac.

The in-app Hold to talk button handles pointer cancellation, key release and
focus loss. Optional global Ctrl+Shift+Space uses the Tauri 2 Rust shortcut plugin
on Windows/macOS/X11. Registration conflicts are reported. Wayland explicitly
falls back to the in-app button; portal integration remains future work.

## Libraries and compatibility decision

- [CPAL 0.18.2](https://github.com/RustAudio/cpal): native WASAPI/CoreAudio/ALSA.
  Initial devices must support 48 kHz mono or stereo, f32/i16/u16. This includes
  common desktop devices but not every Bluetooth hands-free configuration.
  Arbitrary-rate resampling and Bluetooth mode switching remain pending.
- [ringbuf 0.5.2](https://docs.rs/ringbuf/0.5.2/ringbuf/): bounded SPSC queues.
- [WebRTC-rs 0.21](https://github.com/webrtc-rs/webrtc): native ICE/DTLS/SRTP,
  RTP tracks and RTCP interceptors, with Tokio and Ring crypto. Its current
  0.x API is pinned by Cargo.lock and covered by executable loopback tests.
- [opus 0.4](https://docs.rs/opus/0.4.0/opus/): Rust interface to bundled libopus
  through opusic-sys. The application remains Rust; this codec uses native C
  and requires CMake/a C compiler. The SFU does not link Opus or decode audio.
- [Sonora 0.2](https://github.com/dignifiedquire/sonora): pure Rust AEC3, noise
  suppression and AGC2. All are optional and initially off. Ten-millisecond
  processing runs on the audio worker, with post-mix playback as the echo
  reference. Initial delay is 40 ms; AEC3 estimates/refines delay internally.
  Hardware acoustic quality is unverified, so headphones remain recommended.

The native path is selected per the project requirement. Windows and Linux
software probes run locally; CI runs the same tests and desktop builds on all
three OSes. macOS runtime, microphone consent, headset changes and acoustic
acceptance remain open. Compilation alone does not establish those results.
WebView capture was not substituted for the requested native path.

## Audio timing and performance

Capture is averaged to mono, processed at 48 kHz and encoded as 20 ms Opus frames
at 32 kbit/s. Capture callbacks only convert/copy samples and update atomics.
Each received speaker owns a separate bounded packet jitter buffer, Opus decoder,
PCM ring and gain. Jitter starts at roughly 60 ms, reorders sequence numbers,
rejects duplicates/late packets, handles wrap, and requests bounded Opus packet
loss concealment. A source switch resets decoding and its queued PCM.

Playback mixes on the device callback with atomic gain changes. It allocates no
buffers, waits on no locks, and does no networking/codec/DSP work. Queues cap
latency rather than accumulating stale speech; underruns produce silence and
overflows are counted. Output clips to [-1,1]. The mixer supports 32 preallocated
queues; the initial room limit is eight participants. This is a bounded initial
deployment, not a measured promise of production capacity.

Run `cargo run -p thiscord-frontend --example audio_bench --features native-audio
--release --locked` for a synthetic eight-stream mixer timing. This excludes
device latency, DSP, codecs and network costs. The local WSL release run measured
0.021 ms per 20 ms block over 1,000 blocks; this is a synthetic local result,
not an end-to-end latency or SFU capacity measurement. Stop and push-to-talk
release signals use atomics so a saturated media queue cannot lose them.
At about 60 kbit/s per encrypted
audio stream including packet overhead, eight active speakers imply roughly
3.4 Mbit/s SFU egress (8 × 7 streams), plus signaling/RTCP.

## Signaling, SFU and authorization

`/api/v1/voice` is a version-1 authenticated WebSocket. A Join frame carries the
token, guild and channel; tokens never enter URLs. The server checks session,
membership, voice channel type, ViewChannel and JoinVoice in a transaction.
Speak is separately enforced on every forwarded publisher stream. A listener
without Speak permission opens only an output device, leaving the microphone
closed. Native socket
Origin is `http://tauri.localhost`, which must remain in ALLOWED_ORIGINS.

There is one bundled WebRTC connection per participant and eight pre-negotiated
speaker tracks. SDP includes gathered ICE candidates; there is no trickle-ICE
command. The server sends Offer plus the participant's slot and ICE configuration,
the client replies Answer once, and participant snapshots follow. Slots are
mapped to fixed SSRCs by the shared contract. Source changes preserve outgoing
SRTP sequence/timestamp continuity. The server forwards encoded RTP, never mixes
or transcodes it. Default RTCP interceptors handle per-hop reports/feedback;
adaptive bitrate and production congestion/load tuning remain pending.

Room membership is in memory. There are eight users per room, 64 active/pending
voice sockets per process, six joins per account/minute, a five-second initial
authentication deadline and bounded negotiation. Signaling is capped at 128 KiB
and 30 incoming frames/second. Publisher ingress is capped at 100 packets/second
and 1500 bytes/packet. Media queues and write deadlines isolate slow receivers.

Current sessions are rechecked every second. Successful access-changing commits
invalidate all voice connections conservatively. Both publisher routing and
receiver writes check the invalidation generation under the access gate, so
cached grants cannot continue forwarding after revocation. Each receiver gets
only its channel's streams; own audio is not looped back. Self mute/deafen is also
enforced by the SFU. This assumes a single backend process.

Disconnect, logout, errors, deleted channels and expiry close the peer and audio
devices. Missing UI heartbeats or suspend gaps stop the audio worker within three
seconds. Rejoining is explicit after access/network/device changes; automatic ICE
restart/reconnect is not yet implemented. Server shutdown closes sockets/UDP with
the process; coordinated drain and production metrics remain pending.

This is transport encryption between client and SFU, **not end-to-end encryption**:
the SFU terminates DTLS-SRTP and can access encoded Opus. Do not log SDP, tokens,
TURN credentials or packet contents.

## WSL and public networking

HTTP localhost forwarding alone does not forward WebRTC UDP. The backend chooses
its outbound IPv4 address by default; `THISCORD_VOICE_BIND=IP:0` can select a WSL
interface. `:0` allocates one UDP port per peer. Windows must be able to reach the
advertised WSL address, and both host firewall directions must permit the media.
Do not configure a single fixed UDP port for multiple connections.

Docker coturn deployment and public port requirements are documented in
[turn.md](turn.md). Configure THISCORD_STUN_URL, THISCORD_TURN_URL and
THISCORD_TURN_SECRET in backend/.env and restart the backend. No third-party
service is assumed or silently contacted. The backend issues account-bound, one-hour
[TURN REST credentials](https://github.com/coturn/coturn/blob/master/README.turnserver)
using HMAC-SHA1; only the temporary credential goes to clients. The static secret
stays on the backend. Long calls need rejoining before relay credentials expire
until refresh/ICE restart is implemented. Router/firewall forwarding and off-site,
restrictive-NAT acceptance still require verification for each deployment.

## Native voice connection diagnostics

Text chat uses the WebView's network stack; voice signaling uses a native Rust
WebSocket. Working text chat alone therefore does not establish native voice
connectivity. The native connector reports DNS, TCP and TLS/WebSocket failures
separately, includes the configured signaling endpoint, and tries alternate
IPv4/IPv6 addresses with staggered concurrent connections. TLS certificate and
hostname verification remain enabled. Tokens are sent only after the upgrade;
errors never include response bodies, tokens or SDP.

On macOS, the bundle declares a local-network usage description. Check System
Settings > Privacy & Security > Local Network when native networking is blocked.
An enabled permission does not rule out DNS, routing, VPN or connection failures.
See [Apple's local-network privacy guidance](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).

New builds support a native-only check from Terminal, without opening a microphone
or using an account:

```sh
/Applications/Thiscord.app/Contents/MacOS/thiscord-desktop --check-voice-connection
```

Older installed releases do not have this command or the detailed errors. The
intermittent macOS signaling timeout reported on 2026-09-29 recovered before these
changes were installed; its cause and macOS runtime behavior remain unverified.

## Verification and remaining work

Native library tests cover independent mixing/gain, deafen backlog, queue bounds,
jitter ordering/replay/wrap, finite DSP output and Opus forwarding across two
encrypted WebRTC hops. Backend PostgreSQL tests use disposable test schemas and
real peer connections to exercise forwarding, isolation, self-mute/deafen,
Speak denial, permission revocation, publisher slot reuse, channel deletion and
cleanup. Windows device enumeration is read-only and opens no mic.

Still required: physical multi-user microphone/headset acceptance on each OS,
macOS runtime results, Wayland global-shortcut portal support, arbitrary sample
rates, long-call recovery, moderator voice controls, detailed speaking/quality
indicators, load/packet-loss/NAT trials and public deployment.
