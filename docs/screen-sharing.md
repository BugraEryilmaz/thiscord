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

- Windows capture: [XCap 0.9.8](https://docs.rs/xcap/0.9.8/xcap/), using its
  screen/window capture APIs on a dedicated worker. No screenshots are saved.
- Video: [OpenH264 0.9.8 Rust bindings](https://docs.rs/openh264/0.9.8/openh264/),
  building the bundled Cisco C/C++ codec from source. Native C/C++ tools are
  required; NASM is optional. This is not a pure-Rust codec or Cisco's separately
  distributed binary. See the bundled third-party license notice.
- Windows audio: [wasapi 0.22](https://docs.rs/wasapi/0.22.0/wasapi/), process
  loopback with `include_tree=false`, excluding the current process tree.
  Capture/Opus encoding runs separately from video and the microphone worker.
  No allocations, codecs or network operations are added to CPAL callbacks.
- Video offers 720p, 1080p, 1440p and 2160p (4K), each at 15/30/60 fps;
  the default is 1080p/30. These are capture targets, not measured throughput
  guarantees. Aspect ratio is preserved without upscaling. Encoder targets range
  from 1.25 to 32 Mbit/s, with 4 MiB encoded access-unit bounds. H.264 baseline
  [level 5.2](https://github.com/cisco/openh264#encoder-features) is advertised,
  using a 90 kHz RTP clock and a keyframe every second
  of captured frames; malformed/incomplete frames are discarded. Incoming SPS
  dimensions/reference counts are checked before native decoding, allowing
  macroblock padding (e.g. 1080p coded as 1088 lines).
  Shared audio uses 48 kHz mono Opus, 20 ms packets and 64 kbit/s.
- A voice peer negotiates eight microphone, eight screen-video and eight shared
  audio tracks. Microphone queues are separate and prioritized over screen
  traffic. Screen ingress/egress queues, encoder batches and decoder queues are
  bounded. This is an initial selectable-rate implementation, without adaptive video
  bitrate, simulcast or demand-based subscriptions. Sustained multi-share and
  constrained-network capacity remain unmeasured.
- Decoded frames stay in native memory. The Leptos viewer refreshes an uncached
  JPEG through a narrowly scoped Tauri `screen:` protocol; no video frames or
  PCM cross control IPC. There is one latest frame per source, expiring after
  two seconds. The viewer requests at up to 60 Hz with one outstanding request
  per share. Software capture, encoding, decoding and JPEG presentation can limit
  achieved frame rate, especially at 4K/60; native GPU presentation/hardware
  encoding and live high-resolution performance testing remain future work.
  Stop/roster changes clear frames. Nothing is recorded to disk.

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
fresh authorization, with conservative voice revocation.

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
builds retain the receiving/decoding path; their publishing controls are disabled.
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
