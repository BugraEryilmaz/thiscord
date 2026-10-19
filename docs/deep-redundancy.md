# Experimental Opus deep redundancy

**Integration gate:** this change depends on RED's reviewed publisher-generation,
source-epoch and privacy-epoch protocol. The standalone main-based branch is not
merge-ready. Same-account replacement and remote restrictions must reset the
decoder and pending PCM even when RTP sequence/timestamps remain continuous.
Legacy epoch-zero sources must use ordinary Opus fallback, with no DRED recovery.
The parent will integrate and re-review these boundaries before marking ready.

The `deep_redundancy` preference defaults to false, survives `audio.json`, and uses
the existing local and admin settings command paths. Audio & voice → Advanced &
troubleshooting labels it experimental and explains bandwidth, CPU and delay.
Native workers do the encoding, parsing and neural synthesis. No callback/IPC
work or media logging is added. Browser voice remains unsupported.

Enabling requests six 10 ms units of encoder history, raises the total Opus VBR
target from 32 to 40 kbit/s and uses a 20% expected-loss hint. This hint is a policy
input, not a measured loss report: libopus 1.6.1 can allocate too few DRED chunks
at 5% and emit no extension. Disabling restores 32 kbit/s and 5% on this standalone
branch. Adaptive-FEC integration must instead restore the adaptive hint when
adaptive FEC remains enabled, and preserve a 20% floor while DRED is enabled.
Neither a successful CTL nor a successful ordinary FEC call proves recovery.

The receive buffer minimum is 80 ms instead of 40 ms while enabled. Device buffer
size and jitter can require more, up to the existing adaptive limit. Disabling
explicitly restores the ordinary target; it does not wait for slow adaptive decay.
This is scheduling delay, not device-to-ear latency. The existing microphone gate
still adds 60 ms in automatic mode. DRED does not recover a second of speech with
no latency. At most three missing 20 ms intervals can use a queued successor,
only before the existing playout deadline. Earlier losses, late donors or absent
features use ordinary adjacent FEC or PLC. Coverage is content/bit-budget dependent.

The nearest successor must have an exact sequence/timestamp hole matching the
missing intervals. Talkspurt markers, no-data cues, explicit DTX, fractional gaps,
and timestamp-only omissions cannot donate. Cursor advancement occurs once;
late originals and duplicate donations cannot replay completed intervals.
The packet cap stays 12, packet age stays 200 ms, history parsing is capped at
2,880 samples, and enabled encoder output is capped at 1,100 bytes. At most the
existing device-sized refill budget (2–6 decisions) runs per worker service.
Each DRED parser/feature object is owned and freed within one decision. No extra
packet/PCM history survives between calls. Neural synthesis history remains in
the source's Opus decoder; `OPUS_RESET_STATE` clears LPCNet, while the encoder's
reset clears DRED latents and neural state. Settings/deafen/device resets retain
the existing callback-safe PCM generation invalidation.

Diagnostics expose `dred_attempts` and `dred_output_frames` in existing technical
Opus decode logs. Output counts mean a covered interval was synthesized from DRED,
not sample-exact audio or proof of intelligibility. Recovery never refreshes the
speaker activity indicator. No unbounded parser cache, shared neural state or
application-owned unsafe FFI is added.

## Compatibility and combinations

DRED is an extension inside Opus code-3 padding. Ordinary receivers ignore that
padding and decode the primary audio. Compatible enabled receivers parse the
extension; absence/incompatible versions fall back safely. No separate RTP codec
payload type or SDP DRED codec negotiation is necessary. Both encoder and receiver
must enable this preference for DRED recovery. The source/privacy fence protocol
is independently required; RTP/WS boundary negotiation is supplied by the RED
dependency and must work even when the RED preference is false.

RED integration uses PT112 and at most two previous frames, with 160 bytes per
repeated block and 1,100 bytes total. Its padding check excludes DRED-bearing
packets from repeated history, so RED must not repeat DRED history three times.
Packets without padding may still receive ordinary RED protection; oversized
primaries use raw Opus. Enabled DRED does not imply three copies, additive
recovery percentages, or guaranteed benefit from enabling all options. Tests
verify real DRED packets carry the code-3 padding flag and fit the payload cap.

## Source, models and build

`vendor/opus` is the MIT/Apache-2.0 `opus 0.4.0` safe wrapper with a narrow owned
DRED extension. Its dependency is exactly `opusic-sys 0.7.5`, whose bundled
`opus/package_version` is 1.6.1. Cargo.lock authenticates the crate archive.
That crate's `dred` feature sets CMake `OPUS_DRED=ON`, which also builds deep PLC.
The existing CMake/C compiler infrastructure works across the Windows x64,
Linux x64 and macOS ARM64 audio matrix; no Python model tooling, network model
fetch, dynamic system Opus, new workspace package or npm dependency is added.
Decoder complexity remains zero while off; enabled recovery uses complexity 5.

The bundled generated model arrays are distributed with upstream libopus under
its BSD-style COPYING notice, reproduced in the shipped THIRD_PARTY_AUDIO.txt.
No separate model license appears in that packaged tree. DRED arrays identify
their training checkpoint as `checkpoint_epoch_1.pth`; this is upstream provenance,
not a locally retrained model. SHA-256 of the packaged C arrays:

```
dred_rdovae_enc_data.c 3bf6d5cbfa3b1fee99a0e65253eeecaa92f861533b9c3926b494e2776c922e5e
dred_rdovae_dec_data.c a8389b6556a75712eafde8733fba57a6efac80700badd6aed25f3fffe597a202
fargan_data.c          69ceccdfc11c15af1f00701357795cc73187ed227c0f2215da23bc01ee996a7b
plc_data.c             e1d61885ba91d7f700e7be9f0a810202dcfc4ba980b75726f2e925a23dc7353b
pitchdnn_data.c        921b6157ff7a6200741c8b3e0c6d0183f2c34567297d63b065486be2bbf995ac
```

