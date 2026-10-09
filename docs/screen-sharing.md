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
sharing and blocks received media. Each received screen share has its own 0-200%
volume and mute/unmute controls beside the video, independent of the sharer's voice
volume. Both levels persist locally per guild/account. Existing voice preferences
are preserved; shared audio defaults to 100%. System audio is currently downmixed
to mono.

**Full screen** expands the received video within the app window while keeping its
audio controls available. **Exit full screen** or Escape restores the inline view.
The existing player continues running through the layout change; this does not
enter operating-system fullscreen.

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
- Pixel processing and encoding: capture requests **FP16 linear scRGB** surfaces.
  Three owned capture textures decouple WGC callbacks from the encoder. A D3D11
  shader scales the selected source and normalizes by its monitor's SDR reference
  white (Windows HDR brightness setting; scRGB 1.0 = 80 nits). An HDR luminance
  shoulder compresses highlights; BT.709 transfer and a video processor produce
  limited-range BT.709 NV12. The encoder consumes these D3D11 textures through
  an MF DXGI device manager. No raw-pixel readback, CPU resizing/color conversion,
  CPU upload, or software encoding occurs in the production sharing path.
  Capture, processing and hardware MFT selection use the same adapter/device;
  the shared immediate context is multithread-protected. Six NV12 surfaces remain
  owned until encoder output retires the corresponding sample. GPU pool pressure
  replaces/skips raw frames before H.264 reference dependencies are created.
- HDR output is deliberately **SDR H.264**, suitable for SDR viewers; this is not
  HDR10 passthrough. Monitor white level is refreshed once per second, using the
  dominant monitor for a window. SDR mids are preserved; HDR highlights above
  75% normalized reference white use a smooth shoulder. Unsupported GPU surface
  encoding or color-processing failures stop sharing with an explicit error.
  The Windows path requires D3D11 FP16 shader/video-processor support,
  D3DCompiler_47 (Windows system component), and an MF D3D11-aware hardware MFT.
  OpenH264 remains a portable test decoder/diagnostic encoder, not a hidden CPU
  fallback. AMD/Intel and mixed-display visual acceptance remain required.
- Video offers 720p, 1080p, 1440p and 2160p (4K), each at 15/30/60 fps; default
  1080p/30. Aspect ratio is preserved without upscaling. These are targets, not
  measured throughput guarantees. Bitrate targets range from 1.25 to 32 Mbit/s.
  Access units are bounded to 4 MiB. H.264 Baseline level 5.2 is advertised with a
  90 kHz RTP clock. Keyframes are requested by elapsed time (one second), including
  a refresh of the last captured frame for a static source. Overload no longer
  stretches recovery to several seconds by counting nominal-fps frames. Hardware
  input is capped at four outstanding samples with a 500 ms stall deadline.
  Prompt recovery requests supplement periodic IDRs. CBR and mean-bitrate settings
  must be accepted by the hardware encoder. The optional H.264 HRD buffer target
  is 100 ms of codec output, in **bytes** (`bitrate / 8 / 10`), leaving headroom
  below the sender deadline. The previous `bitrate / 4` supplied bits to a byte
  property, allowing two seconds rather than the intended quarter-second.
  Diagnostics expose buffer acceptance and readback; acceptance is not a hard
  per-frame size guarantee. Smaller buffers can trade scene-change detail for latency.
- The network remains the Rust WebRTC client and single-process SFU; the SFU
  forwards compressed video without transcoding. The publisher byte-paces packets
  at 1.2 times target codec bitrate (transport overhead allowance), with at most
  8 ms of burst credit and batched wakeups to accommodate OS timer granularity. It does not accumulate
  catch-up credit during stalls. Stale/partially sent frames trigger IDR recovery
  instead of terminating the share. Encoded queues hold 200 ms of target-rate
  frames (six at 30 FPS, twelve at 60 FPS); frames older than 200 ms are rejected
  before sending. The in-flight frame retains a 350 ms capture-age deadline, and
  each RTP write has a 100 ms timeout, so this is not an end-to-end latency bound.
  Admission reserves queue space for pending hardware output and also pauses raw
  input when unsent wire bytes exceed 50 ms at the pacing rate. This count includes
  the in-flight frame and decreases after successful packet writes; failed enqueue,
  recovery drops and cancellation release the remainder. Large frames therefore
  suppress new encoding before filling the queue with H.264 dependencies, even
  when a driver rejects the smaller HRD buffer. The latest raw capture is retained
  for resumption (subject to its existing 250 ms age limit) and replaced by fresher
  captures. These admission limits do not bound unfinished hardware output or
  shorten an individual oversized frame. Discarding dependent frames during recovery does not
  request another keyframe for each skipped delta.
  SFU video deliveries expire after 150 ms in its egress queue, with aggregate
  queue-drop/send-timeout diagnostics and rate-limited recovery feedback.
