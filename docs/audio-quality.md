# Local voice quality diagnostics

These metrics describe local observations, not a recording or an acoustic score.
They never contain account IDs, names, payloads, SDP, addresses or model paths.
The connection view and advanced details work with diagnostic logging disabled.
The existing opt-in logger is the only diagnostic file writer.

## Definitions (established before instrumentation)

Application counters are unsigned, saturating worker aggregates; the WebRTC loss estimate is signed. Totals reset on a new
audio session/connection; stream totals reset when a slot changes source. Device
replacement retains session totals. Ten one-second buckets hold recent counts
and timing histograms (current partial second plus nine preceding seconds).
Expiry uses monotonic time, including when no events arrive. Rates use the
reported window milliseconds, not the lifetime of the call. A zero denominator
means unavailable, never 0% loss. Snapshots are approximate across threads.

| Metric | Unit / scope | Meaning and denominator |
| --- | --- | --- |
| Capture / reference overflow | mono samples / device session | Samples rejected by the respective 120 ms queue; divide by input / output samples respectively. |
| Input / output clipped | mono samples / device session | Input magnitude >=0.999 after downmix (a near-full-scale indicator); output exceeding full scale before hard clipping, after gains, excluding deafen. Divide by input / output samples. Not a measurement of physical device clipping. |
| Output samples | mono sample frames / device session | Frames rendered, independent of device channel count. |
| Outgoing offered, full, expired, invalidated, sent, send failed | packets / connection | One offered packet per encoder result; each rejection has one reason. Expiry is >100 ms since enqueue. Invalidated means closed connection or tightened stop/mute/deafen/PTT controls. Sent means accepted by WebRTC, not delivered. Denominator: offered packets. |
| Incoming application queue drops | packets / connection | Local media queue rejected packets; separate from network statistics. |
| Received / duplicate / reordered / late | packets / source | Jitter input; duplicate means seen in the bounded 64-sequence history; reordered means unseen and behind the highest arrival; late means older than playout cursor. Duplicate and late are disjoint; reordered may also be late. Denominator: received packets. |
| Jitter overflow / resync discard / invalid payload | packets / source | Local buffer policy drops, never network loss. Denominator: received packets. |
| Sequence gaps | packets / source | Missing sequence at playout with a later packet already buffered; not empty-tail PLC, silence or DTX timestamp gaps. Attribution across sender/SFU/network/application is incomplete. Not an end-to-end network loss estimate. |
| FEC attempts / verified recoveries | packets / source | Current decoder never requests FEC: attempts zero, verified recovery unavailable. Future FEC callers must report attempts independently from observable recovery. Successful decode alone does not prove FEC. |
| PLC | mono samples / source | Samples actually returned by empty-input Opus decoding, including tail concealment. Display ms = samples / 48. Denominator: decoded samples. PLC is not proof of packet loss or audible failure. |
| Decode errors / PCM overflow | packets / samples / source | Codec failures and decoded samples rejected by the local PCM ring. |
| Playback starvation | missing mono samples / source | Empty PCM while playout has started and packets remain buffered for decoding. Excludes deafen, inactive slots and zero gains. Local buffering/scheduling evidence, not a device xrun or proof of an audible glitch. |
| Unclassified empty playback | mono samples / source | Empty active queue without buffered-media evidence; may be remote inactivity, DTX, network interruption or scheduling. Never added to loss/starvation. |
| Intentional silence | mono samples / source | Inactive/deafened/zero-gain output slots; no loss/starvation. Muted microphone affects sending, not listening. |
| PCM policy discard | mono samples / source | Backlog trimmed or invalidated by source/gain/deafen controls; separate from empty queues. |
| Queue occupancy | packets, blocks, mono samples | Instantaneous jitter, capture/reference and PCM depths. Playout buffer ms = jitter packets * 20 + PCM samples / 48; excludes devices, network, codec/DSP delay, not end-to-end latency. |
| DSP capture / render, encode / decode duration | microseconds / invocation | Worker wall duration, including preemption; 10 ms DSP and 20 ms codec budgets. Miss is strictly greater than budget. Denominator: timed calls. Errors are timed too. |
| Timing percentiles | microseconds / recent calls | Fixed upper-bound histogram bins; p50/p95/p99 unavailable with no observations. Last bin reports the observed max. No event history. |
| SFU receiver loss report | signed packets / connection | WebRTC audio receive and loss estimates summed over inbound audio tracks, cumulative since peer creation. Duplicates can make loss negative. The forwarded sequence space may retain upstream gaps: this is not isolated link loss. Expires after three seconds. Denominator for an estimated fraction would be received + max(lost, 0), not decoded/PLC frames; no percentage is presented as end-to-end loss. |
| SFU-hop RTT | milliseconds / connection | Selected nominated ICE-pair WebRTC RTT, expires after three seconds. Includes shared transport; not speaker-to-listener latency. |
| End-to-end network loss / latency, limiter activity | unavailable | SFU remaps RTP continuity on source changes; receiver gaps cannot locate which hop/application dropped a packet. Neither receiver reports nor PLC establish end-to-end loss. No standalone instrumented limiter; hard output clipping is reported separately. |

Callbacks batch sample counts into atomics once per callback/stream; a wrap
latches saturation without a CAS retry loop. Callback events enter rolling
buckets when drained by the worker, so a stalled worker delays their observation.
They never take a metrics lock, allocate, log, or compute percentiles. The worker
drains these counters; aggregation, fixed-window expiry and presentation run off
callbacks. Native `quality::Metrics::add`, `timed`, and `snapshot` are a small
audio-specific interface for future jitter/DTX/FEC work. No telemetry exporter,
event history, background recording or new persistent preference is introduced.

Physical multi-user listening, real network impairment, long calls under CPU
contention, and native macOS/Linux device validation remain acceptance work.

## Verification on Windows (2026-10-13)

The audited 81-test baseline passed before editing. Targeted native tests, the
individual `deep-filter` and `neural-echo` feature builds, and the combined
`deep-filter,neural-echo-probe` suite passed; the final combined run selected 110
tests. Shared contract tests, native desktop and WASM UI checks, targeted Clippy
(including audio tests/examples), formatting and whitespace checks passed.
CI already runs these audio tests on its three native platforms; those platforms
were not run locally for this change.

The optimized `ci` profile's synthetic eight-stream mixer measured 0.069 ms per
20 ms block. The bounded instrumentation test processed 24,000 packet/timing
pairs in 2.9861 ms, including periodic snapshots, with 8,984 bytes of fixed metric
storage per source. These are informational measurements on this Windows host,
not a before/after comparison, slow-device guarantee or acoustic result.

Reproduce the main checks with the pinned toolchain and existing build directory:

```powershell
cargo test -p thiscord-frontend --lib --features native-audio --locked audio:: -- --test-threads=2
cargo test -p thiscord-frontend --lib --features deep-filter,neural-echo-probe --profile ci --locked audio -- --test-threads=2
cargo clippy -p thiscord-frontend --lib --tests --examples --features deep-filter,neural-echo-probe --profile ci --locked -- -D warnings
cargo run -p thiscord-frontend --example audio_bench --features native-audio --profile ci --locked
cargo fmt --all -- --check
```
