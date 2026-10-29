# Screen sharing (Windows first)

Join a voice channel in the Windows desktop app, then choose **Share screen**
in the persistent voice bar. The source picker separates Screens and Windows;
select a source, resolution/frame rate and whether to include system audio, then
choose **Go live**. Selecting a source alone never starts capture.
Voice rooms and private calls show a grid of all active webcams and screen shares.
Click a participant's name or LIVE badge to open the room's grid; this joins that
voice channel if needed, using the current microphone settings. A participant
may publish a camera and a screen simultaneously. Your camera and screen have
local previews; the screen preview uses the same encoded frames without an SFU
round trip. Self-preview alone does not upload screen video.

Click any tile to enter fullscreen; Escape or its fullscreen button restores the
grid. WebViews without DOM fullscreen use an expanded app view. Players remain
mounted while expanding. Right-click a tile for **Hide this item**; **Show hidden
items** restores it. Hiding a received screen unsubscribes its video and shared
audio. Hidden state lasts while this grid is mounted and is scoped to the stream
generation. Hiding your own tile does not stop publishing. All other tiles
continue playing. Keyboard users can focus a tile and press Enter to expand.

System audio captures all other applications, even for a single-window share.
It excludes Thiscord's process tree. Microphone mute/PTT affects only the
microphone. Stop sharing stops screen and shared audio. Deafen also stops local
screen/camera publication and received media. Use a participant's right-click
menu (sidebar or grid) for separate voice and **Stream audio** volume/mute.
Both levels persist locally per guild/account; shared audio defaults to 100%
and is currently downmixed to mono. **Stop sharing** remains in the voice bar
while reading text or settings.

See [cameras](cameras.md) for camera permissions, limits and testing without hardware.

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
  Encoder cadence advances only on actual input submission; empty polls, missing
  hardware credit and sender backpressure do not consume a frame slot. This avoids
  holding a newly arrived capture for a full adaptive frame period (200 ms at 5 FPS).
  Fresh captures are not throttled a second time on the worker clock; only repeat
  submissions of the held image have a separate minimum interval.
  RTP timestamps and capture admission use WGC's compositor/QPC timestamp rather
  than callback arrival time. An asynchronous MFT event wakes the encoder worker;
  it does not wait for the next capture or a coarse timer to collect output.
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
  fallback. AMD and mixed-display visual acceptance remain required; integrated
  Intel throughput has been measured below, but its full capture path still needs
  acceptance on a display driven by that adapter.
- Video offers 720p, 1080p, 1440p and 2160p (4K), each at 15/30/60 fps; default
  1080p/30. Aspect ratio is preserved without upscaling. These are targets, not
  measured throughput guarantees. The selected preset is a maximum; adaptive modes
  also allow 360p and 5 fps, down to roughly 167 kbit/s. Preset maxima range from
  1.25 to 32 Mbit/s.
  Access units are bounded to 4 MiB. H.264 Baseline level 5.2 is advertised with a
  90 kHz RTP clock. Initial/reconfigured encoders start with an IDR. Join/loss
  feedback can force another with a 200 ms minimum interval. The fallback IDR
  interval is ten seconds by elapsed time; the hardware GOP target is also ten
  seconds of nominal frames. This removes both former one-second IDR triggers.
  Static sources still refresh their held image once per second, normally as a
  delta, preserving viewer liveness without repeatedly discarding reference detail.
  Feedback can refresh that held image before the heartbeat is due. Feedback is
  retained while hardware or sender admission is busy. Automatic/scene-change
  IDRs reset recovery spacing too. Drivers can insert earlier IDRs or reject/clamp
  the optional GOP target; diagnostics expose setter acceptance and readback.
  Hardware input remains capped at four outstanding samples with a 500 ms stall
  deadline. CBR and mean-bitrate settings must be accepted by the encoder.
  The optional H.264 HRD target is 200 ms of codec output, in **bytes**
  (`bitrate / 8 / 5`), increased from 100 ms to give IDRs/scene changes more budget.
  This is rate-control headroom, not added playout delay or a hard frame-size bound.
  The sender's 100 ms byte admission, 200 ms queue-age and 350 ms in-flight limits
  remain unchanged, including when the driver rejects/overruns the HRD target.
  The optional quality/speed hint is 0 (fastest);
  it preserves selected dimensions, FPS and bitrate but can reduce coding fidelity.
  Diagnostics expose its readback; the measured quality tradeoff is listed below.
