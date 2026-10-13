# Timestamp-aware native playout

The receive path is `RTP + arrival Instant -> bounded packet ordering/media
timeline -> deadline decode/FEC/PLC -> fractional rate correction -> per-source
PCM ring -> gain/mix/master/clipping -> device and AEC reference`. All new state
is native frontend state. No wire contracts, SFU authorization, capture processing,
Sonora tuning, AEC reference timestamps or IPC payloads change.

`native_voice.rs` records arrival immediately after `TrackRemote::poll` yields a
packet, before the audio worker queue. This excludes worker queue delay from the
arrival estimator; it cannot exclude delays inside the OS/WebRTC receive task.
The scheduler takes `Instant` and actual PCM occupancy explicitly, so tests advance
receiver, worker and output clocks without sleeping or opening a device.

## Timing and bounds

- 48 kHz RTP timestamps order media; sequences detect duplicates and sequence holes.
  Both use signed wrapping comparisons within a short receive window. This path
  accepts the existing fixed 960-sample/20 ms Opus contract. Other packet durations
  and malformed headers are rejected before jitter admission. Decode failures
  use bounded PLC, or silence if that fallback fails.
- Each source holds at most 12 packets of at most 1,500 bytes, a 32-entry duplicate
  history, one Opus decoder and fixed resampling state. Packet ordering, startup
  waits and decode decisions are separate operations. There are still 32 bounded
  SPSC PCM rings; no additional application package or DSP dependency is added.
- Startup targets at least 40 ms, or the device-sized refill limit plus 10 ms,
  whichever is larger. An EWMA (1/16 update) of arrival/media interval variation
  raises the target to `minimum + 4 * variation` ms, capped at 120 ms, with 5 ms
  hysteresis. Late packets still supply variation evidence; duplicates do not.
  After five seconds without an increase, the target falls at at most 1 ms/s,
  stopping within the hysteresis band. Isolated timing noise does not force a reset.
- The target covers packet waiting **and** decoded PCM. Each device callback
  publishes its size and estimated next deadline through bounded atomic stores.
  The largest observed mono-frame count is retained for that device lifetime.
  PCM refills at `max(720, callback_samples + 576)` samples: a 15 ms minimum or
  one callback plus a 12 ms worker allowance. The limit is capped at 4,794 samples,
  leaving room for a maximally stretched packet in the existing 5,760-sample ring.
  Small callbacks retain the 1,686-sample maximum; larger callbacks get a larger
  cushion, rather than repeatedly emptying a single 20 ms frame.
- A missing packet normally waits until at most 240 samples/5 ms remain. Within
  12 ms of the estimated next callback (or when that callback is overdue), it can
  also be resolved if the ring cannot cover a complete callback. This avoids both
  callback starvation and premature PLC while a large callback is still far away.
  If the following packet is present at the adjacent media timestamp and has a sequence
  hole after the last normal packet, Opus FEC is tried
  without consuming that carrier. Otherwise PLC runs. An FEC attempt can internally
  produce PLC when the sender supplied no redundancy; metrics do not claim recovery.
