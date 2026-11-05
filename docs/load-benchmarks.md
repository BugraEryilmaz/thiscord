# Load benchmarks (VPS)

How many concurrent users the backend serves for voice, camera video and text
chat on the production VPS, how that was measured, and the connection limits
derived from it. These are synthetic measurements: read the
[caveats](#caveats) before quoting a number.

## Host and method

- Host: the production VPS ([vps.md](vps.md)): 2 vCPU KVM "Intel Core Processor
  (Haswell, no TSX)", 3.7 GiB RAM, no swap, Ubuntu 26.04, PostgreSQL 18.6
  (`shared_buffers` 128 MB). The production backend and database kept running,
  idle, during the runs.
- Server: `voice_load_server`: the real router, SFU and authorization against
  PostgreSQL in a dedicated `thiscord_bench_test` database, five-connection
  pool, a fresh `voice_load_*` schema per run. Release build with the deploy
  script's settings (LTO off, 8 codegen units).
- Generators: `voice_load` (audio; `--camera-kbps` adds synthetic H.264 camera
  packets) and `chat_load` (each user holds the chat, friends and notification
  sockets a signed-in client opens, subscribes with voice roster and member list,
  and posts messages over HTTP). See [voice-load.md](voice-load.md).
- On-host runs (`scripts/load-bench.py`): the server pinned to CPU 1 and the
  generator to CPU 0 over loopback, or both on both vCPUs (`--server-cpus 0-1
  --generator-cpus 0-1`, the production layout); `--kind mixed` runs voice and
  chat at once. It records server, generator and PostgreSQL CPU (the PostgreSQL
  service cgroup, which also holds the idle production database), database
  transactions/s and server RSS over the measured window.
- Off-host runs (`scripts/load-remote.py`): the server on the VPS using both
  vCPUs; voice generators in WSL on a 12-core desktop, media over the internet
  (77 Mbit/s up, 243 Mbit/s down, 27 ms RTT) and signaling through an SSH
  tunnel. Thousands of chat sockets do not fit through one tunnel, so mixed runs
  start the (light) chat generator on the VPS (`--chat-on-server`).
- Workloads: rooms of eight. Audio: 80-byte payloads at 50 packets/s (32 kbit/s).
  Camera: 1 Mbit/s, 30 frames/s, every user publishing and watching the seven
  others in its room. Chat: **server-wide** messages/s in **one** text channel
  that every user subscribes to (the worst case for fan-out), sent round-robin
  by all users.
- Verdicts: voice/video **pass** when every stream loses or delays (>100 ms end
  to end) at most 0.1% of what the generator sent and the generator kept its
  schedule; **server-side pass** ignores generator scheduling slips. Over the
  internet, 16 users alone lose 0.1% (the path's baseline), so off-host runs
  are judged by late packets and by loss above that baseline. Chat passes when
  every user connects, every send succeeds and at most 0.1% of deliveries are
  missing or later than 1 s (from the HTTP send to socket receipt).
- Baseline: `6add51e` (before this work) built with the same load tools and run
  with the same script, interleaved with the current build.
- Every run's summary (84 runs: on-host matrices, repeats, extra runs and the
  off-host series, each tagged with its commit) is in
  [benchmarks/load-runs-2026-11-05.jsonl](benchmarks/load-runs-2026-11-05.jsonl).
  The tables give ranges over the runs of each configuration on the commit
  named in that row; runs of superseded commits stay in the log.

Reproduce on the VPS (a checkout whose `backend/.env` has a `TEST_DATABASE_URL`
ending in `_test`):

```sh
export CARGO_TARGET_DIR=$PWD/target CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8
cargo build -p thiscord-backend --features load-test --release --locked --examples
export $(grep ^TEST_DATABASE_URL= backend/.env)
python3 scripts/load-bench.py --target "$CARGO_TARGET_DIR" --kind voice --users 64 --output v64.json -- --seconds 30
python3 scripts/load-bench.py --target "$CARGO_TARGET_DIR" --kind chat --users 128 --output c128.json -- --seconds 30 --rate 8
```

Off-host, from a machine that can `ssh vps` and has release builds of the
generators (`--remote-dir` is the VPS checkout used above):

```sh
python3 scripts/load-remote.py --ssh vps --remote-dir thiscord-bench --target "$CARGO_TARGET_DIR" \
  --media-ip 57.129.178.14 --bind LOCAL_IP --output v96 --voice "--users 96 --seconds 30"
```

## Results (2026-11-05)

CPU is percent of one core; on the pinned layout the server has exactly one.
"Current" rows ran on commit `c81f29c` (the code under review) unless a row
names an earlier commit; earlier-commit rows are kept where no later change
touched the measured path, and say so. The baseline is `6add51e`.

### Voice (audio)

| Run | Build | Server CPU | PostgreSQL CPU | Result |
| --- | --- | --- | --- | --- |
| 64 users, pinned (3 runs) | baseline | 64.0–75.5% | 25.0–27.2% | no server loss; the generator slipped in every run |
| 64 users, pinned (2 runs) | current | 66.4–66.8% | 1.7–2.0% | pass, p99 17–22 ms |
| 128 users, pinned | baseline | 84.3% | 56.3% | 3.7% missing |
| 128 users, pinned | `bfb5e84`¹ | 98.7% (core saturated) | 3.7% | 1.8% missing |
| 256 users, pinned | baseline | 81.0% | 67.8% | 19 clients disconnected by DB pressure |
| 256 users, pinned | `bfb5e84`¹ | 96.0% | 6.3% | 0 disconnected; overloaded |
| 96 users, off-host (2 runs) | current | 103.8–104.0% | 3.3–4.0% | 0.013–0.022% late, loss below the path baseline, p99 46–47 ms |
| 128 users, off-host | `6999bbf`¹ | 166% | 4.9% | 0.2% late, p99 68 ms: the edge of the host |

¹ Forwarding per packet is unchanged since: later commits changed chat,
notifications, retries and the runtime that runs voice tasks (which only
matters when chat competes for the workers).

The per-second database reauthorization per voice user is gone (PostgreSQL
about 26% → 2% at 64 users), which removed the disconnections at 256 users.
Forwarding costs about 1.0–1.2% of a core per audio user on loopback and about
1.1–1.3% with real networking (off-host).

### Video (camera, 1 Mbit/s)

| Run | Build | Server CPU | Result |
| --- | --- | --- | --- |
| 16 users, pinned (3 runs) | baseline | 48.6–53.2% | pass |
| 16 users, pinned (2 runs) | current | 43.8–48.3% | pass, video p99 15–16 ms |
| 32 users, pinned | baseline | 95.9% | 1.8% video missing, p99 253 ms |
| 32 users, pinned | `bfb5e84`¹ | 87.7% | video 0.001% missing; 0.7% of audio late |
| 32 users, both cores | `bfb5e84`¹ | 87.2% | pass, video p99 24 ms |

About 3% of a core per camera user (publishing to and receiving from seven).

### Where SFU CPU goes

`scripts/sfu-profile.py` attributes a frame-pointer `perf record -g` profile to
stages and breaks this project's own stages down to their hottest frames; the
output for 64 audio users and 16 camera users (`bfb5e84`¹) is in
[benchmarks/sfu-profile-2026-11-05.txt](benchmarks/sfu-profile-2026-11-05.txt).
The webrtc-rs peer driver is 78–82% of server CPU: kernel UDP/IP sends and
receives (one system call per packet, about 35%), its RTP/RTCP and interceptor
logic (about 25%), allocation and copies inside it (about 11–12%) and SRTP
crypto (2–3%). This project's stages are 14–16%: the egress writer (about 8%:
revalidation under the room lock, channel hand-offs and webrtc-rs's own write
path), the relay (about 6%, of which about 1% is WebSocket reads of the
signaling loop that the stage pattern also matches) and the ingress reader
(1%). No single own frame exceeds 1.6% of samples; larger gains need batching
or a different transport inside webrtc-rs, which this work does not change.