- The network remains the Rust WebRTC client and single-process SFU; the SFU
  forwards compressed video without transcoding. The publisher byte-paces packets
  at 1.2 times target codec bitrate (transport overhead allowance), with at most
  16 ms of burst credit and nominal 8 ms batch sleeps to accommodate OS timer granularity. It does not accumulate
  catch-up credit during stalls. Stale/partially sent frames trigger IDR recovery
  instead of terminating the share. Encoded queues hold 200 ms of target-rate
  frames (six at 30 FPS, twelve at 60 FPS); frames older than 200 ms are rejected
  before sending. The in-flight frame retains a 350 ms capture-age deadline, and
  each RTP write has a 100 ms timeout, so this is not an end-to-end latency bound.
  Admission reserves queue space for pending hardware output and also pauses raw
  input when unsent wire bytes exceed 100 ms at the pacing rate. This count includes
  the in-flight frame and decreases after successful packet writes; failed enqueue,
  recovery drops and cancellation release the remainder. Large frames therefore
  suppress new encoding before filling the queue with H.264 dependencies, even
  when a driver rejects the smaller HRD buffer. The latest raw capture is retained
  during backpressure (subject to its existing 250 ms age limit) and replaced by fresher
  captures. Demand resumption or quality reconfiguration can re-encode the last raw
  image with a fresh timestamp, allowing static sources to resume without a new
  compositor event. These admission limits do not bound unfinished hardware output or
  shorten an individual oversized frame. Discarding dependent frames during recovery does not
  request another keyframe for each skipped delta.
  Native peers request 1 MiB receive/256 KiB send UDP socket buffers through the
  existing WebRTC runtime. This prevents kernel loss during coarse pacing wakes;
  application frame/age bounds remain independent. The OS can clamp socket sizes.
  SFU egress uses independent microphone, video and shared-audio writers. Per
  receiver, video is bounded to 512 queued packets and 512 KiB; each audio queue
  has 32 packet slots and 32 KiB. Byte reservations include the in-flight packet,
  serialized RTP and 64 bytes of transport overhead, and release on cancellation.
  Video expires at 150 ms and audio at 100 ms from egress enqueue, including time
  waiting to write. These limits do not bound the WebRTC driver's internal queue
  or network latency. Screen writes poll driver admission once and cancel if it
  would wait, so microphone delivery never waits for a pending video write to
  complete or time out. Backpressure/errors discard that source's backlog and
  back off screen writes for 20 ms; video queue loss, expiry and write failure
  request a rate-limited fresh keyframe through the existing recovery protocol.
  Only microphone write failure or connection-level failure ends the call;
  screen transport errors leave voice connected. Recovery uses the existing
  viewer dependency-loss handling and publisher IDRs, without renegotiation.
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
  to `127.0.0.1` and each usable local IPv4 interface, with no STUN/TURN and no
  audio track. WebView binds its host socket to an interface address once media
  permission enables adapter enumeration (for example after camera use), and
  Windows refuses sends from such a socket to loopback or to another local
  address. The bridge therefore offers the WebView's own address; those ports
  are LAN-reachable, but ICE credentials and the DTLS fingerprint exchanged
  in-process restrict the session to the negotiating WebView. Rust/WASM
  negotiates this peer using control-only Tauri commands, and attaches its
  MediaStream to a real HTML `video` element. There is no native H.264 decode,
  RGB/JPEG conversion, image protocol or frame polling. RTP payload numbers are
  remapped to the WebView's negotiated codec. The WebView owns video decoding,
  timing and GPU composition; actual hardware decode depends on its codec/driver
  support and must be checked on each host. This adds a local encrypted transport
  hop, not another encode. No raw pixels or PCM cross Tauri IPC.
  Where supported, `jitterBufferTarget=150` ms gives the player time to smooth
  paced keyframe bursts. This deliberately trades some playback latency for
  consistent presentation; it is a target, not an end-to-end delay guarantee.
  Unsupported WebViews retain their native buffering. Diagnostics are sampled
  once per second while the independent 250 ms viewer heartbeat continues.
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
  Microphone queues and writers are separate from screen traffic. Video and shared
  audio require explicit viewer subscriptions. Negotiated screen publishers can
  supply two independently encoded qualities (see below); multi-share and
  constrained-network capacity remain unmeasured.

Existing voice STUN/TURN, UDP addressing, certificates and per-hop DTLS-SRTP
apply. The SFU forwards encoded video/audio without transcoding. This is transport
encryption, not end-to-end encryption against the server. See [audio.md](audio.md).

## Viewing demand and adaptive quality

`screen_subscriptions` is negotiated in the voice offer/answer. Clients send a
bounded, full replacement `screen_views` list every 500 ms while connected.
Each entry binds a room slot, account ID and share epoch to a receiver bitrate
budget. The server ignores stale identities/epochs and self-subscriptions;
invalid bounds or duplicate slots reject signaling. Views expire after three
seconds without renewal. Deafen clears subscriptions. Every new share has a new
epoch, including stop/start without leaving voice. Legacy clients can still use
microphone voice; publishing and viewing screens require updated clients. New clients
require a subscription-capable server to publish.

The SFU sends `screen_target` once per second to opted-in publishers. Zero means
no viewers: native encoding and video sending pause, with capture reduced to one
raw refresh per second so a static source can resume. Publisher demand also expires
after three seconds if control feedback stops. The SFU immediately excludes
unsubscribed receivers from video and shared-audio fanout, including already queued
packets. Local close/hide reaches the SFU on the next 500 ms renewal; a stalled UI
first loses its three-second local viewer lease. Publisher shared-audio capture
continues until Stop sharing; it is not forwarded to non-viewers.

Receivers sample native packet-loss/frame-queue counters and sustained WebView
decoder overload once per second. Screen budgets start at 32 Mbit/s so the selected
maximum is attempted immediately; cameras use the same controller capped at their
1.5 Mbit/s encoder limit. Budgets remain bounded to 200 kbit/s–32 Mbit/s.
Ordinary congestion requires three bad samples out of four and reduces the budget
15%, at most once every three seconds. One or two transient samples do not reduce
quality. Severe packet loss (at least ten packets and 15% of expected packets), or
three consecutive samples without complete frames, reduces the budget 35%, at
most once every two seconds. The first two seconds allow negotiation/IDR recovery.
The separate one-second static delta refresh prevents idle desktop content from
counting as a stall. Six healthy receiving samples start recovery with a 12% probe,
followed by another probe every three healthy receiving samples. One or two idle
samples preserve progress but cannot trigger a probe; congestion or a three-sample
stall clears progress. Uninterrupted recovery from the floor to 32 Mbit/s takes
under 150 seconds. A sampling gap over three seconds clears accumulated evidence
and restarts setup grace without changing the retained budget; duplicate/subsecond
polls cannot accelerate adaptation. Each viewer/stream owns independent state.

