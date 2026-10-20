# Native audio and voice

Voice channels use native Rust capture/playback and a Rust SFU in the backend.
Screen/window sharing with system audio is available on Windows; see
[screen-sharing.md](screen-sharing.md) for the separate native capture/video path.
Only settings, levels and participant metadata cross Tauri IPC. PCM and encoded
audio never pass through WASM or handwritten JavaScript. Browser preview supports
text chat; voice requires the desktop executable. Exactly three packages remain.

The persistent bottom account ribbon has microphone, deafen and settings icons,
with accessible labels and hover/focus hints. Joining expands it to show the voice
channel/server, connection state, disconnect, screen sharing and audio settings.
The camera icon is disabled and explicitly labeled unavailable. Push-to-talk keeps
its hold/release control when enabled. Screen selection opens a modal dialog;
screen pipeline diagnostics remain in the joined voice channel view. Audio quality
details remain in Audio & voice settings. Clicking a sidebar voice channel joins
directly; the ribbon's channel name opens call controls. Hovering or focusing
**Voice connected** shows server-hop RTT, recent measured health, cumulative
server-hop loss and local transport drops. Intentional control discards are
excluded; unavailable metrics remain labeled unavailable. Sidebar rosters are available before
joining through permission-filtered chat socket updates; see [chat.md](chat.md).

## Remote support

