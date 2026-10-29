# Noise suppression models and strength

In **Settings > Audio & voice > Voice processing**, check **Noise suppression**,
then open **Noise suppression model** to choose a model and a **Suppression
strength**. Model, strength and the enable switch all apply during a call.
Saved settings without these fields keep Sonora at Strong. All models run
locally on the native audio worker; weights are compiled into the client and
nothing is downloaded at runtime. None of them isolates an enrolled speaker:
other people's voices and TV dialogue can pass through.

| Model | Setting value | Rate | Added buffering | Character |
| --- | --- | --- | --- | --- |
| Sonora (WebRTC NS) | `sonora` | 48 kHz | 0 (filter-bank delay only) | Mainly steady noise (fans, hum); cheapest |
| DeepFilterNet3 | `deep_filter_net3` | 48 kHz | 1,440 samples / 30 ms | Strongest general denoiser; full band |
| RNNoise (nnnoiseless 0.5.2) | `rnnoise` | 48 kHz | 480 samples / 10 ms | Lightweight; good on colored noise, leaves broadband hiss |
| GTCRN | `gtcrn` | 16 kHz internally | 1,632 samples / 34 ms | Small neural model; no audio above 8 kHz |

Buffering excludes the 10 ms capture block, AEC, devices, Opus and network.
Sender age budgets include each model's reported delay; see
[sender timing](audio.md#microphone-sender-timeline-and-expiry).

## Strength

One shared setting maps onto each model:

| Strength | Sonora level | DeepFilterNet3 / RNNoise / GTCRN attenuation limit |
| --- | --- | --- |
| Light | Low (~6 dB) | 6 dB |
| Balanced | Moderate (~12 dB) | 12 dB |
| Strong (default) | High (~18 dB) | 18 dB |
| Maximum | VeryHigh (~21 dB) | Unlimited (the model's own output) |

Neural models limit attenuation by mixing back a fixed share of unprocessed
signal, `enhanced * (1 - g) + noisy * g` with `g = 10^(-limit/20)`. Noise is turned
down by at most the limit and speech that a model misjudges as noise loses at most
the same amount, so lower strengths trade residual noise for a more natural voice.

- DeepFilterNet3 uses libDF's built-in spectral limit (`set_atten_lim`); see
  [deep-filter.md](deep-filter.md#suppression-strength).
- GTCRN mixes the model's input spectrum before inverse STFT.
- RNNoise mixes in the time domain against the input delayed by its one-frame
  output latency. Its internal DC high-pass makes this approximate below ~20 Hz.
- Sonora has no mix-back; strength rebuilds its suppressor at the mapped level.

Changing strength never reloads a model or replaces a stage. Neural stages keep
their history (only the mix factor changes); Sonora restarts its suppressor.
Before this setting DeepFilterNet3 always ran unlimited; use Maximum for that.

## GTCRN integration

`processing/gtcrn.rs` is the only module coupled to GTCRN/tract-onnx. The pinned
upstream streaming export ([provenance](../frontend/models/gtcrn_simple.txt)) is
loaded by tract-onnx 0.21.4, the same pinned tract used by DeepFilterNet3, so no
native C++ runtime is added. Each 10 ms block is decimated to 160 samples with the
existing device-rate FIR converter, analyzed in 512-point periodic sqrt-Hann STFT
frames with a 256-sample hop, run through the model with its three explicit
caches, overlap-added, then interpolated back to 48 kHz. Because 16 ms hops do
not divide 10 ms blocks, 224 samples of silence are queued at reset so every block
can be filled; this and the hop history give 480 samples at 16 kHz (30 ms), plus
2 ms of group delay for each converter. The model output is band-limited to 8 kHz;
the upper band is removed rather than passed through, so background noise above
8 kHz cannot leak. Voices sound less crisp than with the 48 kHz models.

### Native evaluation inside tract

Plain tract-onnx 0.21.4 could not run this graph in real time: the first build
averaged 9.7 ms per 10 ms block with 625/1,000 deadline misses. Profiling showed
almost none of it was arithmetic, so `processing/gtcrn/` replaces three paths:

- **GRU** (`gru.rs`): tract lowers ONNX GRU to a `Scan` whose per-step overhead
  dominated eight 33-step, hidden-size-4 recurrences per frame (~7 ms). A native
  op registered for ONNX `GRU` evaluates the standard equations directly. It
  rejects reverse-only direction, custom activations, clipping, batch-first
  layout and `sequence_lens` instead of approximating them.
- **ScatterND/Gather** (`indexing.rs`): cache updates and scalar-index cache reads
  took ~7 ms through tract's generic paths. After optimization, those nodes keep
  tract's facts but evaluate f32 data with bounds-checked slice copies (constant
  scatter coordinates become precomputed runs); other types use tract's code.

tract's `Scan` also keeps its hidden state inside a reused `SimpleState` and
ignores later initial-state inputs. GTCRN's bidirectional intra-frequency GRUs
must start from zero every frame, so the pure-tract path also carried state
across frames incorrectly. The native op uses standard stateless ONNX semantics.
Tests compare the native plan, run on one reused state as in production, with the
unmodified tract graph run on a fresh state per frame: worst relative error over
60 frames with carried caches is 4.4e-6. A synthetic ONNX GRU test also checks
forward/bidirectional, both `linear_before_reset` values and initial states.

Reset recreates the run state, zeroes the caches/STFT/overlap buffers and rebuilds
both converters, so a privacy boundary leaves no delayed audio. Inference or shape
errors stop audio like DeepFilterNet3; there is no silent unprocessed fallback.
tract allocates temporary tensors per 16 ms frame on the worker, not in callbacks.

## RNNoise integration

`processing/rnnoise.rs` wraps nnnoiseless, a pure-Rust RNNoise port with the
standard Xiph weights compiled in. It processes exactly one 480-sample block per
call (16-bit-scaled samples) and outputs the previous block. On the synthetic
tests it removes about 50 dB of colored (low-pass) noise but under 3 dB of flat
white noise, which it treats as speech-like hiss; expect broadband hiss to remain.

## Feature flags and builds

`rnnoise` (nnnoiseless) and `gtcrn` (tract-onnx/tract-hir, rustfft) are separate features,
both included in `desktop` and `voice-bot`. Selecting a model that a build lacks
returns an explicit error. WASM/shared/backend do not import either runtime; the
shared contract only adds the `rnnoise`/`gtcrn` model names and the
`noise_suppression_strength` field, omitted from JSON at its Strong default so
the previous settings wire shape is unchanged.

## Local results (2026-10-29)

Intel Core i7-13700K, Windows x64, Rust 1.98.1 release build, 1,000 measured 10 ms
blocks per case after warmup; [raw results](benchmarks/noise-suppression-windows-2026-10-29.jsonl).
Worker DSP only (no devices, mixer, codecs or network). Linux, macOS and low-power
laptops were not measured for this change.

| Pipeline (speech + white noise) | Mean / p99 / max per block | RTF | Init | Aligned SNR (input 10.63 dB) |
| --- | --- | --- | --- | --- |
| Sonora, Strong | 0.017 / 0.020 / 0.032 ms | 0.002 | 0.2 ms | 0.70 dB |
| DeepFilterNet3, Maximum | 0.352 / 0.554 / 0.686 ms | 0.035 | 237 ms | 16.14 dB |
| DeepFilterNet3, Strong | 0.367 / 0.627 / 0.696 ms | 0.037 | 223 ms | 16.76 dB |
| RNNoise, Strong | 0.030 / 0.045 / 0.113 ms | 0.003 | 0.1 ms | 12.19 dB |
| GTCRN, Strong | 0.359 / 0.685 / 0.837 ms | 0.036 | 112 ms | 20.99 dB |

No case missed a 10 ms deadline. In the unoptimized dev profile (`tauri dev`),
GTCRN measured 2.0 ms mean / 3.4 ms p99, comparable to AEC3 + DeepFilterNet3
(1.8 / 3.2 ms): its native ops live in the frontend crate, which dev builds do
not optimize. Constant ScatterND coordinates are precomputed into merged slice
copies, and reset restarts the resamplers without redesigning their filters. Aligned SNR is a waveform
metric on one synthetic English fixture with seeded white noise. It penalizes
coloration and filter phase (hence Sonora's low score) and treats GTCRN's missing
upper band only as removed noise, so it is **not** a perceptual ranking: listen
with your own microphone before choosing. On the same fixture DeepFilterNet3 at
Strong scored above unlimited, consistent with less speech damage.

Synthetic low-pass noise reductions in `model_tests` (Light/Balanced/Strong/Maximum):
Sonora 8.0/13.9/19.5/22.6 dB, DeepFilterNet3 6.0/12.0/18.0/55.3 dB, RNNoise
6.0/12.0/17.9/51.6 dB, GTCRN 5.9/11.5/16.7/28.7 dB.

## Tests and benchmark

```sh
cargo test -p thiscord-frontend --lib --features deep-filter,rnnoise,gtcrn --profile ci --locked model_tests
cargo run -p thiscord-frontend --example processing_bench --features deep-filter,rnnoise,gtcrn --release --locked -- 1000
```

`processing/model_tests.rs` checks, for every compiled model: strength ordering and
the 6/12/18 dB caps on stationary noise; live strength changes without replacement;
reset equivalence with a fresh processor; bit-exact bypass; and live switches through
all prewarmed models starting from fresh state. For RNNoise and GTCRN it also checks
noise removal, speech energy retention, measured alignment within two samples of the
reported delay, 30 seconds of silence and invalid-input fail-closed behavior.
`processing/gtcrn.rs` and `processing/gtcrn/gru.rs` hold the native-evaluation
parity tests described above. The three-OS CI builds these features and publishes
informational benchmark JSON; shared runners do not enforce CPU timing gates.