### Chat

| Run | Build | Server CPU | PostgreSQL CPU | Delivery p50 / p99 | Result |
| --- | --- | --- | --- | --- | --- |
| 128 users, idle (3 runs) | baseline | 7.8–8.9% | 19.8–21.3% | – | pass |
| 128 users, idle (2 runs) | current | 6.0–6.2% | 7.0–10.2% | – | pass |
| 128 users, 8 msg/s (3 runs) | baseline | 31.9–32.3% | 159.6–160.1% | 1626–1693 / 2329–2415 ms | fail: 72–79 sends got 429 |
| 128 users, 8 msg/s (2 runs) | current | 29.9–30.1% | 32.3% | 29–30 / 156–163 ms | pass |
| 512 users, idle (2 runs) | current | 25.4–26.3% | 33.2–40.5% | – | pass |
| 512 users, 2 msg/s (2 runs) | current | 65.5–66.3% | 97.4–97.8% | 280–336 / 697–861 ms | one pass; one with 0.66% over 1 s |
| 500 users, 4 msg/s | `3ed168e` | 76.0% | 92.6% | 379 / 944 ms | no loss; 0.7% over 1 s |

At 128 users and 8 msg/s, server plus PostgreSQL CPU per delivered message fell
from about 1.87 to 0.61 core-seconds per thousand deliveries (3.1×), and
PostgreSQL alone from 1.56 to 0.32 (4.9×). Idle, a connected full client (three
sockets) costs about 0.05% (server) plus 0.06–0.08% (PostgreSQL) of a core at
128–512 users, from 0.06% plus 0.16% before. Connecting 128 users took 13–16 s
before and 2.2–2.5 s now; 512 users connect in 28–30 s with zero failures
(16 concurrent joins, no login hashing). Connect storms are dominated by every
subscriber reading and serializing the whole guild member list (a quadratic
cost); sharing those lists between viewers, whose block visibility differs, is
a follow-up. At 512 users and 2 msg/s the host is fully used (server,
PostgreSQL and generator together about 185% of 200%).

