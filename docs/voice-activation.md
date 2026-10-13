# Microphone activation

The native worker uses an **energy-based detector**, not speech or speaker
classification. It reduces short click activation and retains short quiet onsets.
It cannot reliably reject another person, TV dialogue, music, sustained typing or
a fan louder than the selected threshold. Denoising remains a separate option;
no new model, dependency, callback work or shared wire contract was added.

## Behavior and sensitivity

`AEC3 -> selected denoiser -> RMS observation -> optional AGC2 -> gate -> Opus`

Each 20 ms packet carries the RMS of its two denoised 10 ms halves. Detection uses
these values even if AGC is enabled; the retained/emitted audio includes AGC.
The existing sensitivity slider, `audio.json` persistence, finite range
0.001–0.5 and default 0.015 remain unchanged. There is no settings migration.
The opening RMS is `activation_threshold / sqrt(2)`, approximately preserving
the previous slider scale for sustained sine-like signals. Real microphones may
need readjustment because single peaks and AGC-amplified output no longer qualify.
Lower the threshold for quieter speech; raise it when background noise opens it.
The UI raw/processed meters remain peak meters, not this detector's RMS values.

- Attack: both halves of **two consecutive packets** must reach opening RMS
  (40 ms of sustained evidence). Any failed half interrupts attack. A single
  click, even a high-energy click, cannot open the gate alone.
- Hysteresis: while open, either half reaching half the opening RMS refreshes
  release (approximately 6 dB below the opening threshold).
- Release: retain ten complete packets (200 ms) below closing RMS, then close.
  Weak consonants and short pauses survive; entirely subthreshold utterances or
  onsets longer than the lookbehind can still be missed.
- There is no adaptive noise-floor estimator: persistent noise above closing RMS
  can hold an already-open gate indefinitely. This is an explicit limitation,
  covered by a test; no claim of background-voice isolation is made.

The state machine advances using audio duration, not worker wake frequency.
Timestamp/sequence inputs identify discontinuities; timing tests use a fixed
origin and explicit 20 ms steps without sleeping.

## Pre-roll, memory and latency

A fixed ring retains **three 960-sample packets (60 ms, 11,520 bytes of PCM)**.
On opening, it marks the retained packets eligible and emits the oldest one.
Every subsequent capture packet replaces exactly one ring slot and emits at most
one packet. Initial empty slots and closed slots emit zeros. There is no drain
loop on opening, no duplicate packet and no timestamp rewind. Silence continues
at the existing Opus/RTP cadence. The existing bounded capture batch drain can
still process up to six packets per tick; pre-roll adds no extra burst to it.

Tagged-sequence tests measure **exactly 2,880 samples / 60 ms of added delay** on
the automatic path, including continuously open speech. With ordinary 20 ms
packet acquisition, a packet's first sample reaches encoding about 80 ms after
capture, before other device/DSP/network delays. DFN3's documented 30 ms output
alignment remains additional. The release tail drains through the same 60 ms
delay. This is a deterministic scheduling measurement, not device-to-ear latency.
The complete gate state is tested below 12,000 bytes; processing uses fixed arrays
without heap allocation. Existing DSP, codec and transport work stays on workers.

**PTT is independent:** while pressed, current permitted audio goes directly to
Opus regardless of energy, with no gate delay, pre-press replay or release tail.
A press requires fresh post-boundary capture; packet acquisition and the selected
enhancer's normal warmup/alignment still apply. PTT does not bypass mute/deafen,
connection closure or backend permissions.

## Restrictions and resets

The native outgoing control generation, shared by the worker and sender, preserves
stop, mute/deafen, settings changes and both PTT edges even when several actions
occur between worker ticks or the control queue is full. On observing a generation
change, the worker clears gate/enhancement history, resets the local Opus state
and rejects capture starting before that observation time. Old
device blocks still advance AEC with the original reference timestamps, but feed
zero into enhancement. This prevents queued capture or DFN delay from seeding
new pre-roll after unmute. It conservatively discards a partial boundary packet.
Ordinary automatic gate closure does **not** reset enhancement or AEC.

Encoded packets carry the control generation and a native delivery lease. The
sender checks them and connection identity at enqueue, dequeue and every poll of
a pending RTP write. Capture age includes enhancement and gate delay; its budget
is 100 ms plus the fixed gate delay (160 ms automatic, 100 ms PTT), with the gate
allowance capped at 60 ms. A stall never expands this budget. For DFN3 automatic
activation, normal 20 ms acquisition + 30 ms enhancement + 60 ms gate delay uses
110 ms of the 160 ms budget. Output RTP positions stay on the current capture
clock, including holes from capture/queue drops; pre-roll never rewinds it.
Stop/restriction/relaxation invalidates prior generations; device
replacement, DSP gaps, recovery and session drop invalidate delivery leases and
wake pending writes. A generation change racing boundary observation also rejects
fresh capture until old pre-roll has been cleared.
Already-admitted transport writes cannot be recalled. Newly generated silence
can still be sent while restricted so the receiver does not keep concealing old
speech. No PCM or encoded payload crosses Tauri IPC or enters diagnostic logs.

Sequence gaps, backward/nonadvancing timestamps and capture spacing over 40 ms
clear the gate; device/model replacement and new connections start fresh. Backend
authorization remains authoritative and unchanged. Local speaking/transmitting
reports the emitted gate decision and successful bounded queue admission, not
the instantaneous detector state or a claim of remote delivery. Urgent boundaries
clear the local state when observed; existing UI polling latency still applies.