Legacy publishers use the lowest live viewer budget; negotiated layers use the
independent targets described below. The publisher selects a
resolution/FPS/bitrate combination under its selected maximum, recreating the GPU
encoder with an IDR when quality changes. Ordinary downgrades require two seconds
of continuously lower targets; upgrades require six seconds at a stable target
with 20% bitrate headroom. The selected maximum remains reachable at its nominal
budget, including 4K/60 at the 32 Mbit/s ceiling. Rebuilds have a five-second
cooldown; a budget below 60% of the current mode's nominal bitrate bypasses both
dwell and cooldown for immediate downgrade. While a normal downgrade is pending,
the existing encoder retains its nominal bitrate (up to five seconds), while raw
input admission and pacing use live demand plus the existing 20% transport
overhead allowance. Lowering nominal codec output does not shrink the capacity
available to a non-proportional IDR burst. Byte backpressure sheds raw input
before creating dependencies when nominal output exceeds that rate. This avoids
independently snapping the sender to a lower ladder rung during a held mode.
Zero/expired demand pauses immediately and clears pending transitions; renewed
demand chooses its current target immediately. Capture FPS follows the highest
committed encoder FPS across the active layers, so a noisy budget cannot
independently oscillate capture cadence.
Queued/in-flight frames share their encoder's admission rate; the sender also caps
each packet's rate by fresh demand and preserves byte debt across rate changes.
The bounded queues, expiry and recovery rules still apply, including when an
already encoded burst cannot drain after a severe capacity collapse. Diagnostics expose
actual `target_bitrate`, `target_fps`, `resolution` and `video_paused`.

The paced sender measures each encoded access unit's wire size against live demand.
A burst needing more than 200 ms is rejected before its first packet. During sending,
rate cuts and delays must leave projected completion within 230 ms. The receiver's
250 ms assembly window, sender's 200 ms queue age and 350 ms capture age are unchanged.
The initial 50 ms and in-flight 20 ms margins allow transport/scheduler variation;
they cannot guarantee a network deadline. Partial bursts still require SPS/PPS/IDR.

Recovery retries the existing, warmed encoder before changing mode. A cold IDR can
be much larger than its next recovery IDR, and reducing FPS can make an IDR larger.
Three rejected recovery IDRs step down one resolution/FPS mode, without increasing
either dimension or FPS. The pacer retains the available demand instead of falling
with nominal codec bitrate. Each layer retains its own failure evidence, including
at the floor; consuming feedback or delivering a fallback frame does not erase it.
Six seconds of actual complete-frame delivery (no gap over two seconds) are needed
before normal upgrade dwell starts. A mode that failed three recovery IDRs requires
20% more demand before it can be selected again. Silence cannot authorize upgrades.
Zero demand resets recovery with the rest of the paused encoder state.

If three recovery IDRs still fail at the normal 360p/5 floor, capture reduces actual
width/height to 80% of the previous size, bounded to 25% of the original dimensions.
This emergency scaling retains the configured 166666 bit/s codec target, truthful
metadata and live demand pacing. Each step rebuilds once and gets the same bounded
warm retry opportunity. Successful delivery stops the reduction. Restoring dimensions
requires both healthy delivery and 20% more demand than the failed size had; stable
floor demand therefore cannot cycle back to a known failing size. Diagnostics retain
`recovery_scale_percent`, actual resolution, and the last rejected wire size, wire
rate and encoder bitrate, alongside the cumulative `sender_burst_recovery` count.
No quantization override, larger latency cap or software fallback is used.

`screen_gpu_probe --adapter=0 --sender-recovery` exercises two simultaneous hardware
encoders sharing one synthetic FP16 scene through the production Sender, Inbox and
OpenH264 decoder. It checks twenty seconds at 1.25 Mbit/s, twenty seconds at the
200 kbit/s floor, and fourteen seconds of restored 1.5 Mbit/s demand. On an NVIDIA
GeForce RTX 4070 Ti, the original 3a273 reproduction rejected 100 frames and received
zero in twenty seconds: 40758-byte cold 720p/15 IDRs led to 131017-byte 720p/5 and
77899-byte cold / 6287-byte warm 360p/5 IDRs, followed by repeated upgrades. The fixed
path retries 720p/15 and delivers its 24263-byte warm IDR without a quality change.
At 200 kbit/s the measured 6287-byte floor IDR still cannot fit; 512x288 yields a
4297-byte recovery IDR after warmup. Both hardware layers continue static heartbeats
and ten-second IDRs and restore 720p/15 when capacity grows. Deterministic tests replay
these non-proportional sizes, repeated floor rejection, retained failed-mode evidence,
healthy and stale delivery, independent layers, and dependent frames after recovery.
The retained [original failure trace](benchmarks/screen-sender-before-windows-2026-10-21.txt)
and [fixed dual-encoder trace](benchmarks/screen-sender-windows-2026-10-21.txt) show
first decoded delivery at 593/584 ms for 1.25 Mbit/s and 1407/1423 ms at the floor.
Both 1.25 Mbit/s runs decoded 21 frames in twenty seconds with one rejected cold
IDR and no mode changes. Both floor/growth runs decoded 34 frames with five
rejections and three configurations; maximum assembly was 187686 microseconds.
These are synthetic-image hardware results, not physical network acceptance.

Integration requires the SFU to admit the selected layer using measured wire-byte
policing, not reject truthful nominal bitrate metadata during publisher hysteresis.
For example, a held 5 Mbit/s encoder paced at 4.9 Mbit/s must continue through the
same route. The frontend's 120%-of-demand pacer and 200 ms access-unit admission
fit the backend's 120% rate and 250 ms burst bucket; authentication, source/layer,
epoch and per-viewer measured traffic limits remain authoritative. Local Inbox
coverage alone does not establish this: the backend encrypted regression must also
cover the 5 -> 4.9 Mbit/s hold and abusive/understated metadata traffic.
`requested_height`, `requested_fps` and `viewer_bitrate_budget` distinguish the
selected maximum from receiver-driven adaptation. A 4K preset does not upscale
an ultrawide 3440x1440 source to 3840x2160.

