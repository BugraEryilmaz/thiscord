# Experimental DeepFilterNet3 audio processing

In **Settings > Audio & voice**, stop audio, select **DeepFilterNet3 (experimental)**,
check **Noise suppression**, then test the microphone or rejoin voice. The checkbox
can change during a call; the model selection requires stopping audio. Existing
settings keep Sonora selected, and suppression remains off unless enabled.
DeepFilterNet3 enhances speech generally; it is not an enrolled-speaker isolator.

## Processing boundaries

Native capture runs at 48 kHz mono in 480-sample blocks:

`capture -> Sonora AEC3 -> selected denoiser -> optional Sonora AGC2 -> transmit gate -> Opus`

`processing/stages.rs` defines `CaptureStage` (`process`, `reset`, `delay_samples`)
and an ordered `CaptureChain`. Add an adapter implementing that interface to
replace a model or compose more stages with `push`; stage order is explicit.
`processing/deep_filter.rs` is the only adapter coupled to DeepFilterNet types.
The default chain selects exactly one denoiser, avoiding accidental Sonora + DFN
double suppression. Independent AEC and gain controls still work in either mode.

Graph construction and model warmup happen on the native worker before playback
and capture start. The chosen model stays loaded while bypassed so re-enabling
does not compile a model during a call. Bypass skips inference. Changing enable
states clears delayed samples and gain history; a short discontinuity/warmup is
possible. Reset restores pristine neural state without reparsing model files:
upstream `init()` does not clear all rolling/recurrent history.

Callbacks still only convert/copy/mix through bounded rings. tract may allocate
temporary tensors on the worker; this is not a claim of allocation-free inference.
PCM never crosses Tauri IPC. The server/SFU and remote speaker volume paths are
unchanged. Stop, PTT release and mute inhibition bypass a saturated command queue.
Inference failure or non-finite output stops audio; there is no silent fallback
to an unprocessed microphone. Models and embeddings are never downloaded at runtime.

The model adds **1,440 samples / 30 ms of output alignment delay**, plus the
10 ms capture block acquisition (40 ms algorithmic framing budget). This excludes
AEC/filter-bank delay, device buffering, Opus framing and network/jitter buffers.
The offline quality benchmark independently aligns all pipelines, including
Sonora's filter-bank delay, before comparing SNR.

On x86_64/ARM64, a scoped floating-point guard flushes subnormal values during DSP
and restores the caller's control register. This avoids expensive CPU assists
when recursive filters decay toward silence. No rounding mode is changed.

## Dependencies and reproducibility

- Upstream [DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) revision
  `d375b2d8309e0935d165700c91da9de862a99c31` (`deep_filter` 0.5.7-pre).
- Bundled `DeepFilterNet3_onnx.tar.gz`: 7,983,136 bytes, SHA-256
  `c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616`.
- Native Rust inference via tract **0.21.4**, pinned at all four entry points.
  Later versions admitted by upstream's caret requirement change ndarray/types
  and model APIs incompatibly. Update runtime and model together and rerun tests.
- In development, hot inference/FFT dependencies are optimized. App debug checks
  remain enabled. tract-core uses release-style debug assertions because its
  optimizer creates duplicate `Conv.bias` display names that its debug graph-name
  checker rejects, even though release inference works. Dev and release paths
  are both tested; no edits to Cargo caches or vendored fourth package are needed.
- The model and runtime are from upstream's MIT/Apache distribution; selected MIT
  notice and model provenance ship in `frontend/THIRD_PARTY_AUDIO.txt` as a Tauri
  resource. No Python, Node, GPU, external service or native C++ ML runtime added.
- `desktop` includes the `deep-filter` feature. The native library can still build
  with only `native-audio`; selecting DFN in that build returns an explicit error.
  WASM/shared/backend do not import the inference runtime.

## Regression tests and offline benchmark

```sh
cargo test -p thiscord-frontend --lib --features deep-filter --release --locked
cargo run -p thiscord-frontend --example processing_bench --features deep-filter --release --locked -- 1000
```