Opt-in recording schema 2 and `audio_replay` preserve the pre-roll delay and
enhancement-history boundaries. See [audio recording](audio.md#echo-debug-recordings-and-offline-replay).

## Detector evaluation (2026-10-13)

Reviewed the pinned local dependency source as well as upstream APIs/licenses:

| Candidate | API/license and decision |
| --- | --- |
| Sonora AGC2 RNN VAD, already transitive at 0.2.0 | `sonora_agc2::vad_wrapper::VoiceActivityDetectorWrapper::new(backend, 48000)` and `analyze(&[f32]) -> f32` consume 10 ms and internally resample to 24 kHz. The crate metadata/LICENSE declare BSD-3-Clause; `weights.rs` attributes its built-in coefficients to WebRTC's RNNoise weight file (no separate model artifact). AGC2's probability is private and tied to gain enablement. The wrapper has no public full-history reset; its internal periodic reset only clears the RNN, retaining feature/resampler history. A separate frontend adapter could rebuild it at boundaries, but needs quiet-speech/noise calibration and CPU measurements. Not enabled by this change. [Source](https://github.com/dignifiedquire/sonora/tree/a024d6ef8351add55be5e8b1d6cc35f555787660/crates/sonora-agc2). |
| Rust `webrtc-vad` / libfvad | `Vad::new_with_rate_and_mode`, `is_voice_segment(&[i16]) -> Result<bool, ()>` accept 10/20/30 ms at 8/16/32/48 kHz; reset restores rate/mode defaults. The Rust wrapper is MIT; bundled libfvad is BSD-3-Clause. Adds native C/FFI and conversion. No local acoustic or three-platform evaluation justifies adding it here. [API](https://github.com/kaegi/webrtc-vad/blob/master/src/lib.rs), [wrapper license](https://github.com/kaegi/webrtc-vad/blob/master/LICENSE), [libfvad license](https://github.com/dpirch/libfvad/blob/master/LICENSE). |
| Sustained RMS with hysteresis | Selected for a small, deterministic improvement over the audited peak gate without a new runtime/model. Explicitly accepts the speech/noise classification limits above. The synthetic comparisons below justify transient/onset behavior, not superiority to either speech detector. |

No large-model addition is justified by these measurements. A future speech
detector should be compared on consented recordings with target speech and
background speech separately labelled, rather than silently changing this gate's
claim to personal speaker isolation.

## Reproducible checks and results

The previous 81 selected audio tests passed again before edits on Windows.
New deterministic fixtures include labelled quiet onsets, harmonic syllables,
consonant-level intervals, sentence pauses/tails, seeded steady/changing noise
and seven short keyboard-like bursts. On this **synthetic 5.2-second fixture**:

| Metric | Previous peak/hold | Sustained RMS/pre-roll |
| --- | ---: | ---: |
| False opening transitions during labelled non-speech | 7 | 0 |
| Missed labelled speech packets | 2 (40 ms onset) | 0 |
| Added gate scheduling delay | 0 ms | 60 ms |

Metrics align emitted decisions to their source packet; they do not count the
intentional pre-roll/release context as false *openings*. This small fixture is a
regression test, not a general false-positive rate or a listening score. A separate
test confirms sustained above-threshold noise **does** open this energy gate.
The committed synthetic speech WAV also exercises real Sonora/DFN3, AGC on/off
and conventional/neural AEC combinations; pre-AGC evidence must match exactly
while the resulting PCM actually changes with AGC.

```powershell
cargo test -p thiscord-frontend --lib --features native-audio --locked audio:: -- --test-threads=2
cargo test -p thiscord-frontend --lib --features native-audio --locked labelled_synthetic_comparison -- --nocapture
cargo test -p thiscord-frontend --lib --example audio_replay --features deep-filter,neural-echo --profile ci --locked
cargo clippy -p thiscord-frontend --lib --tests --examples --features deep-filter,neural-echo --profile ci --locked -- -D warnings
cargo build -p thiscord-frontend --bin thiscord-desktop --features desktop --locked
cargo clippy -p thiscord-frontend --bin thiscord-desktop --features desktop --locked -- -D warnings
cargo fmt --all -- --check
```

Tests cover attack/release boundaries, threshold chatter, exact capacity/delay,
no duplicate output, invalid inputs, PTT, rapid unseen release/repress and
mute/unmute, deafen/permission denial, connection closure, queue pressure, device
replacement, gaps, enhancement/codec history and replay compatibility. The existing
three-platform CI audio job already runs these tests/replay and Clippy; build
requirements and Cargo.lock are unchanged.

Local Windows x64 verification after merging sender timing/FEC: 133 selected
native-audio tests; 156 library tests plus two replay tests with deep-filter/neural-echo
enabled; audio and desktop Clippy with warnings denied; native desktop build;
formatting and diff checks. The Windows debug link emitted OpenH264 LNK4255
(duplicate object names omit some dependency debug information) but succeeded.
Merge regressions cover fixed pre-roll age allowance with DFN delay, expiry and
cancellation during transport waits, current RTP slots across gate opening/gaps,
and a control change between boundary observation and capture processing.
The original AEC/double-talk regressions remain passing. No database, backend or
UI/shared-contract change required backend/WSL or WASM checks for this task.

Physical acceptance remains open: quiet Turkish/English syllables, whispers,
different microphones/rooms, fan changes, typing, competing voices, double-talk,
rapid controls, real device/network stalls and subjective acceptability of the
60 ms cost on Windows/macOS/Linux. Windows software checks do not establish
physical quality or native Linux/macOS compatibility. No acoustic improvement is
claimed from compilation, energy metrics or these synthetic tests.