Presentation drops and PLI recovery requests are not themselves congestion.
Decoder load and complete-frame throughput use a half-weight moving average.
Pressure requires three of four intervals with arriving frames and either no
decoded output, at least 80% of elapsed time spent decoding, or smoothed decoded
throughput below 80% of arriving complete frames. Pressure clears after two
intervals below 65% decode work and at least 90% throughput, avoiding threshold
chatter. Static input, a stats gap over three seconds, counter resets and invalid
stats clear decoder evidence. This detects overloaded hardware decoders with low
reported CPU decode time while keeping display refresh/presentation drops separate.
The receiver then applies its own sustained-evidence rule to decoder pressure;
this deliberately favors stability over immediate reaction to a brief decode spike.

Deterministic trajectory tests cover isolated/two-sample spikes, sustained/severe
congestion, no frames, camera/screen recovery, duplicate and stale samples, static
refresh gaps, quality threshold oscillation, rebuild cooldown/headroom, independent
controllers and demand expiry/resumption. Decoder tests cover CPU and hardware
throughput overload, hysteresis, stats reset and suspension. These are controller
regressions; they do not establish real network or GPU performance.

`screen_layers` is an opt-in offer/answer capability. Its default is false and
false is omitted from answers to older servers. Negotiated publishers attach their
selected `quality` to the screen start command. Targets additionally contain
`layers: { primary, low }`; omitted fields retain the old single-encoder protocol.
Layer zero uses SSRC 901 and layer one uses SSRC 904. Both carry the current share
epoch and nominal bitrate of the actual encoded mode in two CSRCs, validated for
shape/range and the current epoch before forwarding. The SFU removes bitrate
metadata before sending to receivers. A target change does not relabel queued
frames: the server retains their ingress layer and enforces measured viewer
budgets independently of nominal mode. Stale upstream packets cannot be retagged
as a new share. Cameras and shared audio keep their existing contracts.

The primary target comes from the highest live viewer budget, capped by the selected
preset. Targets preserve the continuous budgets rather than rounding to a codec
preset, so each encoder retains headroom evidence and demand-capped pacing.
Only when a viewer budget is below the primary target does the SFU request the
second encoder. Its budget is the weakest viewer budget, capped at 1.25 Mbit/s and
25% of the selected preset bitrate; its maximum is 720p/15. Intermediate viewers
receive the low layer if their budget cannot accommodate the primary. This is a
bounded two-quality policy, not one encoder per viewer. The primary keeps its full
preset when a 200 kbit/s viewer joins. Total target codec upload and pixel work are
at most 125% of the selected preset (at most 33.25 Mbit/s codec upload); each pacer
adds the existing 20% transport allowance. This is a ceiling, not an uplink capacity
estimate. Multiple simultaneous shares still have no shared-link allocator.

Windows uses one WGC session and fans out owned FP16 textures to two latest-frame
mailboxes. Each encoder owns its MFT, NV12 processor, four-sample admission limit,
200 ms frame queue and 100 ms unsent-byte budget. Capture owns seven bounded textures
when layered (three for legacy capture), each processor at most six NV12 surfaces.
Low-layer input has its own FPS admission and never blocks primary queue admission.
No raw readback or software fallback is introduced. A zero/expired low target drops
its MFT and processor; the worker remains idle until renewed demand. Hardware unable
to create the second session fails the share explicitly. Stop, deafen, disconnection,
source closure and worker failure stop and join both encoders and the audio worker.

The SFU selects exactly one layer per viewer before egress admission, retaining the
same relay SSRC and share epoch. A switch drops deltas until a contiguous SPS/PPS
prefix and IDR start arrive; fragments later in that IDR use ordinary loss recovery.
Prefix buffering is bounded to 4 MiB/4096 packets/350 ms and does not hold an entire
large IDR for burst delivery. Layer switches and hide/reopen preserve outbound RTP
sequence continuity. Each selection has an extended ingress sequence floor and a
128-packet duplicate/reorder window; packets preceding its recovery prefix cannot
reuse an earlier selection's outbound indexes. Forward jumps above 4096 packets
and older out-of-window sequences cannot advance the current mapping. A fresh,
contiguous SPS/PPS/IDR prefix can resynchronize above the previous outbound high
index without guessing the publisher's rollover. Invalid jumps/deltas leave the
current selection usable. Legacy publishers use the same sequence routing and
can recover on a fresh IDR with the decoder's cached parameter sets. Initial
legacy forwarding remains compatible with peers lacking an SPS/PPS prefix.
Ordinary packet gaps stay visible
for receiver loss detection. A separate final transport window also fences queued
duplicates, discontinuities and source/slot reuse, committing only successful
atomic driver enqueues. Sequence state lasts for the receiving connection and is
never reset by a subscription, share epoch, publisher identity or route clear.
After locally measured throttling or a final-writer sequence discontinuity,
the upstream route is invalidated and fresh recovery entries start a new mapping above
the last emitted index with one visible loss gap. This preserves recovery even
when throttling discarded more than half a sequence space; it cannot reset or
reuse the outbound index history.
No discontinuity, IDR, source replacement or subscription reset can erase the
receiving connection's outbound SRTP index history.