The first optional argument is measured blocks per case (100..100000); the second
is a 48 kHz mono PCM16 WAV, limited to one minute. Without it, the committed
synthetic speech fixture is used. The tool never opens devices or saves recordings.

JSON lines report bypass, Sonora, DFN3 and AEC+DFN3+gain, each with speech/noise,
noise-only and silence inputs. They include startup/reset costs, mean/p50/p95/p99/
maximum processing time, RTF and 10 ms deadline misses after a one-second warmup.
Quality runs separately, using seeded noise, measured sample alignment and SNR.
The benchmark includes AEC's render processing for the combined case; its timing
reference is a synthetic independent tone, not an acoustic echo-quality fixture.
It excludes callbacks, playback mixing, codecs and networking. On Linux, run the
built executable under `/usr/bin/time -v` for whole-process peak RSS; that is not
the model's incremental memory cost.

Tests cover noise attenuation AND synthetic speech preservation (muting everything
cannot pass), repeated fresh-state resets, stale-audio removal after queue gaps,
bit-exact bypass, re-enable, illegal hot swaps, independent gain/AEC composition,
invalid samples/stage output, stage order and urgent controls under queue pressure.
Existing echo/double-talk, jitter, mixing and WebRTC regressions remain enabled.
The three-OS CI runs the audio tests in release mode and publishes informational
benchmark JSON artifacts. Shared runners do not enforce hard CPU timing gates.

Physical microphones, listening acceptance, macOS runtime and battery/thermal
behavior remain required before changing defaults. The synthetic English fixture
does not establish intelligibility across real speakers, languages or rooms.

## Local results (2026-09-30)

Intel Core i7-13700K, Windows x64 and Kali Linux under WSL on the same host;
Rust 1.98.1 release builds, 1,000 measured 10 ms blocks per case after 100 warmup
blocks. These are desktop CPU results, not low-power laptop capacity guarantees.
Raw results: [Windows](benchmarks/deep-filter-windows-2026-09-30.jsonl) and
[Linux/WSL](benchmarks/deep-filter-linux-2026-09-30.jsonl).

| Speech + noise pipeline | Windows mean / p99 | Linux mean / p99 |
| --- | --- | --- |
| Bypass | 0.001 / 0.002 ms | 0.001 / 0.001 ms |
| Sonora High | 0.017 / 0.020 ms | 0.020 / 0.033 ms |
| DeepFilterNet3 | 0.361 / 0.663 ms | 0.323 / 0.651 ms |
| AEC3 + DeepFilterNet3 + gain | 0.428 / 0.732 ms | 0.389 / 0.689 ms |

DFN3's speech/noise RTF is approximately 0.032-0.036 (3.2-3.6% of one core).
No measured case missed a 10 ms deadline. Silence still costs work: the combined
pipeline's silence p99 was 2.60 ms on Windows and 0.80 ms on Linux. Before the
subnormal guard, a Linux silence trial reached 13.6 ms with 24 deadline misses.
This is why silence is benchmarked as well as speech; exact timings vary per run.

Whole benchmark process memory peaked at approximately 42 MiB working set on
Windows (sampled process peak) and 46 MiB RSS on Linux (`getrusage` child peak).
These include models, runtime and fixtures across sequential test cases, not
incremental model allocations or the complete Tauri application. Initialization
is roughly a quarter second on this host, performed before capture starts.

On the fixed synthetic speech + white-noise fixture, DFN3 improved aligned SNR
from 10.63 to 16.14 dB. Sonora scored 0.70 dB and the combined AEC/DFN/gain path
0.32 dB on this particular waveform metric, which penalizes filter phase/coloration
and gain changes as well as residual noise. These are not perceptual rankings.
Separate regressions require speech retention and noise attenuation; subjective
audio and acoustic double-talk acceptance are still necessary.

Validation: 35 native library tests pass on Windows (dev with optimized DSP) and
Linux (release); 10 shared contract tests pass. Windows native launcher/audio
and WASM Clippy pass, as do Linux backend/shared Clippy and non-ignored tests.
Database integration tests and macOS runtime checks were not run locally for
this change; CI retains PostgreSQL tests and the native three-platform matrix.