### Limits

| Run | Result |
| --- | --- |
| 160 users, baseline (fixed 128-socket limits) | 32 users refused |
| 300 users, `THISCORD_MAX_SOCKETS=256`, 4 msg/s (`bfb5e84`) | 44 users refused with 429 before database work; the 256 admitted pass (p99 290 ms) |

`sockets_beyond_each_limit_are_refused_before_database_work` checks the same
refusal for each socket type on every commit.

### Mixed load on the production layout

Voice generators off-host, the chat generator on the VPS, the server on both
vCPUs, chat at 2 msg/s. Voice keeps running through the chat window (including
chat's connect storm); its verdict covers the whole 90 s. On this commit's
session, 16 audio users alone had 0.18% missing and 0.57% late on the internet
path (p99 92 ms); earlier the same path was clean, so judge late audio against
that.

| Run | Build | Server / PostgreSQL CPU | Chat | Voice |
| --- | --- | --- | --- | --- |
| 32 audio + 128 chat (3 runs) | current, `6d55762` ×2 | 55–58% / 24–26% | pass, p99 74–88 ms | 0 late, p99 38–41 ms, loss at the path baseline |
| 48 audio + 128 chat (3 runs) | current ×2, `6d55762` | 71–77% / 25–30% | pass, p99 98–127 ms | 0.04–0.8% late (p99 68–92 ms); the higher two on the session with 0.57% baseline lateness |
| 16 camera + 128 chat (2 runs) | current, `6d55762` | 74–77% / 26–27% | pass, p99 80–93 ms | video 0.2% late, audio ≤0.1% late, p99 53–64 ms |
| 48 audio + 192 chat | `6d55762` | 88% / 37% | pass, p99 168 ms | 1.1% late, p99 104 ms |
| 64 audio + 256 chat | `6d55762` | 111% / 49% | pass, p99 454 ms | 1.8% missing, 5.8% late |
| **96 audio + 512 chat: both defaults at once** | current | 69% / 102% | 15% over 1 s, p99 3.9 s | 25% missing, 45% late |
| 32 audio + 128 chat, before the media runtime | `6999bbf` | 54% / 28% | pass | 0.38% late, p99 85 ms |

The largest mix that keeps voice clean is 32 audio users with 128 chat users
at 2 msg/s (three runs); 48 + 128 is marginal. **The default limits are each
safe alone, not together:** 96 voice users plus 512 chat clients exceed the two
vCPUs (PostgreSQL alone used a full core) and both degrade badly. The last row
shows why voice runs on its own Tokio runtime: with chat fan-out sharing the
workers, audio queued behind hundreds of woken socket tasks while the CPU was
two-thirds idle. An earlier 96 + 500 attempt (`6d55762`) is in the run log with
a malformed CPU window (a harness bug fixed in `c81f29c`); it degraded the same
way (20% voice missing, 29 chat users failed to connect).

### Memory

Server RSS: about 2.5 MiB per voice user (248 MiB at 96, 320 MiB at 128,
622 MiB at 256) and about 0.9 MiB per connected chat client with three sockets
(91–92 MiB at 128, 458–492 MiB at 512, mostly per-socket member and presence
lists of the whole guild). With both defaults in use (96 voice + 512 chat) the
server reached 802 MiB, beside PostgreSQL (about 0.3 GiB) on the 3.7 GiB host;
these figures include the media runtime's threads and both blocking pools.
Every new cache is bounded: one chat grant per socket, at most 64 alert grants
per notification socket (ten seconds each), and one shared event page (100
events) and presence list per channel while it has subscribers.

## Capacity and connection limits

On the production layout (both vCPUs shared with PostgreSQL and the OS):

- Audio: 96 users run clean with real networking at about 104–119% of the
  host's 200% CPU; 128 reach the edge. `THISCORD_MAX_VOICE_CONNECTIONS` defaults to 96
  (before this work: a fixed 64).
- Camera video at 1 Mbit/s costs about 3% of a core per camera user; 32 passed
  on two cores. The voice limit counts connections, not cameras: rooms where
  most people share cameras saturate the host at roughly 40–50 camera users.
  Lower the limit for camera-heavy communities.
- Chat: `THISCORD_MAX_SOCKETS` defaults to 512 per socket type (before: a fixed
  128). 512 full clients connect without failures (in 28–30 s), idle at
  about a third of the host (server plus PostgreSQL), and at 2 msg/s in one
  shared channel are at the edge: one of two runs delivered everything within
  a second, the other had 0.66% of deliveries later than that.
- Combined: about 32 people in voice while 128 are active in chat at 2 msg/s.
  The defaults are sized for one workload at a time: 96 voice users and 512
  chat clients together overload the host (see the mixed-load table). A
  community that uses both heavily should lower one of the limits.

Both variables are validated at startup (`THISCORD_MAX_SOCKETS` 16–4096,
`THISCORD_MAX_VOICE_CONNECTIONS` 2–1024; see `backend/.env.example`).
Connections beyond a limit get `rate_limited` before any database work, and
admitted users keep their capacity (the over-limit run above).

## Caveats

- On-host runs share the two vCPUs between the server, PostgreSQL and the
  generators; the largest of them saturate the host including the generator.
  Off-host runs remove the voice generator but add internet loss and jitter;
  chat generators stayed on the host.
- Synthetic media: audio and camera payloads are not decoded; real encoders
  vary bitrate and keyframes. Screen sharing shares the video forwarding path
  but was not measured separately. No TURN relaying was measured.
- Chat uses one channel with every user subscribed; real guilds spread users
  over channels, which costs less per message. Clients that mark messages read
  add HTTP requests not modeled here.
- Most configurations ran two to four times; run-to-run differences of up to
  about 10% CPU are normal on this VPS.
- The PostgreSQL unread-plan test fails on this host on the base commit too
  ([log](benchmarks/unread-plan-test-pg18-2026-11-05.txt)).