- Receivers reorder up to 256 packets for 40 ms, handle sequence wraparound and
  reject duplicates/late packets. A 10 ms timer resolves gaps even if no more
  packets arrive. Complete-frame queues allow eight frames, at most 8 MiB and
  250 ms age. Short backlogs drain in order; overflow can retain a newer queued
  keyframe and its suffix. Unrecoverable dependency loss requests a fresh IDR.
  Feedback is coalesced to one request per stream per 500 ms and per publisher
  per 250 ms at the SFU, with a 200 ms minimum forced-IDR interval at the encoder. Viewer decode PLI counters also feed the same recovery
  control. Publisher responses are consumed before the next available GPU input.
  SPS dimensions/reference counts remain bounded.
- Presentation: compressed H.264 is forwarded to a **local WebRTC peer** bound
  exclusively to `127.0.0.1`, with no STUN/TURN and no audio track. Rust/WASM
  negotiates this peer using control-only Tauri commands, and attaches its
  MediaStream to a real HTML `video` element. There is no native H.264 decode,
  RGB/JPEG conversion, image protocol or frame polling. RTP payload numbers are
  remapped to the WebView's negotiated codec. The WebView owns video decoding,
  timing and GPU composition; actual hardware decode depends on its codec/driver
  support and must be checked on each host. This adds a local encrypted transport
  hop, not another encode. No raw pixels or PCM cross Tauri IPC.
- Viewers send control heartbeats at 250 ms, expire after three seconds (tolerating short UI stalls), and close on
  unmount/document hiding. Replacement viewers have distinct leases. Roster/epoch,
  deafen and connection changes revoke forwarding and close the local peer;
  pending work checks its lease and arrival deadline. Negotiations are bounded.
  No images, video or SDP are recorded/logged. A browser preview without Tauri
  cannot join this native presentation bridge.