Timestamp freshness is separate: normal switches on the same compositor clock
reject older/equal IDRs, including across timestamp wrap. Subscription/share/access
epoch changes reset that clock comparison, and 60 seconds without forward media
requires a fresh SPS/PPS/IDR and resets it as well. This avoids ambiguous signed
timestamp comparisons after the roughly 6 h 38 min half range at 90 kHz. Ingress
age and authorization fences still apply; timestamps are not proof of freshness.
Recovered selections reject packets with timestamps preceding their recovery
prefix; that floor slides behind forward media with a 60-second window so long
uninterrupted calls do not strand timestamp comparisons.
Old viewers can decode the selected SPS/PPS/IDR without any new receiver fields.
Keyframe requests are authorized against live viewer leases and routed to the
selected encoder; the existing publisher rate limit remains shared by both layers.
Joins and target changes request an IDR and periodic recovery remains available.
Both screen SSRCs share the original publisher ingress rate limit. Publisher
bitrate metadata describes the actual encoded mode; it neither grants bandwidth
nor rejects an otherwise eligible held mode. A 5 Mbit/s nominal encoder may stay
selected while demand/pacing falls to 4.9 Mbit/s. Layer choice still follows live,
authorized demand: weak viewers use the low encoder when a higher primary is
needed. Queue entries retain the server-observed layer index and recheck that
selection at the final transport poll, so queued primary packets cannot cross a
switch to low. If only a weak viewer remains, it can receive the selected primary
while the publisher reconfigures, but only within its measured byte allowance.
Epoch, subscription and access checks remain authoritative independently of mode.
No publisher claim bypasses these limits. Every screen viewer/source slot
(including legacy screen publishers)
has measured token buckets before queue admission and at the final transport poll.
They count serialized RTP plus 64 bytes for SRTP/UDP/IP, allowing 120% of the
authorized subscription bitrate and a 250 ms burst (minimum 3000 bytes). A
200 kbit/s view therefore permits at most 30,000 measured bytes/second plus
7500 burst bytes, independent of the publisher's bitrate claim. TURN framing and
lower network layers are not separately metered. Budget downgrades cap existing
credit immediately at the next poll; upgrades do not retroactively refill it.
Renewals, unsubscribe/resubscribe, share epochs and source changes do not replenish
credit. Each bucket has constant memory per slot.

Screen video and system audio have no SFU NACK retransmission cache. The pinned
WebRTC responder writes cached RTP below application authorization/lifecycle and
byte accounting, without a callback that can safely reacquire those gates.
A stream-specific wrapper excludes these SSRCs from cache binding (including
negotiated RTX) and discards their authenticated NACKs before bitmap expansion
or retransmit queueing. Screen NACKs cause no extra media or keyframe work.
Microphone/camera retain the default responder; camera shares H.264 but has
distinct SSRCs. RTCP reports, TWCC and upstream loss feedback remain enabled.
Screen receivers recover dependency loss through the existing explicit,
authorized, coalesced `ScreenKeyframe` command and periodic publisher IDRs.
This intentionally trades packet-level screen repair for a bounded IDR recovery
pause on lossy links. All recovery media takes the ordinary, measured egress path;
a future retransmission cache must use those same live gates and byte budgets.

Over-budget packets are dropped without waiting, invalidate that viewer's pending
screen route/backlog and request a coalesced IDR. Layered delivery resumes at a
fresh recovery prefix when credit permits; an encoder that continuously exceeds
the budget may remain undecodable until it reduces output. Existing bounded
512-packet/512-KiB queues, 150 ms video age limits and both-endpoint authorization
checks remain in force. Camera, microphone and system-audio paths are unchanged.
Expired/removed subscriptions discard buffered switch prefixes.

This remains an application feedback controller, not a bandwidth guarantee.
Packet-loss, bandwidth-step and hardware reconfiguration trials with two desktop
clients remain required. The synthetic `screen_gpu_probe --layers` exercises two
real hardware encoders on one FP16 surface, independently decodes both resolutions,
and destroys/recreates the low encoder while the primary remains alive.
On the local RTX 4070 Ti, this probe decoded 30 primary frames in each of four
phases, plus ten 360p/5 and ten 720p/15 low frames in their active phases; all
phases completed in 1.03–1.83 seconds. This establishes concurrent synthetic
hardware encoding on that GPU, not full desktop/network acceptance on other
adapters. Publisher diagnostics expose low-layer labels, counters and timings
with a `low_` prefix; primary metrics retain their existing names.

## Authorization and lifecycle

ViewChannel, JoinVoice and current membership/session checks remain mandatory.
Speak also grants screen/system-audio publishing in this first version. There
is no separate screen-sharing permission/editor yet. Explicit `screen` signaling
state gates both publisher ingress and receiver egress. Queued packets are
checked against the current publisher identity, share state, receiver deafen
state, viewer subscription/lease and access epoch, including slot reuse. Voice uses mutation-only packet
admission/draining: ordinary database authorization checks do not hold the chat
gate or stall media. Access-changing commits invalidate queued media and require
fresh authorization, with conservative voice revocation. Transport writes hold
room locks and packet permits only during individual nonblocking polls. A pending
write releases both and revalidates before resuming; a slow receiver cannot hold
up room updates or subsequent media readers. This relies on the pinned WebRTC
`write_rtp` implementation enqueuing atomically on its final poll.

Each microphone, screen video, camera video and shared-audio writer keeps its independent bounded
queue. Queue admission checks the current viewer lease and share epoch before
reserving bytes; intentional filtering does not count as congestion or request
recovery. The same viewer and epoch checks run under the room lock on every
transport poll, including after waiting for room access. Unsubscribe, lease
expiry and share restart therefore invalidate packets already dequeued by a
writer as well as queued packets. These viewer changes do not stop microphone
delivery, and fresh subscriptions can resume screen delivery.

`shared::voice::MediaKind` owns publisher SSRCs, relay ranges, payload types and
track/mixer slot mappings. Internal SFU queues carry the kind explicitly.

Leave, channel switch, logout/session replacement, connection failure, window
destruction and capture/sender errors cancel the capture lease. A scheduling gap
over two seconds stops capture rather than resuming it after suspend. Rejoining
voice never restarts a share automatically. A stalled OS capture call retains
the single worker slot until it returns; users cannot accumulate capture threads
by repeatedly pressing Start. Captured frames awaiting send are age-limited.

