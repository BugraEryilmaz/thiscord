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
   `backend/src/friends/mod.rs` polls once a second per connected friends socket.
   `friends/store.rs::snapshot` reauthenticates, locks relationships, reloads all
   friend profiles and, when a conversation is selected, reloads its history.
   The comparison that suppresses identical outbound snapshots happens *after*
   this database work. `chat/session.rs` similarly calls
   `chat/store.rs::visible_voice_channels` once a second for roster subscribers,
   reloading permission inputs and evaluating visible channels before comparing
   the roster. These costs grow with connected clients even during idle chat.
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