- System audio is unchanged: [wasapi 0.22](https://docs.rs/wasapi/0.22.0/wasapi/)
  process loopback excludes Thiscord's process tree. Audio capture/Opus encoding
  runs separately from video and microphone work. Shared audio uses 48 kHz mono
  Opus, 20 ms packets and 64 kbit/s. Capture drains available WASAPI packets before
  waiting for another event, including coalesced notifications after worker delays,
  and zeros packets marked silent. No codec/network work enters CPAL callbacks.
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

The version-1 voice offer adds `screen_video` and `screen_feedback`; older offers
default both to false. A new client opts into feedback/diagnostic events in its
answer only when the server advertises support. Older clients never receive new
event variants; newer clients use periodic recovery with older backends. Deploy
the updated backend to enable prompt feedback and SFU counters. Feedback checks
current membership/access generation, deafen, publisher Speak/sharing state and
stream epoch, and coalesces requests across viewers.
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

Expand **Screen pipeline diagnostics** by the sharing controls or **Playback
diagnostics** under a received stream. **Copy diagnostics** copies aggregate
values only. Native counters include capture events/throttling, raw replacement,
encoded/sent/received frames and bytes, reordered/late/lost packets, queue peaks,
expired frames, recovery requests and local bridge errors. Timing aggregates
contain sample count, mean and maximum for capture/GPU command submission,
capture-to-encoded-output, sender age, assembly and preview submission. GPU submit
time measures CPU command submission, not a GPU timestamp query. A bounded
64-event history records recovery reasons with times since stream start.

Native WebRTC reports add network jitter/loss and nominated-pair RTT/bandwidth
estimates. These are library-reported values: unsupported estimates may be zero.
SFU counters distinguish ingress and egress queue drops from transport timeouts;
they update every five seconds. They are cumulative, not per-sample rates.
Browser statistics show decoded FPS, decoder drops, freezes, local-hop loss/jitter,
decode/jitter-buffer time and decoder identity/efficiency when supported. Presented
FPS is measured separately from video presentation counters, since decoding 60 FPS
need not mean displaying 60 FPS. Missing browser fields mean unavailable, not zero.
No SDP, candidate addresses, credentials, frame contents or recordings are included.

Publisher `audio_capture_packets`, `audio_capture_silent_packets`,
`audio_capture_discontinuities`, `audio_capture_queue_resets`, `audio_sent_frames`
and `audio_send_failures` cover shared-audio capture and sending. Discontinuities
are WASAPI-reported flags (an initial flag can occur at stream start); a growing
count during continuous playback warrants investigation. These counters do not
measure receiver packet loss or output underruns. For crackle heard by a viewer,
also collect that client's Audio & voice diagnostic logs; `audio_snapshot` includes
aggregate output underruns. Silence/inactive streams can contribute to that
aggregate, so correlate changes with audible glitches. No audio is recorded.

Read diagnostics in pipeline order: raw replacements/GPU busy and encode latency
identify publisher pressure; sender drops/age identify pacing backlog; SFU drops
and native receive gaps identify transport/relay pressure; preview errors identify
the local bridge; browser decode/presentation differences identify WebView pressure.
Compare a moving source (static sources naturally produce fewer capture events).
Rates derive from counter deltas, not the selected quality. Times use local clocks;
there is no synchronized one-way capture-to-display latency measurement.

Publisher diagnostics now include `sender_queue_wait` (enqueue to dequeue),
`pacing_wait` (actual sleep), `pacing_lateness` (wake delay beyond the requested
deadline), `rtp_write` (each WebRTC write, including failed/timed-out attempts),
and `frame_send` (packet pacing plus writes for completed frames). All report
sample count, mean and maximum. `pacing_sleeps` counts batch wakeups;
`encoded_peak_frame_bytes` and `sender_peak_frame_packets` expose large bursts.
`sender_peak_bytes` includes queued and in-flight unsent packet bytes, using the
same overhead estimate as pacing. `sender_backlog_budget_ms` is the raw-input
admission threshold, not a hard encoded-queue byte limit. `encode_backpressure`
counts fresh raw captures deferred before encoding; newer raw captures replace
deferred ones. `encoder_buffer_target_bytes`, `encoder_buffer_applied` and
`encoder_buffer_readback_bytes` distinguish the requested HRD size, setter acceptance
and driver-reported value (`unavailable` when readback is unsupported). Encoded losses
are separated into `encode_queue_full`, `encode_stale`, `encode_dependent`,
`sender_stale`, `sender_dependent`, `send_timeouts` and `send_errors`, while
`encode_dropped` remains their aggregate. Large wake delays implicate scheduling;
large write times implicate the WebRTC write path. Low times with large frames
and growing queue wait implicate offered load versus the configured pacing rate.

`cargo run -p thiscord-frontend --example screen_gpu_probe --features screen-share --profile ci --locked`
uses synthetic FP16 textures, GPU scale/tone-map/NV12 conversion and GPU H.264,
then decodes the output to check expected SDR levels. It also reads monitor HDR
metadata without capturing pixels. Local NVIDIA validation passed 360 frames:
720p SDR, 1440p-to-1080p HDR normalization and 4K-to-720p HDR highlights. The attached
HDR monitor reported 240-nit SDR white. Intel/AMD acceptance remains outstanding.

Add `-- --burst` to this probe command to encode/decode 180 frames of detailed
synthetic scenes at 2560x1070, 18 Mbit/s and a 60 FPS input target, with repeated
keyframes. This exercises rate control without desktop capture or network traffic.
Local NVIDIA validation accepted/read back the 225,000-byte HRD target, decoded
all 180 frames and produced three IDRs around 170 KB (170,123-byte peak). This is
synthetic evidence, not a before/after measurement of a user's shared content.
The deterministic pacing regression replays 524 KB frame bursts and coarse timer
wakeups: slot-only admission exceeds the sender age limit; byte admission avoids
those queued-frame expirations at the same pacing rate by deferring raw input.
Live two-client playback and scene-change quality still require acceptance.

`screen_encode_probe` tests the legacy system-memory input encoder API with
synthetic data; that API is not the production capture path. `screen_preview_probe`
serves a synthetic local WebRTC video session at `http://127.0.0.1:18741` for 20
seconds. No desktop pixels, accounts, microphones, recording, STUN or TURN are used.
A local browser run decoded 60 FPS with zero reported decoder freezes/loss; the
hidden preview dropped presentation frames. This does not establish two-client
capture-to-display performance or hardware decoder selection in the actual app.

Tests cover sequence wrap, duplicates, bounded reordering deadlines, recoverable
and overflowing backlogs, feedback throttling/permissions and compatible wire
negotiation, plus existing codec/lease/permission tests. CI compiles diagnostics
and runs portable tests on Windows/macOS/Linux. Linux needs a WebKitGTK build with
working WebRTC/H.264. Live capture, HDR appearance, Intel/AMD drivers, multi-client
network conditions and actual WebView hardware decode still require acceptance.

HDR/device references: [Microsoft Advanced Color](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range),
[MF D3D device manager](https://learn.microsoft.com/en-us/windows/win32/medfound/mft-message-set-d3d-manager),
[H.264 HRD buffer units](https://learn.microsoft.com/en-us/windows/win32/codecapi/avenccommonbuffersize-property),
[WebRTC statistics](https://www.w3.org/TR/webrtc-stats/).