Instance owners/admins can open a connected participant's audio settings in the
browser admin panel and adjust the same native preferences. They can explicitly
request technical diagnostic logs, including device names, while that voice
connection is active. Recordings and raw media are excluded. See
[remote audio support](admin.md#remote-audio-support) for commands, limits and
concurrency behavior. No diagnostic logs are uploaded without an admin request.

## Use

Restart the backend and desktop app after building. In Settings → Audio & voice,
select devices, adjust output level and choose voice activation or push-to-talk.
For quiet microphones, increase Microphone boost (1×–10×, default 1×). This fixed
gain runs after denoising and before voice activation and automatic gain; it also
boosts transmitted audio. See [microphone activation](voice-activation.md).
Device lists refresh every three seconds. Open a voice channel and click Join
voice. Select another voice channel and join to move yourself. The voice bar stays
available while reading text channels. Disconnect releases the devices.

### In-game voice overlay

The desktop client shows a transparent, always-on-top roster when connected to
voice and the main window is out of focus (including when minimized). It sits at
the top left of the display containing Thiscord. Your entry stays first; hovering
it reveals clickable Mute/Unmute and Deafen/Undeafen buttons. Other rows and empty
space pass mouse input through to the game. Initials stand in for missing profile
pictures. Green rings identify speakers;
mute/deafen marks remain visible. Audio & voice has an overlay toggle for the
current app session, enabled by default. Returning to Thiscord, disconnecting or
losing voice access hides the overlay.

The overlay is intended for windowed/borderless games, not exclusive fullscreen.
It uses an ordinary Tauri window, without game injection or graphics hooks.
Compositor rules can limit stacking/positioning, especially on Wayland and macOS
fullscreen Spaces. macOS transparency enables Tauri's `macos-private-api` feature;
this is for the existing direct-distribution builds, not Mac App Store submission.
Actual game/monitor behavior still needs Windows/macOS/Linux runtime acceptance.

The overlay capability exposes participant presentation metadata and a narrow
self mute/deafen command, available only while connected. It cannot select another
participant, change audio devices, or access credentials. Native cursor hit testing
enables mouse input only over your own row; the overlay stays non-activating.
Remote indicators use decoded microphone energy before playback gain, with a
250 ms hangover; missing packets and concealment do not extend activity. Screen
audio never lights a microphone indicator. Local activity follows the transmit
gate (including PTT), and mute/deafen/speak restrictions override activity. This is
an activity indicator, not speech recognition. Native visibility and UI metadata
are polled every 50/100 ms respectively; no audio samples cross IPC.

Short local rising/falling tones announce other participants joining/leaving the
connected room through server notification subscriptions. Own joins and
connection replacements/resumptions stay silent. Event/source notification
preferences can disable sound or enable a banner; see [notifications.md](notifications.md).
Bursts coalesce into bounded cues. During voice, sounds follow the selected output device, master
volume and deafen, and are included in the echo reference/debug speaker track.
They are not inserted into the outgoing microphone stream. PCM is synthesized
before device startup; callback playback uses no allocation, locks or file I/O.
The Rust palette in `frontend/src/notification_sound.rs` also renders the browser
and out-of-call desktop cues so the same event keeps the same sound everywhere.

Each speaker has an independent 0-200% voice volume slider directly in their
right-click menu while connected to the same call. Sidebar avatars gain a green
ring while speaking, using the same native activity and mute/deafen rules as the
overlay; other channels do not show inferred activity. Received screen shares
have separate volume and mute controls beside their video; changing either level
does not change the other. Gains persist locally per
guild/account ID in `speaker-volumes.json`, including 0% mutes, and restore on
rejoin/reconnect/restart regardless of username or stream slot. The same person
can have different gains in different guilds. Preferences belong to this local
installation, are not synced to the backend and are shared by accounts using this
OS profile. Test-stream gains are temporary. Device IDs, master gain, microphone boost, activation
threshold, mute/deafen,
transmit mode and processing choices persist in the OS app config directory's
`audio.json`. No tokens or recorded audio go there. Device and processing settings
can be changed during a call without leaving the voice channel.

Device failures suspend capture/playback while retaining the voice connection,
roster and volumes. When a saved device is absent from enumeration, opening audio
immediately uses the system default for that direction only; a still-present
microphone/output selection is retained. This applies on join, rejoin, live
recovery and device changes. Both endpoints are resolved before streams are
allocated, and listeners never resolve or open a microphone. The voice bar reports
the missing device and the microphone/output actually opened, including on the
first successful join. Saved preferences stay unchanged, so a reconnected headset
is selected again on the next open. If no default is available, the error identifies
the missing direction and recovery remains bounded.

Recovery makes three attempts using the selected input/output (with the missing
device fallback above), then three using the system defaults, with 500 ms between
attempts. Opening the initial devices uses the same bounded path; an immediately
successful first open does not consume the later recovery budget. Retry counts persist for the call,
including across briefly successful restarts, to prevent endless failure loops.
Fallback can change the microphone: the voice bar and audio status announce the
default devices actually opened. Saved device preferences are unchanged.

Each background open has an eight-second deadline. A stuck driver operation ends
recovery early and retains its single worker slot until it finishes; the app never
spawns unbounded replacement workers. Leaving voice, revocation, stop or suspend
discards late completions. Muting, deafening and PTT release remain effective during
recovery. Device/model changes wait until recovery finishes. Recovery ends debug
recording and clears queued remote audio before playback resumes.

After exhausting retries, voice disconnects with the last error. On Windows,
best-effort WASAPI session enumeration lists active audio-session executable names
and PIDs on selected/default endpoints. These are possible conflicts, **not proof
of exclusive ownership**; shared sessions can span processes, protected processes
may expose only a PID, and the blocking process may be absent. No process is killed.
These bounded diagnostic results are included in the local diagnostic logs. Other platforms, empty results or
failed queries explicitly report that the blocking process could not be identified.
Diagnostics run on the bounded background worker and also have a timeout.

CPAL buffer underrun/overrun (`Xrun`) and real-time scheduling (`RealtimeDenied`)
notifications keep the stream running. They may indicate an audible glitch or
reduced scheduling priority, not a disconnected device. Other notifications enter
bounded recovery and identify the microphone/output stream and CPAL error category.
Route changes also use recovery instead of accepting an implicit backend reroute.
Callbacks record atomic failure/activity counters; error formatting stays off the
callbacks. Five seconds without expected input/output callbacks also enters recovery.

## Worker resilience and diagnostic logs

Live voice uses a native, connection-scoped heartbeat with a 15-second expiry;
background WebView polling no longer keeps the call alive. Local playback/microphone
tests retain their three-second UI lease. A two-second scheduling gap is logged,
not treated as suspension. Leave, window destruction and connection revocation
still stop audio; a stale native heartbeat after a long stall/suspend also stops it.
Incoming packets use a separate bounded queue so they cannot fill the control queue.

CPAL stream destruction runs on one cleanup worker because Windows stream Drop
waits for its audio thread to exit. Capture callbacks stop accepting samples and
playback callbacks output silence before streams enter cleanup. At most four device
sets may exist across active, opening and retired streams. A hung cleanup retains
its quota slot; repeated hangs eventually require a full app restart. The OS handle
and microphone indicator may remain active until then, but retired callbacks cannot
forward audio. This avoids unsafe thread termination or unbounded replacement threads.
Ordinary device opens and recovery are bounded separately as described above.

The control worker catches unwindable panics, closes its voice connection, rejects
queued work and restarts up to three times per app worker lifetime. Rejoining uses
a fresh session. Abort/native crashes and indefinitely blocked driver or DSP calls
cannot be restarted safely in-process. An independent watchdog logs the last worker
operation and heartbeat age; queue-full, missing reply and worker-exit errors are
reported separately. Tests inject blocked stream destruction, worker panics,
media saturation, callback stalls and lease expiry; real-game validation is pending.

**Collecting logs:** first turn on **Settings → Audio & voice → Enable diagnostic
logging**, reproduce the failure, then use **Open diagnostic logs** and send all
`audio-diagnostics*.jsonl` files with the approximate
failure time and whether restarting Thiscord while the game stayed open helped.
The button does not wait for the audio worker. On Windows the folder is normally
`%LOCALAPPDATA%\tr.com.thiscord.desktop\diagnostics`. Logs survive app restarts.

Logging is off by default, including for existing installs without a saved choice.
The toggle applies immediately and persists in `diagnostics.json` in the app config
directory. Disabling logging stops the writer and discards queued events before
returning; existing files remain available. While off, no diagnostic writer or log
file is opened, and panic stacks are not captured for these logs. Opening the folder
does not enable logging. Backend `tracing` logs remain independently controlled by
`RUST_LOG` (default `info`).

Enabled logging is asynchronous and stored locally, with a bounded 512-record queue
and three files of up to 2 MiB each. Records include timestamps, app/platform details,
device names/formats, processing options, callback/drop counts, recovery attempts,
cleanup progress, worker watchdog state, window focus, voice lifecycle and panic
locations/stacks. Process names/PIDs appear only in best-effort conflict diagnostics.
Panic stacks can include source paths; arbitrary panic payloads are excluded.
No PCM, encoded audio, private messages, account/channel IDs, credentials, tokens,
SDP, TURN credentials or configured model-file paths are deliberately logged.
The separate opt-in WAV debug recorder is not enabled by diagnostic logging.
File writes flush per record but remain best effort if the process is killed or
storage fails; queue overflow is counted when the writer can resume.

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

### Mute and deafen shortcuts

Audio & voice → Keyboard shortcuts configures global toggle shortcuts. Defaults
are Ctrl+Shift+M (mute) and Ctrl+Shift+D (deafen). Enter a modifier/key combination
and choose Save shortcuts; an empty field disables that action. Reset to defaults
fills the default combinations for review before saving. Preferences persist in
`shortcuts.json` in the local app configuration directory. Holding a key toggles
once until released. Duplicate combinations (including aliases), invalid keys,
and the reserved Ctrl+Shift+Space PTT binding are rejected. OS registration
conflicts preserve the previous working bindings. Startup failures appear in the
shortcut settings panel. Windows, macOS and Linux/X11 use the native shortcut
plugin; Wayland requires disabling the global shortcuts and using the UI controls.

The voice bar, global shortcuts and overlay toggle the same native preferences.
Device/processing saves preserve the current native mute/deafen values, and the
main view polls them so out-of-focus actions appear on return. Deafen blocks both
capture transmission and playback; undeafening preserves your manual mute choice.
Shortcuts also work before joining voice. Backend speak restrictions remain
independent and cannot be bypassed by unmuting.

## Changing settings during a call

Model loading and replacement device opening run on one background preparation
thread while the existing media worker continues servicing capture, playback,
packets and control requests. Successful changes commit between complete processing
ticks. A device switch replaces local streams/queues only: the WebRTC connection,
Opus encoders/decoders, roster and per-user volume settings stay in place. Echo
adaptation restarts, and switching devices may produce a brief audio gap.

DeepFilterNet's tract state is not `Send`. Both denoisers are therefore initialized
on the audio worker before live playback starts and retained there for subsequent
switches; bypassed stages perform no inference. This adds initialization time and
resident memory for DeepFilterNet even when Sonora is selected. Neural echo-model
loading and movable AEC state are prepared in the background. No unsafe thread
transfer or PCM over Tauri IPC is used; device callbacks remain unchanged.

Failure to prepare/start a replacement preserves the working setup. Drivers that
cannot open another shared stream may reject a live switch; the app does not tear
down working audio to force an exclusive device open. Cancellation, leaving voice,
suspend or a session replacement prevents a late preparation from reviving audio.
Preparation has an eight-second deadline; a timed-out worker keeps the single job
slot until it finishes, preventing unbounded background work. Mute/deafen can
restrict audio immediately while a settings change is pending. Changes are saved
in order; unsupported global PTT registration rolls back to the previous shortcut.

A successful device switch finishes any debug recording because the device clocks
and formats changed. Start another recording for the new setup. Model changes are
recorded as settings events and can be reproduced by the offline replay tool.
Regression tests use fake streams to cover swap failure, stale completion, timeout,
transport/roster/volume retention and processing switches. Actual shared/exclusive
driver behavior and audible transitions still need Windows/macOS/Linux hardware
acceptance; these software tests do not establish gapless device switching.

## Libraries and compatibility decision

- [CPAL 0.18.2](https://github.com/RustAudio/cpal): native WASAPI/CoreAudio/ALSA.
  Capture and playback accept reported PCM formats from 8-384 kHz, with 1-32
  channels. Integer PCM at 8/16/24/32/64
  bits (signed or unsigned) and f32/f64 samples are converted by native callbacks.
  Mono capture and stereo playback are preferred. Multichannel capture is averaged
  to mono; playback uses the first one/two channels and silences the rest. A
  compatible default format is also considered when enumerated formats are missing.
  Rejection messages identify input/output, device name, default format and a bounded
  summary of reported formats. Native 48 kHz is preferred, followed by a compatible
  OS default and then the nearest supported rate. Selection intersects reported
  ranges with the supported bounds; it never invents a device format. This covers
  rates such as 8/16/24/32 kHz Bluetooth/voice, 44.1/48 kHz desktop audio and
  88.2/96/176.4/192/352.8/384 kHz interfaces, as well as intermediate PCM rates.
  Other rates, more than 32 channels and non-PCM/DSD formats are rejected explicitly.
  Automatic Bluetooth profile switching, device permissions/exclusive access and
  driver failures are separate from format compatibility; hardware acceptance
  on Windows/macOS/Linux is still required.
- Device-rate conversion uses a Blackman-windowed polyphase FIR, with an
  anti-alias cutoff and tap count scaled for downsampling. Filter tables and
  history are allocated before opening streams. Callbacks use bounded storage
  and work, preserving an integer rational phase across arbitrary buffer sizes.
  Native 48 kHz bypasses conversion. Capture becomes 48 kHz before its SPSC queue;
  playback converts after mixing, independently of sender-clock correction.
  Capture timestamps account for phase and filter delay. Echo/debug speaker
  reference is reconstructed from the clamped device signal at 48 kHz and timed
  to physical playback, accounting for the reference converter's delay. Filter
  histories are cleared on interruptions or playback access/generation, active,
  gain and deafen changes so delayed audio cannot cross a privacy cutoff. Clearing
  a mixed filter can briefly interrupt other mixed sources too. DSP, recording,
  queue budgets and Opus retain the 48 kHz media clock. Tests cover rate/format
  selection, signal/alias rejection, sample counts, timing, callback boundaries,
  echo-reference reconstruction and privacy cutoffs.
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
  reference. Noise suppression uses the High setting. Automatic gain explicitly
  enables adaptive digital gain, starts at unity and caps gain at 20 dB with the
  library's -50 dBFS output-noise limit; it does not change OS microphone volume.
  Echo buffering delay comes from CPAL capture/playback timestamps, mapped onto
  a common monotonic clock. AEC3 estimates/refines the acoustic delay internally.
  Hardware acoustic quality still needs acceptance, so headphones remain recommended.
  Conventional residual estimation remains the default. A vendored Sonora
  extension also supports an optional Rust neural residual estimator inside AEC3,
  independent of the selected denoiser. Enable the experimental checkbox and Echo cancellation, including during a call. Native clients include the pinned model;
  an optional local file override is available under Advanced model settings.
  See [neural echo integration](voice-isolation.md#neural-residual-echo-estimator-extension-2026-10-02)
  for setup, model provenance limits, reference tests and measured worker costs.
- DeepFilterNet3: optional native Rust/tract denoising, bundled in desktop builds.
  Select it in Audio & voice and enable Noise suppression. Both bypass and model
  changes apply during a call. Existing settings retain Sonora. See [deep-filter.md](deep-filter.md)
  for the pinned runtime/model, stage interface, tests and performance results.

The native path is selected per the project requirement. Windows and Linux
software probes run locally; CI runs the same tests and desktop builds on all
three OSes. macOS runtime, microphone consent, headset changes and acoustic
acceptance remain open. Compilation alone does not establish those results.
WebView capture was not substituted for the requested native path.

## Local voice quality

The voice bar summarizes recent local processing/buffering pressure, transport
queue drops, clipping and late/missing packets. Settings > Audio & voice >
Advanced voice quality diagnostics shows recent and lifetime counters, independent
stream slots, bounded timing percentiles and instantaneous buffer occupancy.
See [metric definitions, scopes, denominators and limitations](audio-quality.md).
Empty active PCM queues alone no longer count as playback starvation. Deafen,
inactive slots, zero gains and explicit DTX are intentional silence; empty
active queues without buffered-media or silence evidence are unclassified. These
are not device xrun counters.

Metrics stay in memory when logging is off. Opt-in logs add numeric session and
per-stream snapshots every ten seconds and at session end, split to stay within existing record
bounds. No account/transport identifiers, audio or model paths enter these records.
WebRTC RTT and receiver loss reports are explicitly SFU-hop observations; they
cannot establish speaker-to-listener loss or delay. FEC recovery and limiter
activity are unavailable; Opus FEC attempts and explicit PLC samples are counted
separately, and output hard-clips.

Outgoing native packets carry capture time, media position and connection/control
generation. The five-packet queue rejects full sends, applies the capture-age budget
described below (including the capped activation allowance), and invalidates queued
packets across control changes or connection replacement. Counts distinguish
these outcomes from successful WebRTC submission.
AEC3 processing, configuration and reference timestamps are unchanged.

## Audio timing and performance

Voice activation uses sustained energy after denoising and microphone boost, before AGC, with
40 ms attack, separate opening/closing thresholds, 200 ms release and a fixed
60 ms pre-roll delay. PTT bypasses this detector and delay. Existing persisted
sensitivity/defaults remain supported. See [voice activation](voice-activation.md)
for scheduling, control boundaries, detector evaluation and synthetic comparisons.
The post-mix AEC reference and its timestamps/tuning are unchanged; closing the
automatic gate never resets echo adaptation.

Capture is averaged to mono, processed at 48 kHz and encoded as 20 ms Opus frames
at 32 kbit/s. Capture callbacks only convert/downmix/resample samples and update atomics.
Capture and post-mix reference callbacks assemble timestamped 10 ms blocks into
preallocated 120 ms SPSC rings. Partial callbacks retain their samples. The worker
drains complete capture packets instead of discarding audio from callbacks longer
than 60 ms. Work per tick stays bounded, and stop/PTT release are checked while
draining. Lost blocks carry a sequence gap that restarts DSP adaptation; playback
reference overflows are counted too. Callbacks allocate no memory; enhancement
DSP and codecs run off the callbacks. Device-rate FIR conversion uses prepared
coefficients and bounded history. The processor is constructed before starting either device.

### Microphone sender timeline and expiry

`frames::Frame.at` is the first sample's CPAL capture time mapped to monotonic
`Instant`. Each input stream anchors its device clock to `Instant` on its first
callback, then maps capture timestamps relative to that fixed anchor. Subsequent
callbacks never remap capture time against their invocation time: preemption or
batched delivery would otherwise become false forward gaps, DSP/activation resets
and inflated RTP positions. Delayed buffers retain their original capture age;
opening a replacement stream establishes a new anchor. Only timestamps from the
same CPAL stream are compared. Subsequent samples in a callback use exact 48 kHz
offsets. Capture blocks also carry a device-stream sample position, advanced even
when the 120 ms ring is full. A forward callback-time hole greater than 2 ms adds
the rounded missing sample count. Differences within 2 ms follow device-clock drift and mapping
jitter. A timestamp more than 2 ms behind the expected end of the previous callback
uses that expected time instead: repeated/backward callback timestamps must not move
the continuity baseline backward and manufacture a hole when they catch up. This
prevents repeated DSP/activation resets and inflated RTP timestamps when 10 ms
capture buffers arrive with paired timestamps. Real forward holes and queue
overflows still preserve missing sample positions and reset processing. A partial
10 ms block crossing a hole is discarded. Capture and reference rate conversion
retain fractional phase between callbacks and compensate timestamps for their
filter delay. The existing 48 kHz path retains its direct reference assembly.

DSP preserves block timestamps and sample positions. It never combines two halves
across a gap into one 20 ms Opus packet: the earlier half is discarded and the later
half retained until a contiguous partner arrives. The existing gap reset then
clears DSP history. There is at most one retained 480-sample block beyond the ring.
Normal packets advance by exactly 960 samples. The session maps these positions
to one 48 kHz media clock before queue admission, so discarded packets still occupy
time. Device recovery/reconfiguration bridges new device positions using elapsed
monotonic capture time from the session origin, clamped beyond the previous packet.
A replacement connection starts a fresh clock; neither wall-clock nor dequeue time
sets RTP timestamps.

`audio::outgoing::EncodedFrame` is native-only: encoded payload, 64-bit media
position, oldest capture time, control generation, connection identity, delivery
lease, talkspurt marker and fixed activation delay. RTP
uses the low 32 bits of media position, preserving wrapping. Sequence numbers wrap
independently at 16 bits and advance only after WebRTC accepts a write. A frame
discarded before admission consumes media time but no sequence number, including
a pending write cancelled by a control boundary or expiry.

RTP positions describe output slots on the capture clock. Explicit enhancement
buffering delays the signal within those slots rather than retiming the clock on
model changes. For age/control checks, the oldest capture time subtracts the active
enhancer's declared buffering (DFN3: 1,440 samples / 30 ms) and the activation
lookbehind (2,880 samples / 60 ms in automatic mode, zero in PTT). Frequency-dependent
filter-bank/AEC phase delay and Opus lookahead are not exact capture-age estimates;
these remain part of acoustic latency validation. No echo algorithm was replaced
or retuned.

The outgoing queue holds five packets plus at most one pending transport write.
The capture-age budget is **100 ms plus the fixed activation delay**: 160 ms in
automatic mode, 100 ms in PTT. The 60 ms allowance is capped and does not grow with
queue occupancy. This leaves the same acquisition/DSP/transport budget in either
mode; DFN3 with automatic activation normally needs 20 + 30 + 60 = 110 ms before
enqueue. Packets reaching the budget are discarded before enqueue, at dequeue,
and before every poll of a pending WebRTC write. The same deadline
cancels blocked admission. Thus a stalled sender drains expired entries without
replaying an arbitrarily old backlog; expiry alone does not disconnect the call.
Transport errors retain the existing disconnect behavior. Work and memory stay
bounded by capture/packet capacities; this does not guarantee downstream WebRTC,
network or receiver latency after admission.

Urgent stop, PTT release, mute/deafen and settings/session changes invalidate the
generation independently of the command queue. Relaxing a restriction cannot
revalidate old frames. A monotonic capture cutoff also prevents queued PCM and
explicit enhancer delay from being relabeled as fresh speech after unmuting.
Opus history resets on generation changes and on the transition to silence.
Fresh muted/PTT-gated frames continue to encode silence; the microphone DTX
policy below suppresses eligible packets after encoding.
Connection identity and active-state checks reject closed/replaced peers before
each transport poll. Gate gaps and device/session resets also close a delivery
lease, invalidating pending pre-gap audio. A control notification wakes blocked writes; packets already
admitted to WebRTC cannot be recalled. Backend permissions remain authoritative.

Worker diagnostic snapshots include `outgoing.queue_full`, `outgoing.expired` and
`outgoing.control_invalidated`, cumulative per audio-engine lifetime. Each rejected
encoded packet increments one reason (invalidation takes precedence over expiry).
They contain no audio or connection identifiers. Recording's `accepted` flag still
means outgoing queue admission, not final network delivery.

Native receive playout now consumes these timestamp holes, including sender-side
drops with consecutive sequence numbers. The same receive path serves mono shared
audio. Physical stalls, device drift and listening acceptance on Windows/macOS/Linux
remain unverified by deterministic sender/receiver tests.

### Playback timing

Each received speaker owns a separate bounded packet jitter buffer, Opus decoder,
PCM ring, rate converter and gain. Startup initially targets 40 ms; arrival
variation adapts the combined packet/PCM target within 40-120 ms. The native media
command preserves RTP timestamp and monotonic arrival time from the transport,
before the bounded audio-worker queue. Only 20 ms Opus packets are supported;
invalid packet headers and unsupported durations are rejected before clock admission. RTP
timestamps advance a 48 kHz sample cursor, independently of sequence numbers.
Reordering/deduplication handles both timestamp and sequence wrap; late, overlapping
or contradictory packets cannot rewind consumed audio. A long media jump can
re-establish sequence ordering after the 16-bit half-range (for example a long
deafen); the timestamp floor still rejects old audio.

Short timestamp holes produce at most 60 ms of consecutive concealment. Whole
20 ms slots use the source's Opus FEC/PLC state; fractional holes use exact-length
silence, since Opus PLC cannot represent arbitrary sample counts. Missing sequence
numbers alone do not add silence if timestamps are contiguous. Missing media and
FEC/PLC never refresh speaking indicators. Decode errors clear potentially modified
decoder history and use bounded PLC, with silence if that fallback also fails.

Each source retains at most 12 packets of 1,500 bytes. Queue age of **200 ms**,
a worker stall above **100 ms**, more than **60 ms** of unknown missing media, a buffered
timestamp span beyond **240 ms**, or queue pressure triggers recovery. A responsive
worker also invalidates decoded PCM when arrivals have stopped for 200 ms. The receiver discards stale entries and reanchors at the newest
40 ms of buffered media (at most three packets), rather than replaying an entire
interruption or synthesizing seconds of concealment. Arrival age is measured from
transport delivery, not worker dequeue. It cannot measure time already spent in
the remote transport/network or establish an absolute sender-to-listener age bound.

Recovery resets that source's decoder/activity/rate converter and requests a PCM flush through
its existing atomic mixer generation. The callback acknowledges after discarding
old PCM; the worker waits for that acknowledgement before writing fresh decoded
audio. Other sources and gains are preserved. Deafen transitions and device changes
also clear pending receive audio, retaining consumed sequence/media watermarks.
After six hours without accepted media, a fresh packet starts a new local RTP
epoch with an acknowledged PCM flush. This avoids the signed timestamp comparison's
12.4-hour ambiguity after an overnight deafen; receive-time fences still reject
older queued audio. Short interruptions preserve their existing replay protection.
A replacement source owns fresh decoder/jitter state and rejects media that was
already in the local dispatch queue before its roster entry. Existing connection
identity checks still reject old connection handlers. This does not add a new wire
source epoch: packets from a replaced publisher delayed upstream of transport
delivery still depend on the SFU's existing routing and RTP continuity guarantees.

Audio diagnostic snapshots include the `processing_resets` total and
`processing_reset_causes` counters for capture gaps, reference gaps and processing
reconfiguration. These worker-side counts explain adaptation restarts without
recording audio or device timestamps. Device activation/replacement also increments
the total and is logged separately as `device_active`. Reference/capture queue overflow still
requires a reset; the clock mapping does not hide lost samples.

Audio diagnostic snapshots retain per-source `receive` counters and add `playout`
metrics for target/current delay, arrival variation, occupancy, late/duplicate
and stale discards, rebuffering, concealment, drift and correction. No payloads or
source identifiers are logged. See [adaptive playout](audio-playout.md) for the
estimator, hysteresis, bounds, measurements and remaining listening acceptance.
AEC algorithms, the final post-mix reference and its timing remain unchanged.

The worker sizes decoded PCM refills from the largest observed device callback,
with a 15 ms minimum and a 12 ms worker allowance above one callback. The existing
120 ms PCM ring remains fixed; refill work is bounded to two through six decode
decisions per stream per tick. Missing packets wait until at most 5 ms remain,
or until a callback is imminent and cannot be filled. The minimum receive target
and callback safety trim accommodate this cushion; see [the exact bounds](audio-playout.md).
A bounded streaming sinc converter corrects sender/output drift before mixing, preserving
phase across full packets and fractional media gaps. Tests cover irregular worker
wakes, packet and sample continuity, FEC/PLC, long virtual calls, final AEC samples,
queue pressure, stalls, flush acknowledgement, deafen and source replacement.
Actual device scheduling, acoustic quality and prolonged stalls still need
three-platform live validation.

### Adaptive microphone FEC

**Audio & voice > Advanced & troubleshooting > Adaptive Opus in-band FEC**
(`adaptive_fec`, default false) persists in `audio.json` and uses the existing
local and remote settings path. It takes effect during a call without rejoining.
Off restores the existing 5% hint; in-band FEC itself remains enabled. The false
field is omitted on serialization, preserving the old default settings shape.
Older remote clients may reject an enabled setting; the UI identifies missing
adaptive diagnostics and instructs updating that desktop client.

The sole live loss-hint writer is `transmit::Encoder::adapt`. A narrow Rust
interceptor observes authenticated RTCP Receiver Reports (and reception blocks
in Sender Reports) before the transport's default RTCP terminus. Only the
publisher microphone SSRC qualifies. Listener statistics, accepted-write counts
and RTP timestamp gaps are never used as delivery loss. Pinned rtc 0.21 defines
its remote stats updater but never calls it; polling `RemoteInboundRtp` would
leave this feature inert. The encrypted integration test exercises the direct
interceptor. No custom signaling, payload type or server upgrade is needed;
a peer without usable standard reports stays at baseline.

A fixed numeric mailbox accepts at most one advancing report per second. It
rejects future sequences, reports before the stable source's first sent packet,
duplicates, reordered reports and unusable 24-bit loss values. The transport
records successfully submitted **outer RTP** sequences; RED copies and raw/RED
switching must not create extra receipt counts. The controller uses deltas in
extended highest sequence and cumulative loss. A window needs at least 25
packets, at least one second, less than six seconds and at most 400 packets.
Sequence wrap is modular; loss-counter regression or implausible jumps rebaseline.
This is the server's transport estimate, not proof of audible or permanent loss.

The bounded 5-30% hint uses a 0.25 EWMA, one point of hysteresis, and at most
+3/-1 percentage points per accepted window. Six seconds without a usable window
restores baseline; replayed reports cannot refresh it. Sparse DTX feedback stays
at baseline because omitted silence advances media time, not sent sequence.
Reconnect clears all state. PTT, mute/deafen and other privacy revisions preserve
numeric path estimates; existing generation fences and Opus resets still erase
all retained audio.

`Connection::uplink_sent(source_generation, sequence)` and the worker's matching
`uplink_sample(source_generation)` are the stable-source integration hooks.
Current peers have one source, generation 0. A future authoritative stable source
change must change both arguments; passing the new generation to the getter
immediately rejects prior-source feedback. Never pass the PTT/privacy revision
or `EncodedFrame::generation` here. The first successful new-source send clears
the mailbox and establishes its sequence fence. Within one peer/SSRC this assumes
continuous RTP sequence numbers; a sequence restart requires a fresh peer or an
explicit matching transport/report reset.

For DRED integration, pass **floor 20 when `deep_redundancy` is enabled, otherwise
5** to `Encoder::adapt` immediately before encoding, even when adaptive FEC is
off. That method owns the final `OPUS_SET_PACKET_LOSS_PERC` call; DRED must not
write it afterward. Off/absent/stale/reset/low-loss states all honor the floor,
and floor changes apply immediately. DRED separately owns its 40 kbit/s total
codec budget; adaptive FEC never changes bitrate. This branch implements neither
DRED nor RED. Parent integration must test all eight combined toggle states and
update combined UI bitrate/fallback copy.

Adaptive FEC adds no packets, retained PCM, buffering delay or codec budget:
the microphone remains 32 kbit/s VBR. Reallocated bits can reduce speech detail;
packet sizes still vary. Interception retains ordinary pass-through packets only
during synchronous chain traversal, with a 1024-packet cap per direction. It
scans at most 32 RTCP packets by 31 report blocks, and never logs media, reports,
credentials or identities. Feedback storage is under 256 bytes. Device callbacks
are unchanged. Tests cover bounded ingress, source/SSRC rejection, replay, rate
limits, counter resets, wrap and floor precedence.

The committed synthetic speech fixture with pinned Opus 1.6.1 and deterministic
approximately 20% loss produced these Windows results. Adaptive encoding was
warmed to a 20% hint; this is not a listening score or real-network convergence
claim. Lost-frame squared error against aligned input:

- Isolated loss: fixed 5% FEC 237.834; adaptive 20% FEC 196.568 (17.4% lower).
  Both had 71 missing intervals and 71 immediate-successor opportunities.
- Three-packet bursts: fixed FEC 565.231; adaptive FEC 552.042 (2.3% lower).
  Of 72 missing intervals only 24 had a successor; the other 48 required PLC.
- Payload: 25,445 vs 25,483 bytes, 27.58 vs 27.62 kbit/s (+0.15%). Minimum RTP
  headers are unchanged (50 times 12 bytes/s); UDP/IP/SRTP/TURN is excluded.
- Encoder lookahead was 6.5 ms in both cases; added buffering was 0 ms.

All eligible FEC outputs differed from a separate PLC decoder, but divergent
codec histories and Opus's implicit PLC mean these are **not verified recovery
counts**. The encrypted two-peer test must observe real reports and change the
encoder above 5% before passing. Reproduce with
`cargo test -p thiscord-frontend --lib --features native-audio --profile ci --locked adaptive_fec -- --nocapture`.
Physical multi-client impairment/listening and native macOS/Linux device
acceptance remain unverified. Existing CI runs these native tests on all three
platforms; no new dependency or CI feature is required.

### Optional microphone packet redundancy

Advanced audio settings offer default-off negotiated RFC 2198 RED, repeating up
to two bounded recent microphone packets. Updated connections also use
authenticated privacy generations across signaling/RTP; control boundaries briefly
pause microphone transmission and listening for a network round trip. See
[packet redundancy](packet-redundancy.md) for compatibility, limits, DRED/padding
combination policy and deterministic recovery/bandwidth results.

### Opus microphone FEC policy

The optional experimental [deep redundancy policy](deep-redundancy.md) adds
bounded Opus DRED recovery and documents its source/privacy protocol merge gate.
It is separate from the default ordinary FEC behavior below.

Microphone encoding retains 48 kHz mono, 20 ms frames, `Application::Voip` and
32 kbit/s. It explicitly sets `set_inband_fec(true)` and
`set_packet_loss_perc(5)` by default. This conservative **fixed 5% expected-loss
hint** remains the disabled/fallback policy. The optional adaptive policy below
uses authenticated server reception reports; it is off for existing and new installs.
Redundancy spends part of the existing bitrate budget on the preceding speech
frame, reducing bits available for the current frame and potentially changing
bandwidth/mode selection. VBR packet sizes still vary. FEC is conditional on the
codec's mode, available bits and signal; it is not present in every packet.

The reviewed lockfile pins `opus 0.4.0` / `opusic-sys 0.7.5`, whose bundled
`opus/package_version` identifies libopus **1.6.1**. In the wrapper's `src/lib.rs`,
the boolean FEC setter sends `OPUS_SET_INBAND_FEC(1)`, and the loss setter sends
`OPUS_SET_PACKET_LOSS_PERC`. The bundled `src/opus_encoder.c` accepts FEC values
0..2 and loss percentages 0..100. The wrapper exposes only modes 0/1; mode 2's
music-preserving preference is not requested. See the official
[encoder controls](https://opus-codec.org/docs/opus_api-1.6/group__opus__encoderctls.html).
Shared system audio retains its separate `Application::Audio`, 64 kbit/s encoder
and default disabled FEC/zero loss hint; the microphone speech policy is not
applied to music. All native receivers can opportunistically decode redundancy.

The jitter decision is normal packet, FEC from the immediate media successor,
concealment (PLC or fractional silence), or wait. Until the callback-aware decode
deadline described above, a missing original can still arrive.
At that playout deadline, an available successor is cloned by reference
and decoded with `fec=true` into exactly 960 samples. It remains buffered for
normal decoding exactly once, after the missing interval. The media cursor advances
once, so a late original cannot replay the recovered interval. FEC requires both
a successor exactly 960 samples ahead and a sequence hole after the last decoded
packet. Consecutive sequence numbers with a timestamp hole may indicate capture
discontinuity rather than a lost encoded interval, so they conservatively use PLC.
Known DTX gaps use silence instead. Neither a talkspurt-marked packet nor an Opus
no-data packet can donate FEC; no FEC is attempted while in DTX silence.
Fractional holes use exact silence and cannot borrow a full FEC frame. No extra
wait is introduced for FEC. With burst loss, earlier gaps use PLC; only the gap
immediately preceding an available packet can use that packet's redundancy.
The adaptive startup buffer, 12-packet cap, 60 ms consecutive-concealment limit and maximum
six decode decisions per stream per worker tick remain bounded.

The [Opus decoder API](https://opus-codec.org/docs/opus_api-1.5/group__opus__decoder.html)
requires the exact missing duration and serial decode order. It may return PLC
successfully when FEC is absent (also confirmed in bundled `src/opus_decoder.c`).
Empty or non-20-ms packets are rejected before jitter admission. Malformed encoded
frames admitted with a valid 20 ms header use explicit PLC. A decode failure resets
potentially modified decoder state before one bounded PLC fallback; a failed PLC
produces one silent interval. No codec work runs in device callbacks. Recovered
audio uses the existing per-source PCM queues, generation invalidation, volume,
deafen and post-mix AEC reference. Only successfully decoded **normal** packets
refresh remote activity; FEC attempts cannot extend an indicator using PLC.

Opt-in local `audio_snapshot` logs include per-live-decoder `opus_decode` counters:
normal frames, FEC attempts, successful FEC-call output frames, explicit PLC
frames, decode/validation errors and silent fallback frames. Counters reset with
source replacement. `fec_output_frames` includes implicit PLC and is **not a
verified recovery count**; `verified_fec_frames` is explicitly null because the
API used here cannot establish that count. No payloads or speaker identities are
included. Existing `useinbandfec=1` SDP remains appropriate: it advertises receiver
capability/preference, not guaranteed encoder redundancy, including for shared
audio ([RFC 7587](https://www.rfc-editor.org/rfc/rfc7587.html#section-6.1)).

`audio::codec::tests` encodes the committed synthetic speech WAV with the production
policy, compares aligned dropped intervals with a PLC-only decoder, and requires
FEC output distinct from PLC. The deterministic transport/playout simulation covers
isolated and burst loss, duplicates, reordering, sequence wrap and irregular worker
wakes through the actual remote decoder/mixer. Additional jitter and codec tests
cover deadline waiting, absent redundancy/successors, late packets, invalid packets,
source reset, deafen and generation invalidation. These are software regressions,
not evidence of acoustic quality or real-network latency. Listening, microphone
quality/bitrate tradeoffs, real loss/jitter trials and Windows/macOS/Linux hardware
acceptance remain pending.

Playback mixes on the device callback with atomic gain changes. It allocates no
buffers, waits on no locks, and does no networking/codec/DSP work. Queues cap
latency rather than accumulating stale speech; underruns produce silence and
overflows are counted. Output clips to [-1,1]. The mixer supports 32 preallocated
queues; the initial room limit is eight participants. This is a bounded initial
deployment, not a measured promise of production capacity.

Audio settings show raw and processed microphone peaks (before mute/PTT gating)
and processing reset counts. To check cancellation, enable it on the person using
speakers, let the other person talk for several seconds, and compare levels while
the speaker user stays quiet. Then test both people talking simultaneously. Echo
cancellation only has a reference for Thiscord playback, not other applications.
Room acoustics, speaker distortion, microphone clipping and device clock drift
still need real-device validation; synthetic tests are not a guarantee for them.

The expandable **Echo diagnostics** section shows playback-reference peak,
estimated filter reduction/delay, percentage of full-scale mono input samples
over the last completed second, and automatic-gain state. Filter reduction is
AEC3's internal linear-filter estimate, **not** a measurement of total audible
echo or a guarantee of cancellation. Estimates are withheld during initial warmup
and when the playback reference is silent. Restarting adaptation clears these
measurements. These metrics contain no speech. PCM never crosses IPC or logs;
the separate, opt-in recording tool below writes local files only.

For echo that mainly appears when both people talk, compare diagnostics while
only the remote person speaks and while both speak. Voice activation can mask
residual echo while the microphone user is quiet, then transmit it alongside
local speech when the gate opens; this symptom alone does not prove the filter
has stopped working. Also compare with automatic gain disabled on the speaker
user's client. The synthetic double-talk probe has not reproduced the reported
failure, so gain is a hypothesis, not an established cause.

Run `cargo run -p thiscord-frontend --example audio_bench --features native-audio
--release --locked` for a synthetic eight-stream mixer timing. This excludes
device latency, DSP, codecs and network costs. The local WSL release run measured
0.021 ms per 20 ms block over 1,000 blocks; this is a synthetic local result,
not an end-to-end latency or SFU capacity measurement. Stop and push-to-talk
release signals use atomics so a saturated media queue cannot lose them.
At about 60 kbit/s per encrypted
audio stream including packet overhead, eight active speakers imply roughly
3.4 Mbit/s SFU egress (8 × 7 streams), plus signaling/RTCP.

### Microphone DTX and media timing

The microphone retains the FEC policy above and explicitly enables Opus DTX;
shared music/system audio explicitly disables DTX. The pinned `opus 0.4.0` wrapper
exposes `set_dtx` and `get_in_dtx`, backed by `opusic-sys 0.7.5` / libopus **1.6.1**.
The [official encoder API](https://opus-codec.org/docs/opus_api-1.6/group__opus__encoder.html)
and bundled `opus/include/opus.h` permit omitting encoded results of at most two
bytes. This is a documented codec rule, not an arbitrary size threshold. The
[DTX-state query](https://opus-codec.org/docs/opus_api-1.6/group__opus__genericctls.html)
also includes required comfort-noise updates; those larger packets must be sent.
The worker feeds every 20 ms frame and only suppresses results meeting both rules.
Capture, render-reference processing, AEC, denoising and gain continue throughout
silence. AEC configuration and reference timing are unchanged.

DTX is disabled for gate-admitted audio, including the improved gate's **60 ms
pre-roll**, so codec VAD cannot discard accepted quiet speech. PTT retains its
immediate path without pre-roll. No additional gate delay is introduced by DTX.
Control generations, capture cutoffs, delivery leases and pending-write deadlines
above remain authoritative; relaxing mute or PTT cannot replay retained audio.

One eligible no-data packet is sent after **each** comfort-noise update. This
small extra packet explicitly cues silence to the receiver and repeats the cue
if an earlier one was lost. Bundled `src/opus_decoder.c` confirms that payloads
of zero or one byte after the TOC trigger PLC/DTX; receive admission first checks
the 20 ms Opus header. All intervening eligible packets are omitted before payload
allocation/queue admission. Sparse CN packets still decode normally, and the mixer
supplies zero between updates. This trades some maximum packet savings for a
receiver that need not infer silence solely from absent traffic.

The adaptive 40-120 ms receive target and 12-packet bound remain. On a decoded
silence cue, empty intervals stop generating PLC, drift integration or callback
underrun alarms. Rendered silence still advances the media watermark, rejecting
late packets without consuming sequence numbers. Sparse updates/resumption retain
their media timestamps and use
the current receive target without a repeated recovery/flush cycle. A buffered
resumption after a cue also preserves its intervening timestamp gap. Worker stalls,
packet age and stale decoded PCM still trigger the existing bounded recovery.
After one second without media the source is also marked inactive; signaling and
roster identity, not audio absence, determine disconnection. If a cue is lost,
the ordinary unknown-gap path allows at most 60 ms PLC until a later cue establishes
silence. It never fabricates continuous packet loss throughout an idle interval.

Resumed speech carries the RTP talkspurt marker. It clears silence mode and cannot
be borrowed as FEC for preceding silence. Two consecutive data frames also exit
silence mode, so losing that marker cannot strand a talkspurt. Ordinary subsequent
speech loss still uses FEC under the adjacent-timestamp/sequence-hole rules above.
Sequences advance on successful transport admission, while the 48 kHz media clock
advances through every omitted frame, as required by
[RFC 7587](https://www.rfc-editor.org/rfc/rfc7587#section-4.1).

Native leases, 500 ms signaling/state heartbeats and WebRTC ICE/RTCP traffic stay
independent of microphone packets. The two-hop encrypted loopback test resumes
after **31 seconds with no RTP**, beyond normal update gaps. This does not establish
public NAT/TURN or multi-hour behavior. Deploy these clients together: older
sequence-only receivers cannot play DTX gaps correctly, and older adaptive clients
lack the silence cue handling. Room-wide client-capability negotiation is absent.

Opt-in diagnostics record cumulative encoder frames/bytes/time, omitted packets
and queue admission/drops at session end. Every five seconds transport diagnostics
report accepted packets/payload bytes and the minimum 12-byte RTP header cost.
Acceptance is not remote delivery. Existing outgoing counters separate expired,
invalidated and full-queue drops. Receive metrics distinguish DTX, inactivity,
observed sequence holes, late packets and silent media gaps (which can also include
lost updates). No payloads or identities are logged. Counts exclude SRTP, UDP/IP,
RTCP, ICE and TURN overhead; full wire cost needs a packet capture. Recording's
`accepted:false` also includes DTX omission; its encoder-input WAV retains every
frame and the gate delay.

```sh
cargo run -p thiscord-frontend --example dtx_bench --features native-audio --profile ci --locked
```

Windows x64 / i7-13700K / Rust 1.98.1, optimized `ci` profile, 2026-10-13 after
merging main's FEC/gate/adaptive playout. Each synthetic case represents 60 seconds.
These are codec-loop wall times, not whole-application CPU or acoustic measurements:

| Input / policy | Packets | Payload bytes | Minimum RTP header bytes | Encode time |
| --- | ---: | ---: | ---: | ---: |
| Silence / continuous | 3,000 | 62,996 | 36,000 | 507.3 ms |
| Silence / DTX | 295 | 3,331 | 3,540 | 506.1 ms |
| Alternating tone and silence / continuous | 3,000 | 64,243 | 36,000 | 276.4 ms |
| Alternating tone and silence / DTX | 1,110 | 58,333 | 13,320 | 280.3 ms |

Silence traffic falls from 50 to 4.92 packets/s (**90.2% fewer packets**); codec cost
is essentially unchanged in this run. DSP still runs continuously. Compared with
the pre-merge benchmark, the repeated silence cue intentionally spends an extra
packet per update; FEC also changes payload sizes. The receive regression simulates
a minute with lost cues/updates, both RTP wraps and quiet/abrupt onsets. It detects
resumed synthetic speech within 100 ms even when the first onset packet is lost,
without repeated silence rebuffering/underruns; that bound excludes capture, DSP,
the 60 ms automatic gate delay and physical devices. Tests also retain the main
branch's FEC, drift, pre-roll, queue expiry, control and AEC regressions.

Merge verification on Windows uses the pinned toolchain, committed lockfile and
configured shared compiler directory: **161 native audio tests** and **173 tests
with DeepFilterNet3/neural-echo-probe** pass, including long-silence transport,
late-packet rejection and queued/buffered resumption. The native desktop and WASM
`thiscord-ui` builds, audio/desktop Clippy with warnings denied, and formatting
checks pass. Backend/shared behavior and database state are unchanged.

Physical listening, first-syllable preservation, real network/relay loss, long-call
drift, battery/thermal cost, full wire savings and Windows/macOS/Linux runtime
acceptance remain open. Compilation and synthetic tests do not establish them.

## Echo debug recordings and offline replay

Since client 0.1.8, join a voice channel, open **Settings > Audio & voice**, and
select **Start debug recording**. The control explains that raw microphone audio
is captured even while muted and includes other participants' playback. Tell
participants before recording. A red recording/stop control remains visible in
the voice bar when navigating away from settings. Use **Stop and save**, or wait
for the automatic 60-second limit, then **Open recording folder**. Share the entire
folder manually; nothing is uploaded automatically. Files remain under Tauri's
application-local-data `audio-debug/echo-...` directory until manually deleted.

| File | Signal |
| --- | --- |
| `speaker-output.wav` | Thiscord's mono playback mix after per-speaker/master volume, clipping and deafen, before device conversion and OS volume. It is not system-wide audio or a recording of the physical speakers. |
| `microphone-input.wav` | Mono CPAL microphone input before Thiscord DSP, gain, voice activation, mute and PTT. OS/hardware processing may already have occurred. |
| `transmit-input.wav` | Microphone signal after all Thiscord processing and mute/PTT/VAD gating, as presented to Opus. It is not the decoded SFU/listener signal. |
| `timeline.jsonl` | Versioned settings/device-format metadata, processing order, original timestamps, dropped-input gaps, capture delay, transmit-gate state and transport-queue acceptance. |
| `README.txt` | Sharing and interpretation instructions. |

All WAVs are 48 kHz mono 32-bit float, padded to a common start/end, with a maximum
of about 35 MB of PCM per recording. Continuous samples are preserved despite
callback timestamp jitter; the original per-block timestamps expose input/output
clock drift. Sequence gaps produce silence padding where possible and are flagged.
Each block's `offset`, `count` and `prefix` locate its original 480-sample frame
within the WAV. Timing metadata preserves the worker's render/capture order so
future processing can consume exactly the recorded reference and microphone blocks.
`gap` marks missing input on that stream; the separate `reset` marker identifies
when the DSP actually restarted, including gaps in the second half of a packet.
`accepted` means the voice transport queue accepted an encoded packet, not that
the server or remote participant received it. Audio rejected by that queue remains
in the diagnostic encoder-input WAV, with `accepted: false` in the timeline.

Device callbacks are unchanged: the audio worker taps their existing bounded
queues and copies blocks into a separate 256-event diagnostic queue. File creation,
WAV encoding and disk writes run on a dedicated thread. Disk errors/queue saturation
stop the recording and report an incomplete result without stopping voice. Recording
stops on leave, logout, session replacement, suspend/inactivity or device failure.
Stop recording uses an atomic signal as well as the command queue. Normal application
exit allows finalization; WAV headers are checkpointed every second during activity.
A forced kill may leave incomplete files. Missing `end` metadata or `complete:false`
must not be treated as a complete regression fixture. Unix recording directories
are mode 0700 and WAV/metadata files 0600; Windows uses the user's app-data ACLs.
Device IDs, model paths, account/channel IDs, tokens and packet contents are omitted.

For a useful sample, let the remote person talk alone for 5–10 seconds, then talk
simultaneously and reproduce the problem. Recording starts with the live DSP already
adapted, whereas replay starts with fresh state; discard the warmup when comparing.
These files preserve the acoustic signals from that call, but cannot recreate future
room changes, device behavior, OS volume, codec effects or networking. Different
denoisers may also add different delays; align comparisons accordingly.

Replay with the recorded settings, or override the estimator/denoiser:

```powershell
cargo run -p thiscord-frontend --release --locked --features deep-filter,neural-echo --example audio_replay -- C:\path\echo-recording C:\path\baseline.wav
cargo run -p thiscord-frontend --release --locked --features deep-filter,neural-echo --example audio_replay -- C:\path\echo-recording C:\path\candidate.wav --neural-echo on --noise-suppression deep_filter_net3
```

Overrides accept `--neural-echo on|off` and `--noise-suppression off|sonora|deep_filter_net3`.
Replay preserves the recorded transmit gates and refuses to overwrite output files
or accept incomplete/oversized input. Change the Rust processing modules to evaluate
future algorithms against the same inputs. Listening and speech-preservation checks
are still needed; a lower output level alone does not establish better cancellation.

New recordings use timeline schema 2. Transmit blocks include
`source_delay_samples` (2880 for automatic activation, zero for PTT); their timestamps
stay on the current capture/output schedule. `capture_boundary` events record the
enhancement-history reset and capture cutoff without resetting AEC. Updated replay
supports schemas 1 and 2, applies the source delay and excludes old device blocks
from enhancement. Retained samples predating the recording cannot be reconstructed
and replay as silence. Earlier replay binaries reject schema 2.

CI includes synchronized WAV/metadata, privacy-field omission, gap/timestamp handling,
duration bounds, writer failure, queue overload, finalization and replay/gate tests.
Actual microphone/speaker and macOS GUI/shutdown acceptance require their native hosts.

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
and 1500 bytes/packet. Independent outgoing microphone, video and shared-audio
writers isolate application backpressure. Audio queues have 32 packet slots,
32 KiB byte budgets (including in-flight writes) and 100 ms enqueue-to-write
deadlines. Expired microphone packets are dropped, including expiry while waiting
for room access or transport admission; expiry alone does not end membership.
Explicit microphone transport errors still disconnect. Screen writes never wait
for transport admission or deactivate voice on failure; video losses request
keyframe recovery. See [screen-sharing.md](screen-sharing.md)
for video budgets and the limits of the shared WebRTC transport.

Current sessions are rechecked every second and immediately after matching access
changes. Each participant registers its account, session and guild before an
authoritative database check. Account-wide changes affect that account; rotation,
replay and logout affect one session; member changes affect that account in one
guild; role/channel changes affect that guild. Other connections retain their
authorization epochs and keep forwarding without reauthorization.

Mutation guards live on the blocking database worker through commit or rollback,
even after HTTP cancellation. They close matching media admission and drain
in-flight application writes before commit. Database checks overlapping a matching
mutation retry; newly registered connections inherit active mutations. Session and
guild checks use authoritative database state and compatible shared row locks.
Lost session/channel access or a changed Speak grant closes affected connections.

Sender and receiver epochs are independent. Viewer admission, bitrate votes,
keyframe feedback, queue admission and each transport poll validate both endpoints
against their own grants. Media remains valid with unequal epochs. Queued packets
retain both original epochs and are discarded after either endpoint changes, even
if that endpoint subsequently reauthorizes. Screen identity, share epoch, viewer
lease and ingress timestamp checks also reject stale subscriptions and packets.
No media permit survives a pending transport poll. Each receiver gets only its
channel's streams; own audio is not looped back. Self mute/deafen is enforced by
the SFU. This assumes a single backend process.

After negotiation, transient signaling failures recover independently of media.
The native signaling worker drops only the failed WebSocket and sends authenticated
`Resume` requests using the secret capability supplied in the original Offer.
It never sends Join. The same peer, RTP tracks, audio devices, native audio
heartbeat, mute/deafen processing and screen capture continue running. The UI
shows "Recovering voice signaling..." while keeping the room connected.

The backend keeps detached media for up to 30 seconds after detecting transport
loss (or after its 20-second signaling silence detector fires). Access refresh,
revocation and publisher/receiver authorization continue during that grace period.
Resume checks the account token, current guild/channel permissions, original
session access, media capability and an increasing sequence number before handing
one bounded socket to the media owner. The owner rechecks access and membership
before acknowledging. A new socket can supersede an apparently healthy old socket;
this covers a proxy losing only its client-facing leg. A lost acknowledgement can
be retried with a higher sequence. Stale requests cannot evict an acknowledged
newer socket, and a replaced call cannot resume into its replacement's media.
Capabilities stay in memory and are never logged. Resume has a separate rate limit
of 30 authenticated requests per account per minute.

The client attempts immediate signaling recovery, then retries with roughly
0.5-1 second backoff and jitter, with a five-second limit per attempt and a
20-second overall deadline. Resumed is followed by fresh participant snapshots;
the client resends current mute/deafen, screen state and subscriptions instead of
replaying commands queued during the outage. Leaving cancels recovery immediately.
Missing/revoked/replaced sessions and exhausted recovery end the call and require
an explicit join. In particular, an unavailable session must not automatically
rejoin: a moderator may have disconnected it while signaling was lost.

ICE/media-write failures still use full reconnection with fresh authorization,
offers and ICE/TURN configuration. The old media and devices close before a new
peer starts. A recoverable ICE Disconnected state retains the existing peer.
Full retries use 1/2/4/8/16/30-second backoff plus up to 500 ms jitter; 30 seconds
of connected audio resets the backoff. Authentication, policy/protocol errors and
replacement notifications are terminal. Older servers without a resume capability
retain the previous full-reconnection behavior. Deploy the backend first.

Explicit Disconnect/logout closes signaling with Leave and a bounded close
handshake. A new authorized Join for the same account and channel still replaces
its membership atomically, including in a full room. Old media, state changes and
cleanup are fenced off before the replacement occupies its slot. The old socket
receives `replaced` and close code 4001 when reachable.

A single native task owns each call through negotiation, connected media, device
or ICE recovery and retry backoff. Repeated joins to that same channel are
idempotent; a channel switch cancels and drains the old task while holding the
same join/leave lock. Cleanup finishes before the next transport attempt begins.
The optional Join `attempt` object carries the first upgrade request ID as
`call_id`, plus `reconnect`. Explicit joins can take over an existing call;
automatic retries can only replace an existing membership with the same call ID.
This prevents a delayed retry on a displaced device from evicting the newer call
even when its replacement notification could not be delivered. Install the backend
update before the new client: older clients can still join the new server without
`attempt`, but older servers reject that new field. Replacement is scoped to the
same account and channel; joins to different channels remain independent on the
server.

Explicit Disconnect/logout, revoked/expired sessions, denied access, deleted
channels, incompatible protocol, certificate verification failures and local
device/DSP failures end retrying. Live voice uses the native 15-second heartbeat
lease described above; local audio tests use a three-second UI lease. Automatic
recovery from local device suspension remains pending. Update installation remains
blocked while a reconnecting channel is selected. Cancellation covers negotiation,
active calls and backoff. Connection
leases invalidate queued starts, rosters and packets from previous attempts, and
track readers receive an explicit shutdown signal. Regression tests cover retry
policy, cancellation, stale leases and transient versus terminal upgrade errors.
Physical network switching and multi-host outage recovery still need acceptance.

Server shutdown closes sockets/UDP with the process; coordinated drain and
production metrics remain pending.

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

Opt-in desktop logs record a per-process `connection_id` for every voice attempt.
After the WebSocket upgrade, `voice_signaling_connected` also records the validated
UUID from the server's `x-request-id` response header. Subsequent socket, peer,
retry, audio-failure and audio-session-end events carry that `request_id`, which
matches the backend HTTP and voice lifecycle logs. A retry gets a new connection
and request ID. Older servers or proxies that omit the header leave it null.

For a call drop, start with `voice_attempt_finished`: it records the reason,
retryability, connection stage, elapsed/connected time, last received signaling
age and received-message count. `voice_socket_error` adds the send/receive
operation and safe underlying error fields: I/O kind and OS code, WebSocket
protocol category (including `reset_without_closing_handshake`), capacity limits,
TLS category or HTTP status. Close events include the numeric close code;
`voice_server_error` includes the wire error code and error request ID.
`voice_signaling_closed` records whether cleanup succeeded, failed or timed out.
Cancellation is recorded separately from a failed attempt. Signaling-only recovery
logs `voice_signaling_recovery_started` and `voice_signaling_recovered` on the
client; server detach/reattach events correlate the new upgrade request ID with
the original media request ID and sequence, without recording the capability.

WebSocket Ping/Pong control frames are accepted before Join, before Answer and
during a call; they are never decoded as JSON voice commands. The transport
automatically replies to Ping. Backend control frames refresh receive liveness,
have a separate limit of 30 frames per second, and appear as `received_pings` and
`received_pongs` in the final lifecycle event. Exceeding that limit records
`signaling_control_rate_limit`. The native client's offer reader also accepts
control frames while retaining its negotiation deadline.

`audio_processing_failed` includes a typed stage/code, current processing options
and device health. It distinguishes non-finite input/output, Sonora render/capture
and delay errors, neural estimator failures, DeepFilter inference failures, and
Opus reset/encode/loopback-decode errors (with Opus function and numeric code).
The detailed user-facing message remains separate from log fields: arbitrary
library error strings can contain model paths or other private data.

At backend log level `info`, each upgraded voice socket records start/end events,
stage, reason, message counts and last-receive age, plus peer state changes and
membership release. The first failure survives subsequent cleanup errors. Reasons
distinguish client leave/close, invalid frames, rate limits, heartbeat/negotiation
timeouts, authorization failures and transport failures. Connection replacements
include the previous connection's request ID and active flag. Abrupt process termination
can prevent a final event; correlate with the service journal's stop/start records.
No frame bodies, close-reason text, headers, URLs, SDP, media payloads or credentials
are included in these new diagnostic fields. Logging adds no audio callback work. The reconnect policy above is implemented
separately from these diagnostics.

To correlate a shared desktop log with the VPS, copy its `request_id` and filter
the service journal, using UTC times for the query:

```sh
sudo journalctl -u thiscord --since '2026-10-13 19:10:00 UTC' --until '2026-10-13 19:45:00 UTC' --no-pager | rg 'REQUEST_UUID'
```

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

The [voice load generator](voice-load.md) drives the real SFU from a separate
machine, with disposable accounts and per-stream packet/latency reports. Its
synthetic transport workload does not establish microphone quality or measured
desktop/Pi capacity until run on the target hardware.

The planned voice-isolation work, candidate models, published computation costs
and acceptance criteria are in [voice-isolation.md](voice-isolation.md) and
[TODO section 5a](../TODO.md#5a-voice-isolation-and-speech-quality).
DeepFilterNet3 is implemented as an experimental denoiser; the remaining models
and personalized speaker isolation are still research candidates.

Native library tests cover independent mixing/gain, deafen backlog, queue bounds,
jitter ordering/replay/wrap, finite DSP output and Opus forwarding across two
encrypted WebRTC hops. Audio error tests cover recoverable notifications, fatal
device/route changes, error direction and preserving the first failure. Device
selection tests cover unplugged input/output independently, rejoin with stale IDs,
replug, missing defaults, enumeration failures and first-open fallback notices.
These deterministic tests do not replace physical USB/Bluetooth unplug acceptance.
Device format tests cover 24/32-bit PCM, multichannel selection/routing, input
conversion and rejection of incompatible rates/channel counts/non-PCM formats. Backend
PostgreSQL tests use disposable test schemas and
real peer connections to exercise forwarding, isolation, self-mute/deafen,
Speak denial and live changes, targeted permission/session revocation, unrelated
session and guild changes with continued bidirectional media, publisher slot reuse,
channel deletion and cleanup. Windows device enumeration is read-only and opens no mic.

DSP tests measure stationary-noise attenuation, delayed/reflected echo reduction,
preservation of near-end audio without playback, live bypass and recovery after
queue loss. The production queue path is tested with both 20 ms and 100 ms device
batches, with independent input/output timing. Locally, deterministic fixtures
measured approximately 20 dB stationary-noise reduction (previously 15 dB) and
54 dB echo reduction after warmup. These are synthetic results, not measurements
of the affected user's microphone or room. Mixer tests verify that the echo
reference includes individual/master gains, clipping and deafen silence.

Double-talk regression coverage uses overlapping, independently modulated voiced
harmonics, a reflected echo path, and optional speaker clipping, with automatic
gain both on and off. It checks correlated echo reduction and local-voice
retention against a noise-suppressed clean control. Correlation measures only
the source-correlated component; these synthetic signals do not establish real
speech intelligibility or performance in the affected room. Diagnostic tests
cover clipping percentages, warmup, silent-reference expiry and resets.

Still required: physical multi-user microphone/headset acceptance on each OS,
macOS runtime results, Wayland global-shortcut portal support, arbitrary sample
rates, long-call recovery, moderator voice controls, detailed speaking/quality
indicators, load/packet-loss/NAT trials and public deployment.


Moderator **Disconnect from voice**, timeouts and bans are available from each
server's Moderation panel, with a shortcut beside voice participants. Disconnect
ends that member's current transports in the server; an explicit rejoin is allowed.
Timeouts deny JoinVoice/Speak until expiry or removal, and bans deny membership.
All three stop screen media as well as microphone audio. They share media epoch
revocation and enforce guild roles/hierarchy; see [permissions.md](permissions.md#server-moderation).
Moderator mute, deafen and move-to-channel controls remain pending.