Reviewed the [official 1.6 decoder API](https://opus-codec.org/docs/opus_api-1.6/group__opus__decoder.html)
against the pinned C implementation, including `dred_end` silence coverage and
reset behavior. The [historical 1.5 demo](https://opus-codec.org/demo/opus-1.5/)
explains the redundancy/latency tradeoff but is not a 1.6.1 compatibility guarantee.
The built-in versioned parser is authoritative for compatible padding data.

## Deterministic evaluation

The existing synthetic speech fixture is 7.38 seconds (369 frames). At the fixed
policies above it produced 34,421 DRED bytes (37.3 kbit/s) versus 25,445 ordinary
Opus bytes (27.6 kbit/s), a 35.3% payload increase. RTP/SRTP/IP overhead is excluded.
Isolated losses recovered 13/15 intervals, two-frame bursts 26/30, and three-frame
bursts 27/45. The tests separately compare against neural PLC with the same
packets and prove output from donors beyond the immediate predecessor; they do
not treat fallback PLC as DRED recovery. Uncovered intervals remain fallback.

Production `Remote::refill` tests exercise real scheduling/mixer output with an
80 ms first-decode deadline, three-frame burst recovery, late-donor fallback,
deafen flush, settings target restoration and source receive-time fences. Other
tests cover DTX, disabled output equality, wraps, malformed inputs, payload bounds,
padding, and native encoder/neural decoder reset equivalence to fresh objects.
The publisher/roster epoch race tests remain a required integration gate above.

`codec_worker_cost_is_measured_without_device_or_network_claims` reports optimized
worker encode/decode totals and maximum decode call time; timing is informational,
not a flaky deadline assertion. Run with `--nocapture --test-threads=1` for local
numbers. Hardware, many-speaker contention, intelligibility, listening quality and
macOS/Linux physical device acceptance require separate evaluation.

One Windows x64 optimized serial run measured 369 frames: baseline encode 61.8 ms
and decode 7.85 ms; DRED encode 74.6 ms and repeated parse/synthesis 48.7 ms,
with 283 covered DRED outputs and a 0.60 ms maximum decode call. This is an
informational synthetic worker workload, not a many-speaker CPU budget guarantee.
The production three-loss test recovered two frames and used PLC for one; with
late donors it recovered zero and used PLC for three. First decode was 80 ms in
both cases. CI retains this report separately on each native audio platform.

## Review and local validation (2026-10-19)

Independent native reviewers `/root/dred_correctness_review` (three rounds) and
`/root/dred_lifecycle_review` (two rounds) inspected the actual working tree.
Findings fixed: disabling retained the 80 ms target; missing production scheduling
coverage; loose burst-wide deadline assertion; and stderr omitted from the CI
measurement artifact. The final timing test checks each recovered slot's own
deadline and injects/rejects donors 160 ms late. The correctness review's third
round found no additional actionable core issue. Both reviews leave the source/
privacy/legacy integration gate unresolved; this is not merge approval.

Windows checks: all 237 audio/library tests and two replay tests pass with
`deep-filter,neural-echo-probe`, optimized `ci` profile; all eight dedicated DRED
tests pass. Audio Clippy (lib/tests/examples, same features/profile, warnings
denied), WASM `thiscord-ui` check, shared tests (18 unit + 17 integration), native
desktop build, desktop Clippy with warnings denied, and formatting/diff checks
pass. The desktop link emits the
existing OpenH264 LNK4255 duplicate-object debug-symbol warning. Compiler cache
collisions initially returned another checkout's shared settings; final checks
ran in a coordinated exclusive window after refreshing local source timestamps,
without cache cleaning or task-specific build directories.

WSL `kali-linux`, using the configured isolated test database and including ignored
tests: 62 backend unit tests and accounts (7), admin web (1), chat (20), HTTP (3),
permissions (7), PostgreSQL migration (1) and TLS (1) integration tests pass.
The initial voice runs inherited deployment configuration and failed at
`backend/tests/voice.rs:168` with `EOF while parsing a value` after about 15 seconds.
Root reproduced the isolated `sfu_forwarding_permissions_isolation_and_cleanup`
failure on clean main `de9ac41f6689394d7aeaf85f87df28589505b6f0` in 15.24 seconds
with deployment configuration; the same clean-base test passed in 23.44 seconds
with only `TEST_DATABASE_URL` supplied and every `THISCORD_*` variable omitted.
This establishes an environment-dependent baseline failure, without identifying
one particular deployment setting as its cause.

Root then rebuilt exact DRED commit
`162d3ce7009f85fb3975292ba11b0f6560ccb3b9` in that isolated test environment,
preventing the test dotenv path from loading deployment settings:

```sh
cargo test -p thiscord-backend --test voice --locked -- --include-ignored --test-threads=1
```

All five voice integration tests passed, zero failed, in 85.74 seconds after
54.49 seconds of compilation. Final outputs used
`~/.cache/thiscord-targets/t3-668100c8`; the shared intermediate cache remained
`~/.cargo/thiscord`. No secret values are recorded. Evidence is retained outside
the repository at
`C:/Users/bugra/AppData/Local/Temp/thiscord-dred-voice-isolated.log`; the runner is
`thiscord-voice-loopback-check.py` in that directory (checkout path and `all`
arguments). No backend/test-protocol edit was needed for this validation.

This resolves the standalone voice-test validation limitation only. The PR stays
draft: RED/FEC integration, publisher/source/receiver privacy fences, legacy
fallback, combined race tests and renewed independent review remain required.