The version-1 voice offer adds `screen_video`, `screen_feedback` and
`screen_subscriptions`; older offers default these to false. A new client opts
into feedback/diagnostic and subscription events in its answer only when the
server advertises support. Older clients never receive new event variants.
Screen publishing requires subscription support. Feedback checks
current membership/access generation, deafen, publisher Speak/sharing state and
stream epoch, and coalesces requests across viewers.
Participant snapshots add `sharing_screen` and `sharing_audio`, both defaulting
to false, and a `screen_epoch` identifying each share incarnation. The SFU
stamps that epoch in video CSRC metadata; receivers discard packets from reused
slots whose epoch no longer matches their roster. Clients only send the new screen command to supporting servers.

## Compatibility and acceptance

This delivery prioritizes Windows publishing, as requested. macOS/Linux desktop
builds retain the receiving/video-player path; their screen publishing controls are disabled. Camera capture instead uses the WebView on all three platforms, subject to its H.264/WebRTC support; hardware acceptance remains required.
Their native screen/audio permission and capture implementations remain follow-up
work. The existing Rust WebRTC implementation is retained. No new SFU or WebView
capture library is selected in place of the native transport.

Automated checks cover codec/RTP round trips and loss, bounded malformed input,
wire compatibility, capture cancellation state, and real-peer SFU video/audio
forwarding, isolation, viewer subscription/unsubscribe, lease expiry, adaptive
budget bounds, stop/restart, mute/deafen and permission revocation.
CI runs portable codec tests and native compilation on all three hosts.

The Windows synthetic GPU check `cargo run -p thiscord-frontend --example
screen_gpu_probe --features screen-share --locked -- --adaptive` exercises
1080p/30 -> 720p/15 -> 360p/5 -> 1080p/30 encoder replacements with one decoder,
checking an initial SPS/PPS/IDR and ten decoded frames at each step. This passed
locally; it does not establish behavior on every GPU or under network congestion.

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
SFU counters distinguish ingress drops and egress losses (including queue budgets,
screen backpressure and write errors) from write-deadline timeouts;
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
`encode_latency` includes capture waiting, GPU processing and encoding;
`encode_input_age` measures capture to MFT submission, and `encoder_service`
measures MFT submission to output retrieval, including output polling delay.
These separate raw-input scheduling delays from time inside the encoder.
`capture_surface_busy`, `encoder_input_busy` and `conversion_surface_busy`
split the aggregate `gpu_busy` counter into capture-pool, MFT-credit and
conversion-pool pressure.
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
`encoder_gop_target_frames`, `encoder_gop_applied` and
`encoder_gop_readback_frames` similarly expose the hardware GOP setting. Actual
`keyframes` and `keyframe_feedback` counts distinguish driver IDRs from feedback.

`cargo run -p thiscord-frontend --example screen_gpu_probe --features screen-share --profile ci --locked`
uses synthetic FP16 textures, GPU scale/tone-map/NV12 conversion and GPU H.264,
then decodes the output to check expected SDR levels. It also reads monitor HDR
metadata without capturing pixels. Local NVIDIA validation passed 360 frames:
720p SDR, 1440p-to-1080p HDR normalization and 4K-to-720p HDR highlights. The attached
HDR monitor reported 240-nit SDR white. Intel/AMD acceptance remains outstanding.

Add `-- --burst` to this probe command to encode/decode 180 frames of detailed
synthetic scenes at 2560x1070, 18 Mbit/s and a 60 FPS input target, with repeated
keyframes. This exercises rate control without desktop capture or network traffic.
Historical NVIDIA validation with the former 100 ms buffer accepted/read back the
225,000-byte HRD target, decoded
all 180 frames and produced three IDRs around 170 KB (170,123-byte peak). This is
synthetic evidence, not a before/after measurement of a user's shared content.
The deterministic pacing regression replays 700 KB wire bursts and coarse timer
wakeups: slot-only admission exceeds the sender age limit; byte admission avoids
those queued-frame expirations at the same pacing rate by deferring raw input.
Live two-client playback and representative scene-change quality still require acceptance.

Add `-- --keyframes --adapter=0` for a paired synthetic fidelity/recovery probe:
360 identical 1080p/60 input frames at 10 Mbit/s, comparing the former one-second
GOP/100 ms HRD, ten-second GOP/100 ms HRD, and ten-second GOP/200 ms HRD. It reports
GOP/HRD readback, IDR frame indices, peak bytes, overall/worst luma PSNR and the
ten-frame windows before/after each second. Each arm forces recovery at frame 210
and decodes that SPS/PPS/IDR with a new decoder to simulate a joined/lost viewer.
The probe uses GPU NV12 readback and software decoding only for measurement;
elapsed runtime is not encoder or network throughput. Its finite moving scene
does not establish desktop text readability or subjective two-client quality.

Local paired results (2026-10-21), in legacy / long-GOP-only / production order:

- NVIDIA RTX 4070 Ti: aggregate luma PSNR **32.541 / 35.429 / 39.161 dB**,
  worst-frame PSNR **24.291 / 24.857 / 30.489 dB**, peak access-unit sizes
  **99,298 / 99,298 / 189,156 bytes**. Output rates were 7.078 / 8.283 / 9.248
  Mbit/s at the unchanged 10 Mbit/s codec target. At the first three one-second
  boundaries the legacy ten-frame mean fell by 9.1–9.6 dB; the production arm
  had no corresponding drop (changes +0.06 to +0.15 dB).
- Intel UHD 770: aggregate PSNR **26.915 / 29.300 / 29.506 dB**, worst-frame
  **21.781 / 21.833 / 22.936 dB**, peak **84,612 / 69,586 / 89,647 bytes**.
  Output rates were 10.107 / 10.075 / 10.003 Mbit/s. Its later scene interval
  remained less faithful even with the new policy; removing periodic IDRs does
  not eliminate all hardware rate-control quality variation.

