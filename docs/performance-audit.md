# Performance audit and operational measurements

Audited 2026-10-22. This is a ranking of expected impact from the current code,
not a claim that production is slow or that a speedup has been measured. Start
with normal traffic, then compare idle, busy-chat and multi-person video periods.
Keep hardware, build profile and approximate concurrency comparable. No user
recordings, exports, message content or manually installed profilers are needed.

Reconciled with `main` through `22451b8`: the [October 21 incident investigation](performance-2026-10-21.md)
found low host/API load and addressed signaling recovery, audio-startup waits,
TCP_NODELAY, preflight caching and repeated navigation. Those fixes are retained;
the ranking below concerns remaining scaling costs, not an alternative attribution
of that incident. Notification expiry now uses the database's remaining lifetime
anchored to a monotonic clock; worker timing preserves that behavior. New attachment
upload/download throughput is outside this audit's measurements, while chat merge
and row geometry counters include updates to attachment-bearing messages.

The owner's [voice test bots](voice-bots.md) can generate controlled SFU load
without user recordings. Their admin controls remain separate from desktop audio
support: bots have no WebView and therefore do not supply UI measurements.

## Findings, highest expected impact first

1. **Recurring friends and voice-roster database work.**
   `backend/src/friends/mod.rs` polled once a second per connected friends socket;
   `friends/store.rs::snapshot` reauthenticates, locks relationships, reloads all
   friend profiles and, when a conversation is selected, reloads its history.
   Since 2026-10-28 that read runs on commit wake-ups, call expiry and at least
   every ten seconds, with presence/typing refreshed in memory (see
   [friends](friends.md)). `chat/session.rs` calls
   `chat/store.rs::visible_voice_channels` for roster subscribers. Since 2026-10-28
   it reuses that result with its access epochs for up to ten seconds (see
   [chat](chat.md)), removing about 90% of roster authorization transactions;
   the one-second in-memory SFU roster read remains. Idle costs still grow with
   connected clients, now at about a tenth of the previous rate.
   Read **Friends snapshot**, **Voice roster authorization**, unchanged-poll
   totals and **Chat/friends/notification worker queue**. High unchanged ratios together
   with sustained elapsed ms/s make event-driven invalidation worth prioritizing.
   Friends admission skips show pressure hidden by the current skip-on-full policy.
   Any optimization must retain expiry checks, authoritative permissions and
   revocation; caching a client permission snapshot is not an acceptable fix.

2. **Five shared database connections amplify waiting and long transactions.**
   `backend/src/db.rs` limits the pool to five connections. HTTP, sockets,
   notifications and diagnostics compete for it. `auth/store.rs::query` also
   materializes PostgreSQL JSON and decodes it again into Rust DTOs. SQL work and
   row-lock waits can hold a connection while other operations wait up to two
   seconds. Read **Database pool checkout**, checkout failures, **SQL query + JSON
   decode**, **SQL execute / locks**, and open/idle connection counts. Slow checkout
   with quick queries suggests concurrency/connection occupancy; slow execute
   may include lock waits. These aggregate timings cannot isolate a particular SQL
   statement or prove CPU saturation. Profile the implicated operation before
   changing pool size or transaction boundaries; do not remove authorization locks.

3. **SFU forwarding scales with publishers × recipients × packet rate.**
   The relay in `backend/src/voice/mod.rs` takes the room read lock, validates and
   rewrites a packet, walks recipients, clones packet metadata and handles screen
   layer queues while holding that lock. Video increases packet rate; sustained
   fanout can delay room writers and back up ingress. Read **SFU ingress queue
   age**, **SFU room read wait**, **SFU locked fanout**, and the existing participant
   video-drop/send-timeout counters. New timings deterministically sample every
   64th ingress packet per relay; they are sample counts, not packet totals, and
   may miss short bursts. Queue age is arrival-to-dequeue, read wait is separate,
   and fanout ends before async keyframe recovery. No locks, allocations or logs
   are added by the timing recorder. Do not move permission checks outside the
   delivery boundary to reduce timings.

