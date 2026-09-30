# Voice isolation research and implementation plan

Research date: 2026-09-30. DeepFilterNet3 is now implemented as an experimental
denoiser. See [integration and local measurements](deep-filter.md). Other model
candidates and personalized speaker extraction remain research work.
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