All six arms decoded 360/360 frames and recovered at the requested frame 210.
Both drivers accepted/read back GOP 60 vs 600 and HRD 125,000 vs 250,000 bytes.
Legacy IDRs occurred at 0/60/120/180/210/270/330; both long-GOP arms emitted only
0/210. These are finite synthetic results, not perceived-quality guarantees.
The larger HRD permits bigger frames; application pacing/expiry bounds remain
authoritative. AMD and real two-client motion/text/network acceptance are pending.

The same change passed `screen_pipeline_probe --seconds=30 --assert` on the
NVIDIA adapter: 1,796 assembled 4K frames, 59.46 sustained FPS, four IDRs, zero
encoded drops/receiver packet loss, and 163 ms maximum measured sender age.
This exercises WGC through the production GPU encoder/pacer and encrypted local
WebRTC receive queue, not the SFU, WAN or visible browser presentation.

`screen_encode_probe` tests the legacy system-memory input encoder API with
synthetic data; that API is not the production capture path. `screen_preview_probe`
serves a synthetic local WebRTC video session at `http://127.0.0.1:18741` for 20
seconds. No desktop pixels, accounts, microphones, recording, STUN or TURN are used.
A local browser run decoded 60 FPS with zero reported decoder freezes/loss; the
hidden preview dropped presentation frames. This does not establish two-client
capture-to-display performance or hardware decoder selection in the actual app.

### 4K throughput and playback probes

Use an optimized build for performance measurements:

```powershell
cargo build -p thiscord-frontend --example screen_gpu_probe --example screen_pipeline_probe --features screen-share --profile ci --locked
.\target\ci\examples\screen_gpu_probe.exe --throughput --adapter=0
.\target\ci\examples\screen_gpu_probe.exe --quality --adapter=0
.\target\ci\examples\screen_pipeline_probe.exe --seconds=180 --assert
.\target\ci\examples\screen_pipeline_probe.exe --browser --seconds=180 --assert
```

`--throughput` pre-renders eight moving FP16 source frames, then measures 600
frames at 60 FPS and 600 without a cadence limit. Both modes perform the production
capture copy, tone map, NV12 conversion and hardware encoding. Source application
rendering is excluded. The unpaced run is GPU-path capacity, not displayed FPS or
a network throughput claim. `--adapter=N` chooses a DXGI adapter for this probe.
`--quality` separately reads back NV12 and software-decodes 120 frames to compare
luma PSNR against the driver default. Its readback/decode costs are never included
in the throughput result and are absent from the production capture path.

`screen_pipeline_probe` instead captures an animated, DPI-correct **3840x2160**
synthetic window with WGC, even on a smaller monitor. It exercises the production
encoder, sender, expiring demand, receiver controller and receive queue through
two encrypted WebRTC peers. `--browser` adds the production local preview
bridge. Negotiate a video-only receive offer via `POST /offer` on the local probe
page at `http://127.0.0.1:18741`, using the collaborative browser tools, and set
its receiver's `jitterBufferTarget` to the player's 150 ms value. Keep the preview
visible and measure `requestVideoFrameCallback` presentation counts as well as
`getStats` decoded counts; hidden playback does not establish displayed FPS.
Only the dedicated synthetic window is captured. No account, microphone or
desktop content is used. The native `--assert` checks dimensions, sustained
throughput, encoded losses, RTP loss and preview errors; it cannot assert browser
presentation and is intentionally separate from the visible browser measurement.

On the local i7-13700K/RTX 4070 Ti (Windows, HDR monitor with 240-nit SDR white),
the optimized GPU path measured **201.56 FPS**, or **3.36 times** the 60 FPS
target. Encoder service time in that unpaced run was 4.33 ms mean/4.67 ms p95;
CPU submission p95 was 0.50 ms. The integrated UHD 770 also sustained 60 FPS,
but its unpaced result was 67.22 FPS: little spare capacity on that adapter.
This is evidence for the measured hardware, not a guarantee for arbitrary GPUs.

The final three-minute WGC/transport/browser run on 2026-10-15 delivered all
10,844 encoded frames with no encoded drops, RTP loss or preview errors. Native
throughput after five seconds of warm-up was 59.70 FPS. A fixed 154-second visible
browser interval (excluding startup and teardown) measured **59.76 presented FPS**,
one presentation skip, zero decoder drops, zero freezes and zero local-hop loss.
Mean decode time was 0.66 ms/frame; mean browser jitter-buffer residence was
114.9 ms with the 150 ms target. Native encoder service averaged 4.60 ms and
capture-to-completed-send averaged 19.16 ms. These are separate local timings,
not a synchronized capture-to-display latency measurement. A GPU utilization
sample during playback was 15% overall, 10% encoder and 30% decoder. The full
probe includes synthetic source rendering and both local peers on one machine.

The longer run exposed a second worker-clock throttle that the short runs missed.
Removing it reduced raw replacements from 182 to 34 over three minutes, while
capture admission continued to cap source work. This, asynchronous MFT output
wakes, bounded pacing credit/socket buffers, normal keyframe spacing and playback
buffering address distinct bottlenecks; changing the quality picker is not a fix.

The speed hint has a measurable fidelity tradeoff on the moving synthetic scene:
31.29 dB luma PSNR versus 32.87 dB for the NVIDIA driver's default hint (33), at
the same 3840x2160/60 FPS/32 Mbit/s configuration. This does not establish
subjective text or video quality for every source. Re-test representative content
before changing the speed hint or claiming equivalent image quality.

