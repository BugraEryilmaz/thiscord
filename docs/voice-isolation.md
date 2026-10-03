# Voice isolation research and implementation plan

Research date: 2026-09-30. DeepFilterNet3 is now implemented as an experimental
denoiser. See [integration and local measurements](deep-filter.md). SpeakerBeam-SS
source feasibility was reviewed below; live integration is blocked on a suitable
streaming checkpoint. Other candidates and personalized extraction remain research.
Implementation checklist: [TODO section 5a](../TODO.md#5a-voice-isolation-and-speech-quality).

## Goal and current baseline

Preserve the microphone user's voice while reducing environmental noise, other
people and TV/video dialogue, including when the user speaks at the same time.
Treat the reported similarity to Discord as a listening observation; establish
a matched baseline before claiming that Thiscord performs better.

Current native processing uses Sonora 0.2 AEC3 with Thiscord's post-mix playback
reference, a choice of Sonora High or DeepFilterNet3 denoising, then optional
adaptive AGC2. There is no enrolled speaker model. See [audio.md](audio.md) for
timing, diagnostics and current tests.

Separate three problems when evaluating a change:

| Problem | Technique to evaluate | Limitation |
| --- | --- | --- |
| Echo of the remote call through local speakers | AEC3 timing/adaptation, then reference-aware neural AEC or residual suppression | Requires a correctly aligned playback reference; another laptop's YouTube audio is absent from that reference. |
| Fans, typing, music and environmental noise | Neural speech enhancement | Preserving speech does not imply selecting the desired person. |
| Another person or TV dialogue, especially during overlap | Personalized target-speaker extraction, conditioned on an enrolled voice | Needs speaker conditioning, reliable streaming output and tests for target absence/confusion. |

A voice-activity gate can stop transmission during silence, but cannot separate
two voices while open. Speaker-aware gating is a useful adjunct, not a substitute
for extraction during overlap. Microphone-array beamforming is a later hardware
option; the existing mono downmix does not preserve separate spatial channels.

## Candidate models and computational cost

These are upstream measurements or explicitly labeled estimates, not Thiscord
results. RTF is processing time divided by audio duration. Single-thread RTF 0.19
means about 190 ms of processing per second of audio, roughly 19% of one CPU core
on that test machine. It is neither 19% of the whole computer nor 190 ms of added
audio delay. Different machines/runtimes make cross-paper CPU rankings unreliable.
MAC/s counts multiply-accumulates per second of audio; it does not directly predict
CPU percentage. FP32 weight estimates below use four bytes per parameter and
exclude intermediate tensors, recurrent state, inference runtime and other models.

| Candidate | Intended use | Published cost / latency | Rust integration and decision |
| --- | --- | --- | --- |
| RNNoise through nnnoiseless | Lightweight general denoising baseline | No comparable CPU/RSS measurement established in this research; measure the exact crate/model rather than copying results from a different RNNoise generation. | Existing Rust implementation; inexpensive integration candidate, but not personalized speaker extraction. [Repository](https://github.com/jneem/nnnoiseless). |
| GTCRN | Very small general speech enhancer | Upstream corrected counts: **48.2K parameters, 33 million MAC/s**; about **0.19 MB** FP32 parameter storage by calculation. The paper's older 23.7K/39.6M counts use different accounting/operations. These are not CPU timings. | Streaming model/export available; validate its operations/state in a Rust runtime. Favor for a low-resource experiment, not as a proven TV-voice separator. [Official implementation](https://github.com/Xiaobin-Rong/gtcrn). |
| DeepFilterNet3 | Full-band general denoising | 2023 paper: **RTF 0.19**, single thread, **Intel i5-8250U**; **48 kHz**, 10 ms hop, **40 ms algorithmic latency**. Local memory/CPU still to measure. | Existing Rust/tract inference path makes it the first full-band quality candidate. No personal voice conditioning. [Paper](https://arxiv.org/html/2305.08227v1), [implementation](https://github.com/Rikorose/DeepFilterNet). |
| SpeakerBeam-SS, causal configuration d1 | Actual target-speaker extraction | Paper: **7.93M parameters**, **RTF 0.36** using one **AMD EPYC 7502P** core and C++ inference; **20 ms algorithmic latency** at **16 kHz**. About **31.7 MB** FP32 weights by calculation; total memory and enrollment costs need separate measurement. | Most relevant research direction for overlapping background voices. Not a ready Rust dependency: establish usable checkpoint/export/license and streaming inference parity before choosing it. [Paper, tables 1/2 and evaluation](https://arxiv.org/html/2407.01857v1). |
| DTLN-aec | Reference-aware neural echo cancellation comparison | Released variants: **1.8M / 3.9M / 10.4M parameters**, approximately **7.2 / 15.6 / 41.6 MB** FP32 weights by calculation. No cross-platform Rust timing established here. | Pretrained TFLite models; conversion/runtime work needed. Evaluate as an AEC alternative, not a drop-in residual stage or personal voice isolator. [Author repository](https://github.com/breizhn/DTLN-aec). |
| VoiceFilter-Lite | Personalized separation design reference | Google reports a **2.2 MB quantized model**; size alone does not establish CPU or end-to-end latency. | Enhances **speech-recognition features**, not an audible waveform. Not a drop-in call-audio solution; useful architectural reference only. [Google Research](https://research.google/blog/improving-on-device-speech-recognition-with-voicefilter-lite/). |

GTCRN's published ONNX integration uses 16 kHz and a 512-sample window; budget
resampling and framing explicitly. Do not preserve the unfiltered upper band as
a shortcut: that could leak the background being removed. Upsampling a 16 kHz
output does not recover the original full-band quality.
[Integration metadata](https://github.com/k2-fsa/sherpa-onnx/blob/master/scripts/gtcrn/add_meta_data.py).

DeepFilterNet2's separate paper reports RTF 0.04; do not apply that number to
DeepFilterNet3. Current upstream Rust inference explicitly rejects DFN2 models
and points to an older implementation. Pin the model and compatible runtime
together. [DFN2 paper](https://arxiv.org/abs/2205.05474),
[Rust model loader](https://github.com/Rikorose/DeepFilterNet/blob/main/libDF/src/tract.rs).

Start with CPU inference through existing Rust libraries or
[tract](https://github.com/sonos/tract). Exported ONNX does not guarantee operator,
state or performance compatibility. Do not silently adopt a native C++ inference
runtime behind Rust bindings. GPU acceleration, large offline separators and
training a model from scratch are outside the first implementation spike.
Audit code and checkpoint licenses separately and verify maintenance/build health
at the pinned revision; a published paper is not a redistributable working model.

## SpeakerBeam-SS feasibility review (2026-09-30)

**Status: source audit only; no SpeakerBeam-SS inference, quality tests or local
performance measurements have run. No SpeakerBeam-SS setting is implemented.**
The original authors' downloadable streaming checkpoint and matching inference
implementation were not located in this review. This is an availability finding,
not proof that no such release exists. The paper's causal d1 results above cannot
be attributed to a different implementation.

Reviewed artifacts:

- [OpenSpeakerBeam-SS](https://github.com/helloooideeeeea/OpenSpeakerBeam-SS/tree/bbbae73f74fc14eafdf7abb93f9094eafccd21df),
  commit `bbbae73f74fc14eafdf7abb93f9094eafccd21df`, is an independent
  reimplementation. Its README reports 7.64M parameters and 21.60 GFLOPs for one
  second of 16 kHz audio; neither establishes streaming latency or laptop CPU cost.
  It uses Resemblyzer speaker embeddings. The code is MIT licensed; its linked
  [checkpoint dataset](https://huggingface.co/datasets/helloidea/OpenSpeakerBeam-SS-dataset/tree/82e6d7646126d1fa33b30c0b44f1d076496cca71)
  has no separate license metadata/model card in the reviewed revision. Clarify
  the weights' redistribution terms before bundling them.
- [Another SpeakerBeam-SS checkpoint repository](https://huggingface.co/gabrielmbmb/voce-speakerbeam-ss-librimix-train360/tree/aeba4a5d8373116d856b2d6cab689c7b08dd1729)
  contains training checkpoints, but no model card, configuration, inference
  source or license was present in that revision. Its name alone does not establish
  compatibility with the paper or the independent implementation.
- [BUTSpeechFIT SpeakerBeam](https://github.com/BUTSpeechFIT/speakerbeam) is the
  earlier time-domain model released for the 2021 tutorial, not SpeakerBeam-SS.

The independent implementation has two concrete streaming obstacles:

1. Its [separator](https://github.com/helloooideeeeea/OpenSpeakerBeam-SS/blob/bbbae73f74fc14eafdf7abb93f9094eafccd21df/model/__init__.py)
   requests Asteroid `gLN` inside the convolutional blocks. Asteroid's
   [normalization](https://github.com/asteroid-team/asteroid/blob/c15708a04d3d28e9a1cd50553c456299dbc6d236/asteroid/masknn/norms.py)
   computes statistics over channels **and time**. The
   [causal flag](https://github.com/asteroid-team/asteroid/blob/c15708a04d3d28e9a1cd50553c456299dbc6d236/asteroid/masknn/convolutional.py)
   trims convolution padding; it does not replace that normalization. Consequently,
   future samples can change an earlier normalized activation. Chunking or replacing
   `gLN` changes inference semantics; quality must be revalidated and retraining may
   be necessary. This conclusion is from source inspection, not a model experiment.
2. Its [S4D forward path](https://github.com/helloooideeeeea/OpenSpeakerBeam-SS/blob/bbbae73f74fc14eafdf7abb93f9094eafccd21df/model/s4d.py)
   computes convolution over the supplied sequence and returns no recurrent state.
   Its [inference script](https://github.com/helloooideeeeea/OpenSpeakerBeam-SS/blob/bbbae73f74fc14eafdf7abb93f9094eafccd21df/inference.py)
   processes a whole waveform. A stateful implementation needs a parity check;
   independently processing 10 ms blocks would lose context. No ONNX artifact or
   export/streaming implementation was found in the reviewed source tree, despite
   the README mentioning ONNX support.

### Requirements to resume integration

Obtain compatible, redistributable extractor and enrollment-encoder weights,
architecture/configuration and a runnable streaming reference. Alternatively,
develop and validate a causal adaptation as a separate model-development effort.
Do not label such an adaptation as the paper's measured d1 model.

Once these inputs exist, reuse `CaptureStage`/`CaptureChain` for a separately
enabled personal-isolation stage. Keep enrollment independent of the denoiser
choice, with an optional DeepFilterNet3 composition evaluated in both orders.
Define state, latency and profile format/version explicitly. Run 48-to-16-to-48 kHz
resampling off callbacks with bounded buffers; do not mix unfiltered high-band
microphone audio back into the isolated output. Bind local profiles to the exact
encoder/model identity, and reject mismatches rather than silently bypassing.

Required regressions and benchmarks, in addition to the existing DSP suite:

- Reference-versus-Rust output parity and chunk-size invariance with persistent
  state; prefix causality (changing future input cannot change output already due).
- Different enrollment and target utterances; two-speaker overlap, similar voices,
  target absence, room reverberation and Turkish/English speech. Measure target
  preservation as well as interfering-speech attenuation; muting everything fails.
- Missing/corrupt/incompatible profiles, explicit bypass and stage combinations;
  reset after gaps/device changes, and immediate mute/PTT/leave under overload.
- Separate enrollment/startup costs from warmed continuous inference. Extend the
  existing offline benchmark with reference extraction quality, resampling/framing
  delay, p50/p95/p99/max frame time, RTF, deadline misses and peak memory. Measure
  release builds on Windows, macOS and Linux; paper timings are not acceptance data.

## Neural residual echo estimator extension (2026-10-02)

The desktop now has an **experimental, default-off Rust neural residual echo
estimator** inside Sonora AEC3. Conventional AEC3 remains the default. The new
checkbox is independent of Sonora/DeepFilterNet3 noise suppression. This targets
playback echo, including double-talk and speaker distortion; it does not isolate
an enrolled speaker or remove unrelated TV dialogue using the playback reference.

### Implementation and replacement boundary

Unmodified Sonora 0.2.0 omits this integration. Two small source patches, in
`vendor/sonora` and `vendor/sonora-aec3`, expose a mono `NeuralResidualEstimator`
trait and inject its residual-power estimates before suppression. These are
excluded third-party dependencies; the workspace still has exactly frontend,
backend and shared. See `vendor/THISCORD_PATCH.txt` for provenance and maintenance.

The frontend adapter lives in `audio/processing/neural_echo/`. It follows the
[WebRTC interface](https://webrtc.googlesource.com/src/+/526e228d25f83b1023760d3835f33d622c7b9f5f/api/audio/neural_residual_echo_estimator.h),
[feature extractor](https://webrtc.googlesource.com/src/+/526e228d25f83b1023760d3835f33d622c7b9f5f/modules/audio_processing/aec3/neural_residual_echo_estimator/neural_feature_extractor.cc)
and [model implementation](https://webrtc.googlesource.com/src/+/526e228d25f83b1023760d3835f33d622c7b9f5f/modules/audio_processing/aec3/neural_residual_echo_estimator/neural_residual_echo_estimator_impl.cc)
at revision `526e228d25f83b1023760d3835f33d622c7b9f5f`:

- Internal mono 16 kHz, 64-sample AEC blocks; aligned render and linear-cancelled
  inputs. The v2 upstream extractor leaves the `mic_frame` tensor zero.
- 256-point symmetric sqrt-Hann spectrum, 128-sample/8 ms hop, power compression
  exponent 0.15 and normalization from APM's PCM16 float scale. Previous raw
  samples are preserved; masks are held between hops. No extra PCM output queue.
- 864-float recurrent state, decayed by 0.999 after inference. Two 129-bin masks
  become 65-bin residual power estimates; dominant near-end uses the unbounded
  mask. Neural history advances during adaptation; estimates replace conventional
  ones only when the linear canceller is usable. The adapter supplies neural
  suppression tuning while installed, including during initial adaptation.
- Optional 12 ms reference headroom when estimated delay and buffered history
  permit. Queue gaps/configuration rebuilds reset and reattach the adapter.
- A bounded, hash-pinned graph executor supports this model's 11 operations and
  hybrid INT8 dense layers. `tract-tflite = 0.21.4` is used for schema parsing;
  its unsupported general importer is not used and DFN's tract pins stay intact.
  Buffers/index maps/FFT plans are allocated at startup, inference on the worker.
- An inference failure latches, zeros capture and stops audio through the existing
  error path. A gap/reset cannot silently clear that failure. No PCM, tensor
  state, SDP, credentials or model file contents are logged or sent over IPC.

This Rust extension avoids another native runtime and a C++ build/FFI surface.
It does not port every feature of current C++ AEC3 or claim full end-to-end C++
bit parity. To replace the estimator, implement the Sonora trait and adapt the
frontend constructor; to change graph execution, replace `neural_echo/model.rs`.
The existing `CaptureChain` remains the independent denoiser composition point.
Building `native-audio` without `neural-echo` excludes this inference dependency;
requesting the unavailable mode returns an error. Desktop includes the feature.

### Bundled model and controls

Open Audio & voice (also during a call), enable Echo cancellation and **Neural residual
echo estimation (experimental)**, then start a microphone test or join voice.
Version 0.1.7 embeds `frontend/models/ree-v2.tflite` in every native client using
`include_bytes!`; no resource path resolution or runtime download is required.
Size/hash validation runs for embedded bytes and optional local files.
Model selection is prepared in the background during calls; Echo cancellation
and the neural checkbox can bypass/re-enable it live. Disable the neural checkbox
to return to conventional AEC3. Existing saved settings default off.
Leave the optional override under Advanced model settings empty to use the bundled
model. Existing explicit overrides are preserved and validated; clear an obsolete
path to return to the bundled model. Backend/SFU setup is unchanged.

The tested candidate comes from an
[unverified third-party mirror](https://huggingface.co/dejanseo/chrome_models/tree/7713774a49fddeae5d620d897b2a01f6204a1294/71/63922A0C010C80A5/BA3548C2C434AE16).
It is 425,264 bytes, SHA-256
`3a18833eaeb08bfffb88a588c10db67885f246ba4794fd0f1609f2ccf6c30b77`.
Only this graph is accepted; its hash fixes tensor contracts and supported
operators. The hash does **not** authenticate provenance or establish permission
to redistribute the weights. Version 0.1.7 includes the model at the project owner's
request. Provenance and distribution terms remain unverified; no model license is
asserted here. Source and hash are recorded in `frontend/models/ree-v2.txt` and the
shipped `THIRD_PARTY_AUDIO.txt` notice. Builds and clients do not fetch weights.

### Reference validation and regression coverage

`frontend/tests/support/neural_echo_reference.py` generates 100 recurrent test
steps with nonzero features, startup silence and a silence tail using the official
`ai-edge-litert==2.2.0` interpreter (no delegates, one thread). Python is an offline
validation tool only; the application and builds do not need it. Float bit
patterns prevent JSON rounding from contaminating comparisons. The Rust probe
checks full recurrent rollout and transitions given identical reference state,
with a maximum absolute-error gate of 0.0001 for outputs and state.

Windows release result: maximum mask error `1.79e-7`; maximum recurrent-rollout
error including state `2.29e-5`. A regression specifically covers rounding the
hybrid quantized input **before** adding its zero-point offset, which otherwise
caused recurrent drift. This validates graph inference, not acoustic quality or
an entire C++ AEC3 pipeline.

```powershell
# Generate the independent reference in a disposable Python environment:
python -m pip install ai-edge-litert==2.2.0 numpy
python frontend/tests/support/neural_echo_reference.py frontend/models/ree-v2.tflite reference.json
cargo run -p thiscord-frontend --release --locked --features neural-echo-probe --example neural_echo_probe -- frontend/models/ree-v2.tflite 1000 reference.json

# The normal native test suite exercises the embedded model (also in CI):
cargo test -p thiscord-frontend --lib --features deep-filter,neural-echo --release --locked
cargo run -p thiscord-frontend --release --locked --features deep-filter,neural-echo --example neural_echo_bench -- frontend/models/ree-v2.tflite
```

Regular CI tests cover bounded/corrupt model rejection, quantization, mask
conversion, injection of aligned internal AEC blocks, settings compatibility,
mode-switch restrictions and latched failure/silencing. Bundled-model tests
cover reset determinism, nonfinite input rejection, bypass/re-enable, NS/AGC
composition, echo energy reduction and retention of speech during double-talk.
The benchmark compares conventional/neural AEC, linear/clipped speakers and
DeepFilterNet3 on/off using the same synthetic speech and room response.

### Local release performance and remaining acceptance

Windows x64, Intel i7-13700K, Rust 1.98.1: model-only mean **0.099 ms**, p99
**0.187 ms** per 8 ms inference hop (1,000 measured steps, 20 warm-up, load 2.13 ms).
For the 16-second simulated room, complete render+capture processing per 10 ms:

| Mode | Mean ms, linear / clipped speaker | p99 ms, linear / clipped speaker |
| --- | --- | --- |
| Conventional AEC3 | 0.054 / 0.052 | 0.132 / 0.147 |
| Neural AEC3 | 0.182 / 0.217 | 0.366 / 0.501 |
| Conventional AEC3 + DFN3 | 0.450 / 0.365 | 1.127 / 1.259 |
| Neural AEC3 + DFN3 | 0.588 / 0.822 | 1.433 / 2.133 |

The WSL/Linux x64 run matched the same reference errors. Model-only mean was
0.107 ms (p99 0.319 ms). Neural AEC3 + DFN3 measured 0.695/0.890 ms mean and
1.752/2.166 ms p99 for linear/clipped speakers; these WSL timings are informational
and not representative of a native Linux laptop. All 44 native tests, including
the two model-dependent tests, passed on Windows and Linux.

No 10 ms deadline misses in the eight runs on either OS. Timings exclude device callbacks,
networking and Opus; they are local measurements, not a slow-device guarantee.
The double-talk correlated local-speech gain improved from 0.378 to 0.541 with a
linear speaker, and 0.408 to 0.510 with clipping (AEC without DFN). Correlation is
only a diagnostic: delay search is coarse and cannot establish intelligibility,
sound quality or real-room echo performance. Very high synthetic far-end
correlation reductions are not claimed as acoustic attenuation results.

Remain default-off until real double-talk listening tests, changing echo paths,
reference timing drift, AGC/denoiser combinations, peak memory, thermal/battery
load and slow Windows/macOS/Linux hardware are evaluated. A Windows/WSL software
probe does not establish macOS microphone or acoustic compatibility.

## Recommended sequence

1. Establish the acoustic fixtures and benchmark runner, preserving Sonora as the
   control. Separate noise, competing speech and echo scores. This work comes first
   so a model cannot appear successful merely by muting the microphone.
2. Compare DeepFilterNet3 against nnnoiseless and GTCRN. Select a general-denoising
   candidate based on listening quality, streaming deadlines and full pipeline cost.
3. In a separate feasibility spike, evaluate SpeakerBeam-SS or an equivalent
   streaming waveform-output personalized model. Confirm available weights and a
   runnable inference path before scheduling product integration. Measure enrollment
   and speaker-encoder cost separately from continuous extraction.
4. Preserve a dedicated echo investigation: real double-talk, timestamp drift,
   room changes, speaker distortion and clipping. Compare a reference-aware neural
   approach only after the reference path is validated. Generic noise suppression
   should not be assumed to fix echo during simultaneous speech.
5. Integrate the winner as an optional mode, then run three-platform acoustic and
   performance acceptance before considering a default change.

## Proposed architecture and resource gates

Process the local microphone once on the sender. Keep the SFU forwarding encoded
media and keep per-remote-speaker playback rings and volume controls independent.
Do not run a separate isolation network for every received stream.

Initial pipeline experiment: timestamped capture -> AEC3 -> one selected enhancer
or personalized extractor -> optional gain/limiter -> transmit gate -> Opus.
Disable duplicate conventional noise suppression when comparing neural enhancers;
test composition explicitly before stacking them. A replacement neural AEC needs
its own correctly aligned reference path. Preserve the actual playback reference
before microphone enhancement; include resampling/model buffering in timing tests.

Preload and warm models before capture. Run inference off CPAL callbacks on a
bounded worker, with reusable buffers and explicit recurrent-state reset after
gaps, device changes and suspend. Preserve immediate mute, PTT release, leave and
access revocation even during overload. Bound backlog rather than playing or
transmitting stale audio. Keep PCM out of Tauri IPC, logs and the backend.

Initial **targets to validate**, not measured guarantees:

- General-denoising mode: RTF <= 0.20 on the slowest supported test laptop;
  added algorithmic delay <= 40 ms, preferably <= 20 ms.
- Personalized mode: explore RTF <= 0.40 and <= 40 ms algorithmic delay; tighten
  or reject based on actual laptop thermals and call quality.
- Both: p99 processing below half the hop duration under normal call load,
  incremental peak resident memory <= 150 MB and model initialization outside
  active capture. Measure resampling, feature extraction and synthesis too.
- Release benchmark on Windows x64, macOS Apple Silicon and Linux x64, plus any
  other shipped architecture. Include eight-participant playback, CPU contention,
  long calls, power-saving modes and battery/thermal effects.

Record hardware, OS, model hash, release build flags, threads, cold/warm startup,
p50/p95/p99/max frame time, RTF, memory, queue drops, resets and end-to-end added
latency. If targets fail, first evaluate a smaller model or supported quantization;
repeat listening tests for every optimization. Do not infer speed from weight size.

## Voice profiles, failures and user controls

Personal isolation needs explicit local enrollment, with a quiet-speech quality
check, preview, re-enrollment and deletion. Store only the necessary embedding
with OS-backed protection; discard enrollment PCM after processing. Never upload
profiles or use them as account authentication. A model must not silently switch
to another person when the enrolled speaker is absent.

Distinguish noise suppression from personal isolation in settings. Let the user
test both speech and silence and choose strength. On missing/corrupt models,
invalid profiles or repeated deadline misses, show the failure and apply a defined
policy. A mode promising personal isolation should stop transmission until the
user chooses a fallback, rather than silently send unfiltered room speech.

## Quality acceptance

Use consented, reproducible recordings/fixtures; no automatic recording of calls.
Cover fans, typing, music, external TV dialogue, nearby speakers, target absence,
similar voices, whispers, laughter and overlapping speech. Include Turkish and
English, multiple accents, distances and reverberant rooms. For echo, test
far-end-only and double-talk separately, gain on/off and nonlinear loudspeakers.

Report background attenuation alongside target-speech distortion, intelligibility,
first-syllable loss and mistaken suppression. Use clean-reference SI-SDR/STOI where
appropriate plus blind listening; report echo ERLE for appropriate single-talk
segments rather than treating it as a double-talk intelligibility score.

Compare against the current Sonora pipeline and Discord with matched hardware,
levels and documented settings/version. Report both CPU and listening results;
do not infer product superiority from a paper benchmark or synthetic tones.
Promote a candidate only after reproducible improvement in the intended scenarios,
no material target-speech regression and passing the resource/lifecycle gates.