- Consecutive missing media is limited to 60 ms before inactivity/rebuffering.
  Sequence holes alone never invent missing media. Short timestamp holes preserve
  exact sample counts: whole slots use PLC, fractional holes use exact-length
  silence through the continuous rate filter. Consecutive sequences with timestamp
  gaps do not borrow FEC; those gaps can be sender capture drops. Longer timestamp
  jumps rebuffer. Consumed sequence/media watermarks survive short inactivity and
  deafen, preventing late audio from rewinding the cursor. After six hours without
  an accepted packet, the next fresh valid packet starts a new local RTP epoch:
  signed 32-bit timestamp ordering is ambiguous after about 12.4 hours at 48 kHz.
  The reset retains counters, rejects older receive times and requires the normal
  decoder/PCM flush acknowledgement. Worker-age and control-time checks still run
  before admission; this does not authenticate the age of packets delayed upstream.
  The microphone now uses
  [Opus DTX](audio.md#microphone-dtx-and-media-timing), retaining CN updates and a
  repeated no-data cue. Known silence suspends PLC and drift integration, preserves
  timestamps, and resumes at the current target without counting a recovery.
  Talkspurt/no-data packets cannot donate FEC. Silence, inactivity, sequence holes
  and late packets have separate metrics; media inactivity does not disconnect.
  Variable packet duration and screen-video timestamp synchronization are absent.
- When actual late discards accompany a target deficit above 20 ms, existing PCM
  drains and a bounded interval (at most 80 ms) builds tolerance. Such increases
  are limited to once per five seconds. Smaller changes use continuous correction.
  A gap above 100 ms between scheduler ticks is a worker stall, not network jitter.
  Worker-queued packets aged 200 ms are rejected; packet maintenance uses the same
  age cap. Capacity pressure or media spans beyond 240 ms reanchor at the newest
  40 ms (up to three packets). Hard recovery resets the decoder/filter and requests
  a PCM flush, waiting for callback acknowledgement before admitting fresh PCM.
  Maintenance runs even with a full ring; it invalidates remaining decoded speech
  after 200 ms without arrivals, including when no encoded packets remain.
  These are explicit discontinuity boundaries, not the normal drift strategy.
  An arbitrarily stalled OS callback cannot offer a wall-clock latency guarantee.
- The worker makes two to six decode decisions per source per tick, depending
  on callback size; the refill limit stops work once enough PCM is queued. The rate
  converter emits at most 966 samples per packet, using 32 multiply-accumulate taps
  per output. Callbacks do no additional DSP, allocations, locks or networking.
  The callback's safety trim is at least 60 ms and expands to accommodate the
  device-sized cushion, never exceeding the existing ring capacity. Lifecycle
  generation changes, deafen and inactive streams still discard their queues.
  The callback estimate cannot anticipate an unseen larger block or a driver
  delivering callbacks early. Blocks beyond the ring's capacity and arbitrarily
  stalled workers remain outside these buffering guarantees.

## Clock correction and device conversion ownership

The next media position, newest arrival and decoded depth estimate the latest
packet's arrival-to-playout delay. Ring consumption includes the actual output
device clock. A five-second low-pass error drives proportional delay correction;
a slower integral estimates sender/output skew, bounded to ±1,000 ppm. The
integral freezes during high variation, large delay error, concealment and for
ten seconds after target increases. It is a clock-skew estimate, not an independent
hardware clock measurement. Worker gaps do not feed the controller.

Combined correction is capped at ±5,000 ppm (±0.5%) and slews at at most 1,000
ppm/s during continuous playback. Explicit rebuffer/reset boundaries may reset
that correction. `playout_rate.rs` owns this conversion, once per source, **before
mixing**. Its streaming, 32-tap Hann-windowed sinc has 256 interpolated phases,
0.94-Nyquist cutoff, and a constant 16-sample (0.333 ms) group delay, including at
unity. Phase/history survive packet boundaries. It slightly changes pitch and
attenuates the extreme upper band; it is not a pitch-preserving time stretcher.

Arbitrary device-rate support remains pending. Its future fixed 48 kHz-to-device
conversion belongs after mixing, and capture has its own device-to-48 kHz owner.
Those converters must not add another per-speaker occupancy/drift loop. A future
non-48 kHz output must provide AEC with the actual final rendered signal mapped
back onto the documented 48 kHz reference timeline, including conversion delay.
Currently the reference is unchanged: the callback records the actual post-rate,
post-gain, post-clipping/deafen mono signal at CPAL playback times. No echo
algorithm, AEC tuning, capture filter or reference offset was changed.

## Diagnostics and verification

Existing opt-in `audio_snapshot` logs include per-source playout metrics without
account IDs, timestamps, PCM or packet contents: target/current delay, arrival
variation, packet/PCM occupancy, late/duplicate/capacity/stale discards, worker
queue discards/stalls, rebuffer/discontinuity counts, FEC attempts, concealments,
decode errors, inactivity and estimated drift/applied correction in ppm.
Per-stream callback starvation samples remain available (buffered-media evidence
required; explicit DTX, inactive slots and deafen excluded). These are not device
xrun counts; see [quality definitions](audio-quality.md). `current_ms` excludes OS/device
buffering and the fixed 0.333 ms filter delay; it is not an end-to-end latency
measurement. Counters restart on source replacement; device/deafen boundaries
clear queued audio and rate state while retaining counters and unexpired media
watermarks.

Run with the pinned toolchain and existing shared build directory:

```sh
cargo test -p thiscord-frontend --lib --profile ci --features native-audio --locked audio:: -- --test-threads=2
cargo test -p thiscord-frontend --lib --profile ci --features deep-filter,neural-echo-probe --locked audio:: -- --test-threads=2
cargo test -p thiscord-frontend --lib --profile ci --features native-audio --locked audio::jitter:: -- --nocapture
cargo test -p thiscord-frontend --lib --profile ci --features native-audio --locked audio::playout_rate:: -- --nocapture
```

`jitter/legacy.rs` is a test-only copy of the audited sequence-only algorithm,
with its clock injected. Identical deterministic network traces compare latency,
PCM depth, underrun time and concealment counts. The simulation consumes PCM on a
separate output clock and models corrected sample counts; production Opus/filter/
mixer tests independently verify continuous samples and the final AEC reference.
Additional tests cover wrap, duplicates, reordering, isolated/burst loss, source
jumps, inactivity/resumption, worker queue delay, independent speakers, malformed
durations, deafen and source generations. Tone tests measure resampling waveform
error against analytic fractional-time signals, including packet boundaries.

Callback regressions run 120-second production Opus/filter/mixer traces with
48, 480, 960, 1,024, 2,048 and 4,096-frame stereo callbacks, varying callback sizes,
irregular 4-12 ms worker wakes and an isolated lost packet. After startup they
require enough PCM for every callback, zero overflows/late discards/rebuffers,
and exactly the expected concealment count. Overnight-deafen tests advance
13, 26 and 49 hours without sleeping, require resumed audio after the flush
acknowledgement, and reject queued old media and repeated consumed packets.

The initial audit was reproduced: all 81 selected native audio tests passed on
Windows before changes. Windows x64, Rust 1.98.1, `ci` profile results:

After integrating main's sender timestamps, FEC policy and activation gate,
the native audio suite contains 153 tests and the combined DeepFilterNet3/neural
echo suite contains 165; both pass. Native library/tests and optional-processing
examples pass Clippy with warnings denied. The Windows desktop build and desktop
Clippy pass, as do the WASM `thiscord-ui` build and `cargo fmt --all -- --check`.
Merge regressions cover exact fractional gaps through rate correction,
decoded-only stalls, and callback flush acknowledgement when a
gap becomes visible after decoding. The checks and trace results below use the
merged receive path. Backend/shared behavior is unchanged; no database operation
was needed. Linux/macOS build/runtime and physical listening checks were not run
locally for this change.

| Deterministic trace | Sequence-only baseline | Adaptive playout |
| --- | --- | --- |
| Stable LAN, 60 s | 40 ms delay, 55 ms maximum decoded PCM | 40 ms delay, 35 ms maximum decoded PCM; no concealment/underruns |
| 80 ms bursts + isolated/burst loss, 180 s | 67 concealments, no empty-ring time | 9 concealments, 151 ms total empty-ring time, two rebufferings; target peaks at 120 ms; delay peaks at 120 ms and ends at 44.8 ms |
| Sender +400 ppm, 30 min | Delay reaches 194 ms; 100 ms empty-ring time | Delay peaks at 44.04 ms and ends at 40 ms; +402 ppm final correction; no concealment, rebuffer or underrun |
| Sender -400 ppm, 30 min | 200 concealments; 700 ms empty-ring time | Delay ends at 40 ms; -402 ppm final correction; no concealment, rebuffer or underrun |

Output-only +400 ppm and sender/output +200/-200 ppm traces also maintain 40 ms
final delay without concealment/rebuffer/underrun. The seeded 0-23 ms jitter trace
raises the target to 52.64 ms without concealment or underruns. These simulations
exclude network base latency, OS buffers, codec/filter delay and physical sound.

The rate converter's analytic 100/1,000/8,000/16,000 Hz tone tests at 0, ±400 and
±5,000 ppm measure at least **67.73 dB** waveform SNR after startup alignment,
including packet boundaries. This is interpolation error, not perceived voice
quality, echo performance or an intelligibility score.

A separate 16-second **real Opus/PCM** comparison uses the committed synthetic
speech fixture with the same burst/loss pattern. Baseline/adaptive concealments
are 66/5; empty-ring samples are 0/7,680 (160 ms); maximum adjacent-sample
steps are 0.1955/0.2038. These values expose the rebuffer-silence tradeoff and do
not rank audible artifacts. Generate both WAVs for matched listening explicitly:

```powershell
$env:THISCORD_PLAYOUT_FIXTURE_DIR = 'target/playout-fixtures'
cargo test -p thiscord-frontend --lib --profile ci --features native-audio --locked audio::playout_tests::speech_trace_comparison -- --nocapture
Remove-Item Env:THISCORD_PLAYOUT_FIXTURE_DIR
```

The output directory receives `playout-legacy.wav` and `playout-adaptive.wav`.
Only the repository's synthetic fixture is used; no microphone or network opens.
No files are written when the environment variable is unset. The comparison
checks finite, non-silent output; listening acceptance remains open.

## Remaining acceptance

Synthetic waveform error and fewer concealed frames do not establish audible
improvement. Rebuffering trades a short silence for future jitter tolerance;
late/loss bursts and activity transitions can still click. Blind listening with
consented speech/music, physical long calls, CPU contention, large device batches,
USB/Bluetooth devices and real networks remain required on Windows, macOS and
Linux. Verify far-end echo and double-talk on the user's working AEC setup. No
physical acoustic or cross-platform runtime acceptance is claimed by this change.