The ignored PostgreSQL test
`sfu_sustains_authorized_4k60_and_limits_excess_egress` exercises actual encrypted,
authenticated peers and the SFU at 32 and 64 Mbit/s with concurrent microphone
traffic. It requires full 32 Mbit/s payload delivery to a 32 Mbit/s view (within
the overhead allowance), then bounds measured egress when the publisher sends
64 Mbit/s to 32 Mbit/s and 200 kbit/s views. It also requires fresh encrypted
delivery after the flood ends and continued microphone service throughout.
The former expectation of unrestricted 64 Mbit/s delivery to a 32 Mbit/s view
was incompatible with bandwidth enforcement. Payloads have video packet sizes
but are not decoded images. The Windows pipeline probe does not itself traverse
the remote VPS or emulate its network path.

Tests cover sequence wrap, duplicates, bounded reordering deadlines, recoverable
and overflowing backlogs, feedback throttling/permissions and compatible wire
negotiation, plus existing codec/lease/permission tests. The production encrypted
regressions in `backend/tests/voice/recovery.rs` measure nominated ICE-pair bytes
under 100 authenticated NACKs, including expanded bitmaps, while live and after
unsubscribe, lease expiry, source stop, a new share epoch and permission revocation.
Wire counters detect duplicate ciphertext that SRTP replay protection would hide
from the application. A camera positive control requires real retransmitted bytes.
The same suite exercises IDR recovery after 4096 lost packets, larger gaps,
wire wrap, duplicates/reordering and reopening for layered and legacy publishers.
Unit tests additionally check both SRTP protection profiles, RTX exclusion, final
queue loss, malformed prefixes and uninterrupted timestamp wrap.
CI compiles diagnostics
and runs portable tests on Windows/macOS/Linux. Linux needs a WebKitGTK build with
working WebRTC/H.264. Live capture, HDR appearance, Intel/AMD drivers, multi-client
network conditions and actual WebView hardware decode still require acceptance.

HDR/device references: [Microsoft Advanced Color](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range),
[MF D3D device manager](https://learn.microsoft.com/en-us/windows/win32/medfound/mft-message-set-d3d-manager),
[H.264 HRD buffer units](https://learn.microsoft.com/en-us/windows/win32/codecapi/avenccommonbuffersize-property),
[hardware GOP semantics](https://learn.microsoft.com/en-us/windows/win32/codecapi/avencmpvgopsize-property),
[encoder quality/speed](https://learn.microsoft.com/en-us/windows/win32/codecapi/avenccommonqualityvsspeed-property),
[WGC compositor timestamps](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframe.systemrelativetime),
[asynchronous MF events](https://learn.microsoft.com/en-us/windows/win32/api/mfobjects/nf-mfobjects-imfmediaeventgenerator-begingetevent),
[WebRTC statistics](https://www.w3.org/TR/webrtc-stats/).

### Local bridge records and self-tests

Every player attempt (screens and cameras) records a bounded, content-free
timeline on both sides of the loopback hop. The WebView records offer gathering,
candidate/codec summaries, ICE/connection/signaling state changes, candidate errors
without their addresses, video element events and whitelisted `getStats` fields.
The desktop records its own ICE/connection states, setup errors, answer summary,
first forwarded packet and receive counters. Both sides report connectivity-check
counts per candidate pair, so a stalled player distinguishes "WebView sent checks
but the desktop received none" from a dropped return path, a DTLS failure, no
compatible candidates, or a connected bridge without decoded frames. Records keep
classified addresses only and are retrievable through the admin dashboard
([admin.md](admin.md#video-playback-diagnostics)); they are not written to disk.

After an attempt times out or fails, the WebView runs a self-test at most once
per ten minutes, taking about 30 seconds without visible UI or media: two
WebView peers connecting to each other (data channel only), a video-free desktop
peer on `127.0.0.1`, and the same desktop peer bound to the computer's non-loopback
interface addresses. The interface peer exists only for this ICE/DTLS-authenticated
test, carries no media and closes after ten seconds. Viewers bind both, so the
two self-tests still tell which path a WebView can reach.

Chromium/WebView2 on Windows reaches `127.0.0.1` from a wildcard-bound host
socket: in a local Edge 156 run the desktop saw a loopback source and Chromium
selected a peer-reflexive local candidate. A socket bound to a specific LAN
address cannot send to loopback on Windows (`WSAEADDRNOTAVAIL`). WebView binds
interface addresses once media permission enables adapter enumeration; sharing
a (virtual) camera reproduced this locally, and loopback-only viewers then stayed
loading. Viewers and the camera bridge therefore also bind every IPv4 interface.

## October 22, 2026 crash and reconnect investigation

Windows recorded an access violation in desktop 0.1.42 at 18:19 local time
(16:19 UTC) during screen sharing with test bots. The local dump is
`%LOCALAPPDATA%/CrashDumps/thiscord-desktop.exe.36300.dmp`; retain it locally,
outside the repository. Its exception record reports a write to address `0x24`
at `ntdll.dll+0x1a12d`. Scanning the faulting thread's stack memory found code
addresses in `RTWorkQ.dll` and NVIDIA's `nvEncMFTH264x.dll`. This suggests the
Media Foundation hardware encoder path, but a stack-memory scan is not a
symbolized unwind and does not establish a driver or application teardown bug.
No encoder fix or native crash reproduction is claimed by this investigation.

The subsequent reconnect failure was reproduced independently in a PostgreSQL
and real WebRTC regression. Startup rotated the stored session token while old
voice connections remained on the backend. Their permission-refresh path
treated its retained old credential as a new replay, revoking the replacement
token. Existing voice checks now reject stale tokens without replay side effects;
new HTTP and voice handshakes still enforce replay revocation. The regression
covers old human/bot shutdown, replacement-token usability, and external replay.
The client also returns to sign-in after an HTTP-confirmed session rejection.
Signing out and back in restored the affected installed client. These source
changes require a backend deployment and client release; neither was performed
as part of the investigation.
