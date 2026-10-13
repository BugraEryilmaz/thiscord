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

- RTP sequences order packets; 48 kHz RTP timestamps establish media positions.
  Both use signed wrapping comparisons within a short receive window. This path
  accepts the existing fixed 960-sample/20 ms Opus contract. Other packet durations
  become one silent slot and a decode-error count, not an unbounded decode loop.
- Each source holds at most 12 packets of at most 1,500 bytes, a 32-entry duplicate
  history, one Opus decoder and fixed resampling state. Packet ordering, startup
  waits and decode decisions are separate operations. There are still 32 bounded
  SPSC PCM rings; no additional application package or DSP dependency is added.
- Startup targets 40 ms. An EWMA (1/16 update) of arrival/media interval variation
  raises the target to `40 + 4 * variation` ms, bounded to 40-120 ms, with 5 ms
  hysteresis. Late packets still supply variation evidence; duplicates do not.
  After five seconds without an increase, the target falls at at most 1 ms/s,
  stopping within the hysteresis band. Isolated timing noise does not force a reset.
- The target covers packet waiting **and** decoded PCM. PCM refills below 15 ms
  and stays at or below 1,686 samples (35.125 ms), rather than adding the old
  40-60 ms cushion to a new network buffer. The 15 ms threshold preserves the
  existing irregular-worker regression, which includes 12 ms wakes.
- A missing packet is resolved only with at most 240 samples/5 ms left in the
  output ring: this is the worker's decode lead before the ring deadline. If the
  following packet is present at the adjacent media timestamp, Opus FEC is tried
  without consuming that carrier. Otherwise PLC runs. An FEC attempt can internally
  produce PLC when the sender supplied no redundancy; metrics do not claim recovery.
- After three consecutive missing slots (60 ms), playout becomes inactive or
  rebuffers from available media. Consecutive sequence numbers with timestamp gaps
  are discontinuity evidence, not a run of losses. The received sequence watermark
  is separate from the tentative PLC cursor, so a future DTX sender can resume
  without having advanced its sequence during silence. This does not implement
  sender DTX, comfort noise, variable packet duration or timestamp synchronization
  with screen video.
- When actual late discards accompany a target deficit above 20 ms, existing PCM
  drains and a bounded interval (at most 80 ms) builds tolerance. Such increases
  are limited to once per five seconds. Smaller changes use continuous correction.
  A gap above 100 ms between scheduler ticks is a worker stall, not network jitter.
  Worker-queued packets older than 160 ms are rejected. Rebuffering discards packet
  entries older than 160 ms, and normal decode refuses packets older than 200 ms.
  These are explicit discontinuity boundaries, not the normal drift strategy.
  An arbitrarily stalled OS callback cannot offer a wall-clock latency guarantee.
- The worker makes at most two decode decisions per source per tick. The rate
  converter emits at most 966 samples per packet, using 32 multiply-accumulate taps
  per output. Callbacks do no additional DSP, allocations, locks or networking.
  Adaptive remote queues never reach the callback's existing 60 ms safety trim,
  retained for local device tests. Lifecycle generation changes, deafen and
  inactive streams still discard their queues.

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
decode errors, inactivity and estimated drift/applied correction in ppm. Global
callback underrun samples remain available. `current_ms` excludes OS/device
buffering and the fixed 0.333 ms filter delay; it is not an end-to-end latency
measurement. Counters restart on source/device replacement and deafen resets.

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

The initial audit was reproduced: all 81 selected native audio tests passed on
Windows before changes. Windows x64, Rust 1.98.1, `ci` profile results:

All 96 native audio tests and all 108 audio tests with DeepFilterNet3 + neural
echo enabled pass. Targeted library/test Clippy passes for both feature sets;
native desktop check/Clippy and `cargo fmt --all -- --check` pass. Backend, shared
and UI behavior did not change; no database operation was needed. Linux/macOS
build/runtime and physical listening checks were not run locally for this change.

| Deterministic trace | Sequence-only baseline | Adaptive playout |
| --- | --- | --- |
| Stable LAN, 60 s | 40 ms delay, 55 ms maximum decoded PCM | 40 ms delay, 35 ms maximum decoded PCM; no concealment/underruns |
| 80 ms bursts + isolated/burst loss, 180 s | 67 concealments, no empty-ring time | 10 concealments, 156 ms total empty-ring time, two rebufferings; target peaks at 120 ms; delay peaks at 145 ms and ends at 44.8 ms |
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
are 66/6; empty-ring samples are 0/6,333 (about 132 ms); maximum adjacent-sample
steps are 0.1955/0.2050. These values expose the rebuffer-silence tradeoff and do
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
