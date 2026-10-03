# Screen sharing (Windows first)

Join a voice channel in the Windows desktop app, then choose **Share screen**
in the persistent voice bar. Select a screen or a visible window and choose
whether to include system audio. **Start sharing** is the only capture trigger.
The other participants see the share in that voice channel. **Stop sharing**
remains available while reading text channels or settings.

System audio captures all other applications, even for a single-window share.
It excludes Thiscord's process tree, so received voice and shared audio are not
sent back into the call. Microphone mute and push-to-talk affect the microphone
only. Stop sharing stops both screen and shared audio. Deafen also stops local
sharing and blocks received media. Each speaker's volume controls their voice
and shared audio together. System audio is currently downmixed to mono.

Windows system audio requires build 20348 or newer, normally Windows 11.
Unsupported or unavailable loopback capture stops the share with an error;
the user can explicitly retry with system audio unchecked. No microphone is
substituted for system audio. Closing/minimizing the selected window or losing
the display stops capture; it never falls back to another window/display.

## Media and dependencies

- Windows capture: [windows-capture 2.0.1](https://docs.rs/windows-capture/2.0.1/windows_capture/)
  wraps Windows Graphics Capture. One persistent capture session delivers compositor
  frame events for the selected monitor/window. There is no screenshot loop. A
  latest-frame mailbox decouples capture from encoding; overload replaces raw
  frames before they become inter-frame dependencies. Capture throttling preserves
  cadence phase, including 59.94 Hz displays, instead of discarding every second
  frame near a 60 fps limit. The system capture border is preserved.
- Video encoding: Windows Media Foundation enumerates **hardware-only H.264 MFTs**
  first, with Baseline profile, NV12 input, low-latency mode and bounded asynchronous
  input/output. The UI shows the selected encoder. Unsupported configurations or
  driver failures fall back explicitly to OpenH264 0.9.8 (bundled Cisco C/C++ via
  Rust bindings); fallback is visible in the sharing bar. This is hardware encoding,
  **not yet a zero-copy GPU pipeline**: capture textures are read back to RGBA,
  resized when necessary and converted on CPU before hardware encoding. Native
  C/C++ tools remain required for the portable software codec; NASM is optional.
- Video offers 720p, 1080p, 1440p and 2160p (4K), each at 15/30/60 fps; default
  1080p/30. Aspect ratio is preserved without upscaling. These are targets, not
  measured throughput guarantees. Bitrate targets range from 1.25 to 32 Mbit/s.
  Access units are bounded to 4 MiB. H.264 Baseline level 5.2 is advertised with a
  90 kHz RTP clock. Keyframes are requested by elapsed time (one second), including
  a refresh of the last captured frame for a static source. Overload no longer
  stretches recovery to several seconds by counting nominal-fps frames. Hardware
  input is capped at four outstanding samples with a 500 ms stall deadline.
- The network remains the existing Rust WebRTC client and single-process SFU;
  the SFU forwards compressed video without transcoding. Receivers assemble RTP
  into a two-frame bounded queue. Loss, overflow or arrivals older than 250 ms
  discard dependencies and wait for SPS/PPS + IDR. SPS dimensions/reference counts
  are validated, including macroblock padding (1080p can be coded as 1088 lines).
- Presentation: compressed H.264 is forwarded to a **local WebRTC peer** bound
  exclusively to `127.0.0.1`, with no STUN/TURN and no audio track. Rust/WASM
  negotiates this peer using control-only Tauri commands, and attaches its
  MediaStream to a real HTML `video` element. There is no native H.264 decode,
  RGB/JPEG conversion, image protocol or frame polling. RTP payload numbers are
  remapped to the WebView's negotiated codec. The WebView owns video decoding,
  timing and GPU composition; actual hardware decode depends on its codec/driver
  support and must be checked on each host. This adds a local encrypted transport
  hop, not another encode. No raw pixels or PCM cross Tauri IPC.
- Viewers send control heartbeats at 250 ms, expire after one second, and close on
  unmount/document hiding. Replacement viewers have distinct leases. Roster/epoch,
  deafen and connection changes revoke forwarding and close the local peer;
  pending work checks its lease and arrival deadline. Negotiations are bounded.
  No images, video or SDP are recorded/logged. A browser preview without Tauri
  cannot join this native presentation bridge.
- System audio is unchanged: [wasapi 0.22](https://docs.rs/wasapi/0.22.0/wasapi/)
  process loopback excludes Thiscord's process tree. Audio capture/Opus encoding
  runs separately from video and microphone work. Shared audio uses 48 kHz mono
  Opus, 20 ms packets and 64 kbit/s. No codec/network work enters CPAL callbacks.
- Eight microphone, eight screen-video and eight shared-audio tracks are negotiated.
  Microphone queues are separate and prioritized. There is no adaptive video
  bitrate, simulcast or demand-based network subscription yet. Multi-share and
  constrained-network capacity remain unmeasured.

Existing voice STUN/TURN, UDP addressing, certificates and per-hop DTLS-SRTP
apply. The SFU forwards encoded video/audio without transcoding. This is transport
encryption, not end-to-end encryption against the server. See [audio.md](audio.md).

## Authorization and lifecycle

ViewChannel, JoinVoice and current membership/session checks remain mandatory.
Speak also grants screen/system-audio publishing in this first version. There
is no separate screen-sharing permission/editor yet. Explicit `screen` signaling
state gates both publisher ingress and receiver egress. Queued packets are
checked against the current publisher identity, share state, receiver deafen
state and access epoch, including slot reuse. Voice uses mutation-only packet
admission/draining: ordinary database authorization checks do not hold the chat
gate or stall media. Access-changing commits invalidate queued media and require
fresh authorization, with conservative voice revocation. Transport writes hold
room locks and packet permits only during individual nonblocking polls. A pending
write releases both and revalidates before resuming; a slow receiver cannot hold
up room updates or subsequent media readers. This relies on the pinned WebRTC
`write_rtp` implementation enqueuing atomically on its final poll.

`shared::voice::MediaKind` owns publisher SSRCs, relay ranges, payload types and
track/mixer slot mappings. Internal SFU queues carry the kind explicitly.

Leave, channel switch, logout/session replacement, connection failure, window
destruction and capture/sender errors cancel the capture lease. A scheduling gap
over two seconds stops capture rather than resuming it after suspend. Rejoining
voice never restarts a share automatically. A stalled OS capture call retains
the single worker slot until it returns; users cannot accumulate capture threads
by repeatedly pressing Start. Captured frames awaiting send are age-limited.

The version-1 voice offer adds `screen_video`; older offers default to false.
Participant snapshots add `sharing_screen` and `sharing_audio`, both defaulting
to false, and a `screen_epoch` identifying the publisher connection. The SFU
stamps that epoch in video CSRC metadata; receivers discard packets from reused
slots whose epoch no longer matches their roster. Clients only send the new screen command to supporting servers.

## Compatibility and acceptance

This delivery prioritizes Windows publishing, as requested. macOS/Linux desktop
builds retain the receiving/video-player path; their publishing controls are disabled.
Their native screen/audio permission and capture implementations remain follow-up
work. The existing Rust WebRTC implementation is retained. No new SFU or WebView
capture library is selected in place of the native transport.

Automated checks cover codec/RTP round trips and loss, bounded malformed input,
wire compatibility, capture cancellation state, and real-peer SFU video/audio
forwarding, isolation, stop/restart, mute/deafen and permission revocation.
CI runs portable codec tests and native compilation on all three hosts.

Manual Windows acceptance still needs two desktop clients: share a monitor and
a window, play another app's audio, verify Thiscord playback is excluded, mute
the microphone, stop/restart, minimize/close the source, leave/rejoin, revoke
Speak/membership, disconnect the network and suspend/resume. Check privacy prompts,
DRM/protected/blank windows, mixed-DPI displays and audio quality on real devices.
Compilation and synthetic transport tests do not establish these runtime results.

Local Windows validation used `_CL_=/Ob1` to work around MSVC 19.42 spending
over 15 minutes optimizing Opus `NSQ_del_dec.c` at `/Ob2`. Repository/CI build
settings were not changed. Native tests also emitted the codec's LNK4255 debug-symbol
warning for duplicate `dct.o` object names; linking and tests succeeded.

## Pipeline diagnostics and validation

`cargo run -p thiscord-frontend --example screen_encode_probe --features screen-share --profile ci --locked`
uses synthetic pixels only and requires Windows hardware H.264 support. It verifies
120 encoded frames at both 720p/60 and 1080p/60 by decoding the emitted bitstreams.
On the development RTX 4070 Ti, it selected `NVIDIA H.264 Encoder MFT` and all 240
frames decoded. This is an encoder/bitstream check, not a capture-to-display fps
measurement. Intel/AMD hardware selection and driver-failure fallback still need
physical-device acceptance.

`screen_preview_probe` (same Cargo feature/profile) serves a synthetic video
interop harness on `http://127.0.0.1:18741`: a video element and a local `/offer`
endpoint for an automated browser's recvonly WebRTC offer. It runs one video
session for 20 seconds and never captures the screen or accesses an account.
It is a developer example, not an application HTTP endpoint. In the browser,
inspect inbound WebRTC `framesDecoded`, `framesDropped`, `framesPerSecond`,
`decoderImplementation` and `powerEfficientDecoder` when supported.

Portable codec tests also negotiate a video-only local peer using a different
payload number and lower advertised H.264 level, then check that the received
access unit is byte-identical. Native tests cover viewer replacement/revocation
and stale capture completion; cadence tests cover source jitter and overload.
CI keeps those tests on the Windows/macOS/Linux matrix. Native WebView playback
still requires acceptance on all three platforms; Linux needs a WebKitGTK build
with working WebRTC/H.264 support. Browser automation was unavailable in the local
implementation session, so no real-WebView playback/fps result is claimed.