4. **Exact unread counts still scale with unread backlog and subscribers.**
   `chat/store.rs::unread` evaluates accessible channels and counts each live
   unread range. The bounded range scan and event-driven chat refresh are already
   implemented; there is no remaining unconditional unread scan every 750 ms on
   the current socket protocol. Large backlogs still cost proportional work per
   refreshing subscriber. Read **Unread authorization + counts**, **Chat refresh**
   and **Focused permission inputs**. The first includes channel evaluation and
   unread SQL, not the preceding permission-input query; `Chat refresh` includes
   checkout/authentication and its nested work. Use the existing synthetic
   [unread benchmark](unread-performance.md) before considering maintained counts.

5. **Frontend friend rows repeatedly clone/search the whole snapshot.**
   In `frontend/src/app/friends.rs`, sidebar and friend-list derived signals call
   `snapshot.get().friends.into_iter().find(...)`. Each evaluation clones the
   snapshot, including selected history; many row properties evaluate that
   derived signal. Aggregate work can approach quadratic growth in friend count
   per update, with additional history-clone cost. **Friend row snapshot lookup**
   measures those two row lookup sites, including the clone and search. Compare
   calls, accumulated time and visible timer lateness while viewing a large
   friends list. A follow-up can borrow with `with`, index by ID, and memoize rows.
   This audit deliberately preserves the behavior to measure its baseline.

6. **Native DSP can exceed real-time budgets on slower desktops.**
   Capture/render processing, optional neural inference and codec work run on
   workers in `frontend/src/audio`, with 10 ms capture/render and 20 ms codec
   budgets. Reuse the existing always-collected quality counters: deadline misses,
   p50/p95/p99/max, capture/reference overflow, playback starvation and transport
   drops. In `/admin`, select a participant → **Advanced & troubleshooting**.
   These are available through live audio status without enabling file logging.
   CPU-sensitive features and multiple remote streams can move this finding to
   first priority for an affected desktop. Compare stage deadlines with queue
   faults; a network loss counter alone does not establish a DSP problem.
   Since 2026-11-01 the worker is woken by device callbacks and producers instead
   of a 5 ms poll, batches queued packets, runs at MMCSS/QoS priority with MMCSS
   WASAPI callbacks on Windows, and non-48 kHz conversion and mixing skip
   per-tap modulo and inactive-stream work. These changes are unmeasured on real
   hardware; compare deadline misses and starvation before and after updating.
   Also since 2026-11-01: the denoiser/boost/AGC chain is skipped while nothing
   can be transmitted (muted, deafened, restricted or push-to-talk released) while
   echo cancellation keeps adapting; playback mixes 1 ms blocks per stream instead
   of popping one sample per stream; non-48 kHz playback no longer compares all 32
   stream slots per output sample; received packets no longer wake the worker;
   and only explicit/periodic status requests build the quality report. The
   voice-activation lookbehind still adds a constant 60 ms: sending its pre-roll
   as a burst would not lower latency unless receivers time-compress the talkspurt
   start, which would need listening tests.

7. **UI geometry work and the admin dashboard can stall the main thread.**
   Chat is already virtualized with a 500-message cache and batched merges.
   `app/chat.rs` still performs geometry reads in viewport updates and row resize
   callbacks, potentially forcing layout. **Chat batch merge** and **Chat layout /
   viewport** measure synchronous work, not browser paint or total frame latency.
   The admin dashboard reloads all channel names in SQL and recreates room views
   every five seconds; **Admin diagnostics collection** measures its backend cost.
   **Visible UI timer lateness** and **This admin browser's UI measurements** help
   distinguish frontend responsiveness from backend latency. Timer lateness is a
   screening signal: OS sleep and scheduling can also cause it.

## Follow-up changes (2026-11-03)

These changes address the findings above without new production measurements.
They are expected to reduce the named work; none of them has a measured speedup.
Compare the same monitors before and after deploying them.

- **Finding 1, friends history.** Snapshot reads keep their authentication,
  relationship locks, access filtering, call expiry and ten-second cadence, but
  reload the selected conversation's history only after the selection/page
  changes, a message send/edit/delete commits in that conversation, or the cached
  page is 60 seconds old (see [friends](friends.md)). New clients opt in with a
  socket query to receive history only when it changed; older clients still get
  it in every snapshot. **Friends snapshot** includes the remaining reads;
  **Friends unchanged** now counts reads where neither the snapshot nor the history
  revision changed.
- **Finding 2, pool size.** `DATABASE_POOL_SIZE` (1-32, default 5) configures the
  pool; checkout, statement and lock timeouts are unchanged (see
  [database](database.md)). The default stays five because no checkout-wait
  measurement has justified a different value.
