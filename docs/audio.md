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

CPAL buffer underrun/overrun (`Xrun`) and real-time scheduling (`RealtimeDenied`)
notifications keep the stream running. They may indicate an audible glitch or
reduced scheduling priority, not a disconnected device. Fatal notifications stop
audio and identify the microphone/output stream and CPAL error category. Route
changes still require explicit restart, including when CPAL could automatically
switch to another device. Callbacks record only an atomic failure code; error
formatting and stream cleanup happen on the audio worker.

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
  Devices must support 48 kHz PCM with 1-32 channels. Integer PCM at 8/16/24/32/64
  bits (signed or unsigned) and f32/f64 samples are converted by native callbacks.
  Mono capture and stereo playback are preferred. Multichannel capture is averaged
  to mono; playback uses the first one/two channels and silences the rest. A
  compatible default format is also considered when enumerated formats are missing.
  Rejection messages identify input/output, device name, default format and a bounded
  summary of reported formats; 48 kHz alone does not guarantee a compatible device.
  This includes common desktop devices but not every Bluetooth hands-free configuration.
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
  reference. Noise suppression uses the High setting. Automatic gain explicitly
  enables adaptive digital gain, starts at unity and caps gain at 20 dB with the
  library's -50 dBFS output-noise limit; it does not change OS microphone volume.
  Echo buffering delay comes from CPAL capture/playback timestamps, mapped onto
  a common monotonic clock. AEC3 estimates/refines the acoustic delay internally.
  Hardware acoustic quality still needs acceptance, so headphones remain recommended.
  Conventional residual estimation remains the default. A vendored Sonora
  extension also supports an optional Rust neural residual estimator inside AEC3,
  independent of the selected denoiser. Enable the experimental checkbox while
  stopped and enable Echo cancellation. Native clients include the pinned model;
  an optional local file override is available under Advanced model settings.
  See [neural echo integration](voice-isolation.md#neural-residual-echo-estimator-extension-2026-10-02)
  for setup, model provenance limits, reference tests and measured worker costs.
- DeepFilterNet3: optional native Rust/tract denoising, bundled in desktop builds.
  Select it in Audio & voice while stopped, enable Noise suppression, then join
  or test the microphone. Suppression can be toggled live; model changes require
  stopping audio. Existing settings retain Sonora. See [deep-filter.md](deep-filter.md)
  for the pinned runtime/model, stage interface, tests and performance results.

The native path is selected per the project requirement. Windows and Linux
software probes run locally; CI runs the same tests and desktop builds on all
three OSes. macOS runtime, microphone consent, headset changes and acoustic
acceptance remain open. Compilation alone does not establish those results.
WebView capture was not substituted for the requested native path.

## Audio timing and performance

Capture is averaged to mono, processed at 48 kHz and encoded as 20 ms Opus frames
at 32 kbit/s. Capture callbacks only convert/copy samples and update atomics.
Capture and post-mix reference callbacks assemble timestamped 10 ms blocks into
preallocated 120 ms SPSC rings. Partial callbacks retain their samples. The worker
drains complete capture packets instead of discarding audio from callbacks longer
than 60 ms. Work per tick stays bounded, and stop/PTT release are checked while
draining. Lost blocks carry a sequence gap that restarts DSP adaptation; playback
reference overflows are counted too. No per-sample allocation or DSP runs on the
device callback. The processor is constructed before starting either device.
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
and 1500 bytes/packet. Media queues and write deadlines isolate slow receivers.

Current sessions are rechecked every second and immediately after access-changing
commits. Each participant is reauthorized against the database; unrelated account
or guild changes leave authorized calls connected. Lost session/channel access or
a changed Speak grant closes only affected connections (Speak determines native
microphone setup in the offer). Both publisher routing and receiver writes check
the participant's validated generation under the access gate. Forwarding pauses
until reauthorization, and packets queued under an older generation are discarded,
so cached grants cannot continue forwarding after revocation. Each receiver gets
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

The planned voice-isolation work, candidate models, published computation costs
and acceptance criteria are in [voice-isolation.md](voice-isolation.md) and
[TODO section 5a](../TODO.md#5a-voice-isolation-and-speech-quality).
DeepFilterNet3 is implemented as an experimental denoiser; the remaining models
and personalized speaker isolation are still research candidates.

Native library tests cover independent mixing/gain, deafen backlog, queue bounds,
jitter ordering/replay/wrap, finite DSP output and Opus forwarding across two
encrypted WebRTC hops. Audio error tests cover recoverable notifications, fatal
device/route changes, error direction and preserving the first failure. Device
format tests cover 24/32-bit PCM, multichannel selection/routing, input conversion
and rejection of incompatible rates/channel counts/non-PCM formats. Backend
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