- **Finding 3, SFU fan-out.** Packet-only parsing (screen layer and layer CSRCs)
  moved before the room read lock. Under the lock the relay still validates the
  source, rewrites the header, plans screen layers and admits each recipient,
  including both endpoints' access permits, viewer/route state and the screen byte
  budget. The mpsc enqueues, which can wake writer tasks, now run after the lock is
  released, synchronously and while the admitted permits are held. Access-mutation
  drains therefore still wait for every admitted packet, and egress writers still
  revalidate under the room lock before transport writes. **SFU locked fanout**
  measures the shorter locked admission only; enqueue time is not separately timed.
- **Finding 4, unread counts.** Counts and mentions are exact below 1,000 and
  capped at 1,000 otherwise (shown as "999+"), with each channel's scan limited to
  that many live rows. Mentions use a new partial index of mention-bearing messages.
  The synthetic local benchmark visits 40,000 instead of 200,000 message rows for a
  200,000-message backlog (see [unread performance](unread-performance.md)); this
  is one local measurement, not production latency.
- **Finding 5, friend rows.** A memoized map indexes friends by conversation ID.
  Rows, call cards and the open conversation read single entries through memos
  that clone only that friend; list filters, call lists and the conversation's
  history effect borrow the snapshot instead of cloning it, and history is cloned
  only for the matching conversation. **Friend row snapshot lookup** now times
  each row memo's map lookup and single-row clone, so its call count follows rows
  re-evaluated after a friends-list change rather than every derived read.
- **Finding 7, admin dashboard.** Diagnostics read channel names only for rooms
  live in the voice registry, instead of every channel, and skip that query when no
  room exists. The dashboard's room section re-renders only when room telemetry
  changes, so idle instances no longer rebuild it on every five-second refresh;
  host and performance sections still rebuild per refresh. Chat geometry reads in
  `app/chat.rs` are unchanged: no change was found that clearly avoids a forced
  layout without changing scroll anchoring behavior.

## Load-tested changes (2026-11-05)

Measured on the VPS with the load generators; method, tables and caveats are in
[load benchmarks](load-benchmarks.md).

- **Finding 1, voice roster authorization.** Voice connections no longer run a
  database authorization every second: the one-second roster reuses the last
  check while the connection's epoch is current, with a full check at least every
  ten seconds and after every access commit.
- **Finding 2, shared pool.** Application SQL is kept as per-connection prepared
  statements, so repeated statements skip parsing and usually planning (see
  [database](database.md)). The default pool stays at five connections; ten did
  not help the measured connect storms.
- **Chat fan-out (new).** Each subscriber of a channel previously re-authenticated,
  locked the guild and read the same new events. Deliveries now reuse an
  epoch-fenced full check for up to ten seconds and share one event read and one
  presence read per channel and publication; unread and presence deliveries are
  coalesced to once per second per subscription. Notification alerts reuse a
  recipient's check per channel. **Chat refresh (reused authorization)** times the
  reuse path separately from **Chat refresh**.
- **Connection limits (new).** `THISCORD_MAX_SOCKETS` (default 512, per socket type)
  and `THISCORD_MAX_VOICE_CONNECTIONS` (default 96) replace fixed 128/64 limits.
- **Database trouble (new).** Pool exhaustion and statement/lock timeouts no longer
  reach clients as revocation; see [chat](chat.md).

## Client media pipelines (2026-11-06)

Voice and video-share hot paths on the Windows, Linux and web clients (macOS not
covered). `media_hotpath_bench` measures the device- and network-free pieces with a
counting allocator:

```
cargo run -p thiscord-frontend --example media_hotpath_bench --features native-audio --profile ci --locked
```

Windows x64, i7-13700K, `ci` profile. "Before" is commit `0ae37e5` built with a
private `CARGO_BUILD_BUILD_DIR`: with the shared intermediate directory, another
checkout's build links the newer `thiscord-shared` and measures the new code.

| Operation (per packet or frame) | Before | After |
| --- | ---: | ---: |
| RED parse, two history blocks | 63 ns, 2 allocations | 11 ns, 0 |
| RED receive (parse + payload slices) | 107 ns, 2 allocations + copies | 48 ns, 0 (zero-copy slices) |
| RED encode | 97 ns, 3 allocations | 34 ns, 0.06 (pooled arena) |
| Opus send payload | 38 ns, 1 allocation | 23 ns, 0.02 (pooled arena) |
| H.264 keyframe check, 25 KB delta | 7.3 µs | 3 ns (header-only scan) |
| H.264 keyframe check, 300 KB IDR | 87 µs | 6 ns |
| H.264 NAL split, 25 KB | 7.3 µs | 2.4 µs (skipping start-code scan) |
| H.264 frame assembly, 23 packets | 834 ns, 7 allocations | 437 ns, 1 |
| Quality accounting, 5 counts + 1 timing | 216 ns (lock + clock read each) | 9–16 ns worker batch; 56 ns shared (send task) |
| Mixer, 20 ms with 1 / 8 talkers | 7.6 / 16.4 µs | unchanged |

Shared quality counters (the send task records every packet's outcome) are one
relaxed atomic add into pending cells, merged into the window by the worker's
existing tick once per second; the adaptive-FEC sent-sequence record takes its lock
only when the source generation changes. The mixer counts silence only for slots
that have a stream (an empty slot's counts were never drained and would have been
charged to the next stream to take it), and skips zero counter updates.
Per-stream and per-session quality accounting on the audio worker is batched:
counts and stage timings accumulate in worker-local cells and merge into the shared
ten-second window once per second per stream (and before every snapshot), so the
worker takes no lock and reads no extra clock per packet or frame. An atomic,
lock-free window was also tried and measured no faster (231 ns), because the cost
was the per-call clock read. The mixer callback is under 0.1% of its 20 ms budget
and was left alone. Every sent packet's RTP header still
needs one small CSRC `Vec`, which the webrtc-rs `Header` type owns.

`frontend/tests/hotpath_allocations.rs` turns the allocation counts into budgets
that fail CI: RED receive 0, pooled Opus/RED payloads under 0.1, keyframe and
front-unit scans 0, frame assembly at most 1, the mixer callback 0, and at most
8 per packetized frame. That last one replaces the webrtc-rs `H264Payloader`, which
allocated one buffer per FU-A fragment (274 for a 300 KB IDR); the app-owned
packetizer emits identical payloads (pinned against the old payloader) from
zero-copy NAL slices and one fragment buffer per frame (6 allocations measured).

Other changes, pinned by tests rather than benchmarks:

- Linux capture drops compositor frames above the active cadence at the
  `pipewiresrc` pad, before any copy or GPU upload (GStreamer test counts buffers
  reaching the next element). Previously a 144–240 Hz display paid a full-frame copy
  per refresh even while paused. `scripts/linux-pipewire-capture-check.sh` runs the real capture
  path against a live PipeWire node (WSL kali, no portal): a 1080p144 source shared at
  30 fps offered 144 frames/s, admitted 30/s, and held eight admitted frames without
  starving PipeWire's pool; process CPU was 3.5% of a core, versus 14.2–15.5% when
  the same run copies every frame before throttling (the previous order). The NVENC
  CPU-scale family was selected; VA/CUDA zero-copy families and a desktop portal
  session were not available.
- A screen layer holding a frame for its cadence sleeps until it is due:
  61 instead of 105 worker wakes per second in the unit test (Windows' default
  timer granularity limited the old 2 ms poll; where 2 ms sleeps are honored it
  was several hundred).
- Windows capture textures are allocated on demand up to the 3/7 cap
  (`capture_textures` reports the peak). A single-layer 4K60 HDR pipeline probe
  still peaked at three. With both layers
  (`screen_pipeline_probe --seconds=20 --assert --layers`, low layer encoding at
  1 Mbit/s) it peaked at four instead of the seven allocated up front before: about
  200 MB less VRAM for 4K FP16 (66 MB each), with no busy-surface drops.
- Native mute/deafen state and screen views are sent on change, with a 5 s / 1 s
  refresh, instead of every 500 ms; each one takes the SFU room write lock. The web
  client's views follow the same rule.
- The native call loop's `select!` is biased toward ready outgoing media.
- Encoder output (Windows MFT, Linux and software) reads parameter sets and the
  picture type from the units before the first slice instead of two full-frame
  scans, and the MFT path copies each frame once, IDR parameter-set prefix
  included, instead of copying and then re-copying IDRs.
- The receive relay keeps a running byte total instead of summing its queue per
  frame, writes each frame to the local preview peer under one deadline, and
  validates every SPS during the packetizer's own NAL split (one walk per frame).
  Each video track uses one pinned timer re-armed only when its deadline changes,
  instead of a new tokio `Sleep` per RTP packet, and the inbox returns that deadline
  from the same lock as the push.
- An active share of an unchanging source (no new captures for 200 ms) bounds its
  worker sleep at 100 ms instead of 20 ms: 10 polls per second (unit test), with
  captures, encoder output, budget release, demand and rejected-frame feedback
  still waking it at once. The native call loop keeps its audio worker lease with a
  keepalive every two seconds instead of every 500 ms.
- A share paused for lack of viewers sleeps until demand returns (which wakes it
  directly), with a 250 ms fallback for stop, instead of polling every 20 ms
  (unit test: at most 6 wakes in a paused second, encoding within 150 ms of demand).
- Linux encoder output is queued by buffer reference and copied once, after any
  keyframe parameter-set prefix, instead of copied and then re-copied for IDRs. The
  GStreamer layer test exercised this path at runtime in WSL with NVENC
  (`nvh264enc`, CPU-scale family): forced keyframes carried parameter sets and decoded.
- A sender held by backpressure wakes its encoder worker when budget frees
  instead of the worker polling every 2 ms.
- The audio worker wakes once per second outside a session instead of every
  100 ms, and periodic diagnostics JSON is built only while logging is on.
- Web: an unboosted microphone is sent as a clone of the capture, skipping the
  WebAudio limiter and destination; voice activation reads the meter every 20 ms
  instead of every 100 ms; one `getStats` walk serves audio and video, runs in its
  own task, and runs every second while WebAudio meters cover speaking (200 ms
  otherwise); remote video tiles update their status from `<video>` media events
  instead of a timer (only the own preview polls, to follow a camera switch).
- Desktop players renew their three-second viewer lease and sample decoder
  pressure once a second once playing (250 ms only while connecting) and set their
  status signal only when it changes.
- The own screen preview receives each fully paced frame whole (a reference to the
  encoded buffer) instead of a cloned packet with a new CSRC `Vec` per RTP packet that
  the inbox then reordered and reassembled (unit test: the queued frame is the
  sender's buffer). The native viewer monitor wakes once a second instead of every
  250 ms.
- The native camera publish relay has no 250 ms tick: keyframe requests wake it
  through an atomic waker and one pinned timer covers the ten-second fallback (unit
  test: a request wakes the parked relay within 50 ms; nothing pending, no wake).
- Linux shared audio: the PipeWire callback unparks the encoding thread after each
  buffer instead of the thread polling every 5 ms. `scripts/linux-pipewire-audio-check.sh`
  (private PipeWire + WirePlumber in WSL, null sink, `pw-play` tone, 2 s capture):
  95 consumer wakes with the same 192,512 samples captured, versus 439 with the old
  poll (the test now fails above 250).
- Windows shared audio reads each WASAPI packet into one contiguous buffer and
  downmixes whole 20 ms chunks, instead of pushing and popping a `VecDeque` one byte
  at a time, and takes Opus payloads from the pooled arena instead of one allocation
  per packet.
- Native voice: a received packet carries a copyable connection id instead of a
  `Connection` clone (four reference counts in and out per packet). A guarded send
  checks once and polls the write once; only a pending write arms the invalidation
  waiter (a lock) and the deadline timer. The send arm shares one clock read between
  admission and RED history. RTP writes for screen video, shared audio and the camera
  relay that complete on their first poll build no timeout timer.
- Web: a due voice-activation gate runs before an incoming signaling message is
  parsed.

Browser run (web client): the `/app` bundle from this branch, served by a local
backend with its own database (`scripts/web-call-check-backend.sh`, WSL), in
headless Chromium 150 with fake capture devices, two accounts in one voice channel,
driven over the DevTools protocol by `scripts/web-call-check.py voice|leak|camera`
with page hooks on `getUserMedia`, `replaceTrack`, `track.enabled`, `getStats`,
`AudioContext`, track `stop` and signaling sends:

- The unboosted microphone was sent as the capture clone (`replaceTrack` received
  the capture-labelled track, not the `MediaStreamAudioDestinationNode` track).
- `getStats` ran every 1,003–1,010 ms (meters running), from its own task.
- Voice activation, measured from an independent 5 ms analyser probe on the same
  capture to the sent track's `enabled = true`, with 120 ms tone bursts every 1.5 s
  (`WEBCHECK_HOLD=60 ... voice`, both participants in the call): hcalice median
  7.9 ms, p95 16.2 ms, max 17.6 ms (n = 42); hcbob median 9.3 ms, p95 15.7 ms, max
  17.5 ms (n = 42, `WEBCHECK_REVERSE=1`). Headless Chromium starves the second page's
  probe timers (it caught 5 of about 40 onsets while that page's gate still opened on
  every burst), so each participant is measured as the first page. The probe itself
  adds about ±5 ms.
- Three join/leave cycles: three call `AudioContext`s created and three closed,
  and every capture, clone and processed track stopped (three per leave).

A camera run (one participant publishing the fake camera, the other watching its
tile) decoded 1280×720 at 20 fps. The watcher renewed `ScreenViews` once per second
and `state` every 5.0 s. Its `getStats` walks first came in pairs 200/800 ms apart
(audio and video feedback had separate timers); a due walk now also serves feedback
due within half an interval, after which every walk was 1,004–1,008 ms apart. Moving
the tile out of view sent an empty view list 420 ms later and no renewals while
empty; bringing it back re-subscribed 142 ms later with a keyframe request right
after the renewed view. A screen-share run (`screen`: Chromium's fake display capture
through the share picker) decoded 1920×1080 at 20 fps on the watcher with views renewed
once per second and the tile status cleared by media events. Three microphone device
tests outside a call (`devicetest`) created and closed three `AudioContext`s and
stopped every capture, clone and processed track (three per test).

Windows relay to a browser decoder: `screen_pipeline_probe --browser --seconds=20
--assert` with headless Chrome negotiating the probe page over the DevTools protocol:
the app packetizer and production `Preview` loopback peer delivered all 1,196 frames,
which Chrome decoded at 3840×2160 with 0 dropped and 0 lost (PASS, 59.9 fps).

Desktop app run (Windows, debug build): the Tauri app under a separate identifier
against a loopback Windows build of the backend (`thiscord-backend` on 127.0.0.1,
the WSL test database through a local TCP forwarder; a WSL-hosted SFU is unreachable
over UDP because WSL's Hyper-V firewall blocks inbound traffic by default), driven
through WebView2's DevTools port, with a headless Chrome participant publishing its
fake camera:

- Receive relay and WebView player: the camera played at 1280×720, 20 fps (200
  frames presented and decoded in 10 s, 0 dropped). Moving the tile out of view
  ended its lease (0 frames in the next 3 s); back in view, the first frame arrived
  after 1,983 ms (a new local peer and keyframe), then 19.4 fps.
- Screen share of the 3440×1440 desktop at 1080p30 with system audio, both layers:
  the NVIDIA MFT encoded and sent 28.2 frames/s (0 dropped, 0 send timeouts, three
  capture textures); the own preview presented 21.9 fps at 1920×802 (231 decoded,
  0 dropped) from the whole-frame path; the remote browser decoded 1920×802 at
  26.4 fps (3 dropped of 280). WASAPI shared audio: 100 capture packets/s and 50.1
  Opus frames/s sent, 0 failures, queue resets or silent packets.
- Native voice on the real microphone and speakers: 966 capture and 966 render
  callbacks in 9.7 s with no deadline misses (p99 50 µs), Opus encode p99 500 µs,
  811 packets sent, none expired, invalidated or failed; 0.3 ms SFU-hop RTT. The
  other participant was muted (no test audio through real speakers), so native
  playback of received voice was not exercised here; it is covered by the encrypted
  loopback and voice-bot runs.

4K60 HDR pipeline probe A/B on the same machine, interleaved (base `0ae37e5` built in
a private build directory, `screen_pipeline_probe --seconds=20 --assert`): base 59.00,
59.19, 58.53, 59.72 fps (two runs below the probe's 59.0 acceptance), this branch
59.60, 59.80, 59.36, 59.37 (all pass); six further layered runs passed at
59.2–59.9. One run of an intermediate build ended in a segmentation fault in about
thirty runs of this branch; it did not recur and its cause is not established.

Not measured: received voice on real speakers, browser main-thread profiles,
total process VRAM, the Linux desktop app (WebKitGTK player and relay; Linux
capture, encode, share audio and voice were run separately), Linux desktop-portal
capture and the VA/CUDA families, and macOS.

## Reading the monitors

Open `/admin` as the instance owner or an instance admin. **Performance monitors**
shows cumulative calls, mean/max duration and counts above each displayed budget.
After the next successful refresh it also shows calls/s, elapsed ms/s and new
over-budget observations. Elapsed ms/s includes waiting and concurrency; it is
not CPU usage and nested rows must not be added together. There are no percentiles
for the new counters. The existing audio histograms provide them for DSP.
Browser clock resolution can round very short UI operations to zero; use counts
and accumulated time over several refreshes when investigating repeated work.

Budgets are investigation thresholds, not measured service-level objectives:
checkout/worker queue/ingress age 10 ms, query/execute/roster/friends/chat/unread
50 ms, focused permission inputs 20 ms, admin collection 200 ms, sampled room
wait 1 ms and fanout 2 ms. UI lookup 1 ms, merge/layout 16 ms, timer lateness 50 ms.
Look for sustained new overruns and a matching user-visible symptom, not a single
lifetime maximum. One operation can increment several nested measurements.

For a connected desktop, select its participant name. **Desktop UI performance**
reads an automatic fixed-size snapshot from its main WebView, alongside existing
audio diagnostics. It is copied to native memory approximately every ten seconds;
the panel polls through the existing authenticated audio-status relay. No new
network upload endpoint or background client-to-server telemetry stream exists.
The report is marked stale after thirty seconds. Hidden/suspended WebViews can
stop reporting; stale is not proof of a hang. Timer intervals spanning a visibility
change are excluded. Overlays do not start additional monitors. The new IPC command
is restricted to the main window and accepts four fixed numeric timing slots.

**Download diagnostics** also includes the UI snapshot even if the audio worker
cannot respond. This existing download additionally contains audio settings and
available technical logs; UI counters themselves contain only numbers. Old clients
and servers remain readable through optional/defaulted response fields. A missing
snapshot means unavailable, not a zero-duration measurement. Browser preview has
no remote telemetry transport; only the admin browser's own UI counters are shown
locally. Offline clients cannot be inspected through the live-voice relay.

Backend counters are process-wide, in memory, always on, fixed in number and use
relaxed atomics. Snapshots are approximate under concurrent updates. No per-user,
per-guild, per-query or arbitrary label maps are retained. SQL text/parameters,
tokens, message contents, packet contents and media never enter these counters.
Queries/executions include failures; checkout failures have a separate counter.
The SQL helpers cover application queries, not Diesel migrations/readiness or
implicit transaction-control statements. The worker-queue timer covers chat,
friends and notification tasks, not all blocking work. Admin reads add a little
work to the observed totals; the current diagnostics request completes after its
own snapshot, so its duration appears on the next refresh.

Backend restart and WebView reload reset their respective totals. New data is
not persisted. Backend diagnostics retain current instance-role/session checks
and revocation barriers. Desktop counters are client-reported, useful for
diagnosis but not trusted for authorization or billing. Rebuild both backend and
admin assets for the dashboard, and update desktops for UI measurements. Production
load confirmation and Linux/macOS desktop runtime validation remain follow-ups.

## Validation

Validated shared serialization/backward compatibility tests, concurrent timing
accounting and early-return accounting. WSL checks cover the backend library,
HTTP and admin-web tests and PostgreSQL-backed chat/friends/notification/unread
tests, including diagnostics access and revocation during collection. Clippy
passes for backend tests, the WASM UI and the native Windows desktop. Formatting
and diff whitespace checks pass. These functional checks do not establish a
production latency baseline or Linux/macOS desktop runtime compatibility.

The interactive Trunk/browser smoke check could not complete: concurrent builds
in the shared intermediate directory repeatedly supplied another checkout's
`thiscord-shared` artifact between Trunk's build/artifact-discovery calls (missing
the new performance fields and, once, requiring an unrelated `attachment_ids`
field). The WASM Rust build/check succeeded when the current shared crate was
rebuilt. No caches were cleared and other checkout processes were left running.
Repeat the bundled-browser check when the shared build slot is idle.
