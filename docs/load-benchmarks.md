# Load benchmarks (VPS, single host)

How many concurrent users the backend serves for voice, camera video and text
chat on the production VPS, how that was measured, and the connection limits
derived from it. These are synthetic single-host measurements: read the
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
- Harness: `scripts/load-bench.py` pins the server to CPU 1 and the generator to
  CPU 0 (loopback) and records server, generator and PostgreSQL CPU (the
  PostgreSQL service cgroup, which also holds the idle production database),
  database transactions/s and server RSS over the measured window.
  `--server-cpus 0-1 --generator-cpus 0-1` is the production layout (everything
  shares both vCPUs); `--kind mixed` runs audio/camera and chat at once.
- Workloads: rooms of eight. Audio: 80-byte payloads at 50 packets/s (32 kbit/s).
  Camera: 1 Mbit/s, 30 frames/s, every user publishing and watching the seven
  others in its room. Chat: **server-wide** messages/s in **one** text channel
  that every user subscribes to (the worst case for fan-out), sent round-robin
  by all users.
- Verdicts: voice/video **pass** when every stream loses or delays (>100 ms end
  to end) at most 0.1% of what the generator sent and the generator kept its
  schedule; **server-side pass** ignores generator scheduling slips. Chat passes
  when every user connects, every send succeeds and at most 0.1% of deliveries
  are missing or later than 1 s (from the HTTP send to socket receipt).
- Baseline: `6add51e` (before this work) built with the same load tools and run
  with the same script, interleaved with the current build.
- Raw per-run JSON stays on the VPS under `~/bench-results`; the summaries below
  are copied from them.

Reproduce on the VPS (a checkout whose `backend/.env` has a `TEST_DATABASE_URL`
ending in `_test`):

```sh
export CARGO_TARGET_DIR=$PWD/target CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8
cargo build -p thiscord-backend --features load-test --release --locked --examples
export $(grep ^TEST_DATABASE_URL= backend/.env)
python3 scripts/load-bench.py --target "$CARGO_TARGET_DIR" --kind voice --users 64 --output v64.json -- --seconds 30
python3 scripts/load-bench.py --target "$CARGO_TARGET_DIR" --kind chat --users 128 --output c128.json -- --seconds 30 --rate 8
```

## Results (2026-11-05)

CPU is percent of one core; on the pinned layout the server has exactly one.

### Voice (audio)

| Run | Build | Server CPU | PostgreSQL CPU | Result |
| --- | --- | --- | --- | --- |
| 64 users | baseline | 75.5% | 27.2% | no loss; generator skipped 0.9% of sends |
| 64 users | current | 73.3% | 3.2% | pass, p99 23 ms |
| 128 users | baseline | 84.3% | 56.3% | 3.7% missing, p50 127 ms |
| 128 users | current | 98.7% (core saturated) | 3.7% | 1.8% missing, p50 89 ms |
| 256 users | baseline | 81.0% | 67.8% | 19 clients disconnected by DB pressure |
| 256 users | current | 96.0% | 6.3% | 0 clients disconnected; overloaded |
| 128 users, 2 cores | current | 109% | 2.8% | 0.8% missing; generator 86%, host saturated |

The per-second database reauthorization per voice user is gone (PostgreSQL
27% → 3% at 64 users), which removed the client disconnections at 256 users. An
earlier interleaved A/B at 64 users (two runs each) measured server CPU
69.0/70.5% before and 67.3/64.5% after. Forwarding itself costs about 1.1–1.2% of
a core per audio user.

### Video (camera, 1 Mbit/s)

| Run | Build | Server CPU | Result |
| --- | --- | --- | --- |
| 16 users | baseline | 49.0% | pass, video p99 19 ms |
| 16 users | current | 47.6% | pass, video p99 19 ms |
| 32 users | baseline | 95.9% | 1.8% video missing, p99 253 ms |
| 32 users | current | 87.7% | video 0.001% missing; some audio late |
| 32 users, 2 cores | current | 87.2% | pass, video p99 24 ms |

About 3% of a core per camera user (publishing to and receiving from seven).

### Where SFU CPU goes

`scripts/sfu-profile.py` attributes a frame-pointer `perf record -g` profile to
stages; the output for 64 audio users and 16 camera users is in
[benchmarks/sfu-profile-2026-11-05.txt](benchmarks/sfu-profile-2026-11-05.txt).
The webrtc-rs peer driver is 78–82% of server CPU: kernel UDP/IP sends and
receives (one system call per packet, about 35%), its RTP/RTCP and interceptor
logic (about 25%), allocation and copies inside it (about 11–12%), and SRTP
crypto (2–3%). This project's code (ingress reader, relay fan-out with its
authorization, egress writer with its revalidation, signaling) is 14–16%.
Removing all of our per-packet work would therefore save at most about a
sixth; larger gains need batching or a different transport inside webrtc-rs,
which this work does not change.

### Chat

| Run | Build | Server CPU | PostgreSQL CPU | Delivery p50 / p99 | Result |
| --- | --- | --- | --- | --- | --- |
| 128 users, idle | baseline | 8.9% | 21.3% | – | pass |
| 128 users, idle | current | 6.6% | 8.1% | – | pass |
| 128 users, 8 msg/s | baseline | 32.2% | 160.1% | 1693 / 2400 ms | fail: 79 sends got 429 |
| 128 users, 8 msg/s | current | 29.0% | 25.6% | 24 / 135 ms | pass |
| 128 users, 8 msg/s, cached delivery path | current | 27.8% | 29.5% | 22 / 162 ms | pass |
| 500 users, idle | current | 23.4% | 31.5% | – | pass |
| 500 users, 2 msg/s | current | 69.1% | 93.5% | 287 / 742 ms | pass |
| 500 users, 4 msg/s | current | 76.0% | 92.6% | 379 / 944 ms | no loss; 0.7% over 1 s |

At 128 users and 8 msg/s, server plus PostgreSQL CPU per delivered message fell
from 1.88 to 0.53 core-seconds per thousand deliveries (3.5×), and PostgreSQL
alone from 1.56 to 0.25 (6×). Connecting 128 users took 15–16 s before and 2.3 s
now; 500 users connect in 24–28 s with zero failures (16 concurrent joins, no
login hashing). The 500-user runs at 2–4 msg/s use the whole host including the
generator (about 190% of 200%).

### Limits

| Run | Result |
| --- | --- |
| 160 users, baseline (fixed 128-socket limits) | 32 users refused |
| 300 users, `THISCORD_MAX_SOCKETS=256`, 4 msg/s | 44 users refused with 429 before database work; the 256 admitted pass (p99 290 ms) |

### Mixed load on the production layout

Both vCPUs shared by the server, PostgreSQL and both generators. The voice
generator runs for the whole window; the chat generator measures 30 s in it.

| Run | Server / PostgreSQL / generators CPU | Chat | Voice |
| --- | --- | --- | --- |
| 48 audio users alone | 48% / 2% / 36% | – | pass, p99 11 ms |
| 48 audio + 192 chat users at 2 msg/s | 77% / 36% / 48% | pass, p50 67 ms, p99 263 ms | no client lost; 0.26% missing, 1.8% over 100 ms, p99 121 ms; generator skipped 671 sends |
| 16 camera + 128 chat users at 2 msg/s | 71% / 27% / 42% | pass, p99 187 ms | video 0.0005% missing, 1.3% over 100 ms, p99 115 ms; audio p99 108 ms |

Chat holds up under concurrent media. Audio and camera stay delivered but their
tail latency crosses the strict 100 ms threshold when chat bursts compete for the
two vCPUs with PostgreSQL and the generators (which themselves slip, as the
skipped sends show); the same audio load alone stays at 11 ms. These mixes did
not pass the strict media verdict, so treat them as the practical upper end of
combined load on this host rather than a comfortable operating point.

### Memory

Server RSS: about 2.5 MiB per voice user (320 MiB at 128, 622 MiB at 256) and
about 1 MiB per connected chat user with three sockets (97 MiB at 128, 505 MiB
at 500, mostly per-socket member and presence lists of the whole guild). At the
default limits (512 sockets of each type, 128 voice connections) that is about
0.85 GiB, beside PostgreSQL (about 0.3 GiB here) on the 3.7 GiB host. Every new
cache is bounded: one chat grant per socket, at most 64 alert grants per
notification socket (ten seconds each), and one shared event page (100 events)
and presence list per channel while it has subscribers.

## Capacity and connection limits

On the production layout (both vCPUs shared with PostgreSQL and the OS):

- Audio: about 1.15% of a core per user, so roughly 130 audio-only users fit in
  the CPU left beside a lightly loaded database. `THISCORD_MAX_VOICE_CONNECTIONS`
  defaults to 128.
- Camera video at 1 Mbit/s: about 3% of a core per camera user; 32 passed on two
  cores. The voice limit counts connections, not cameras: rooms where most
  people share cameras reach CPU saturation at roughly 50 camera users, well
  below 128 connections. Lower the limit for camera-heavy communities.
- Chat: `THISCORD_MAX_SOCKETS` defaults to 512 per socket type. 500 full clients
  connect without failures, idle at about a quarter of the host, and receive
  2 msg/s in one shared channel within a second. Higher message rates at that
  size saturate the host.

Both variables are validated at startup (`THISCORD_MAX_SOCKETS` 16–4096,
`THISCORD_MAX_VOICE_CONNECTIONS` 2–1024; see `backend/.env.example`).
Connections beyond a limit get `rate_limited` before any database work, and
admitted users keep their capacity (the over-limit run above).

## Caveats

- One host: the generators run on the same two vCPUs as the server and
  PostgreSQL, so the largest runs saturate the host including the generator.
  Off-host clients would leave more CPU for the server; the per-user costs above
  are the server's own.
- Loopback networking: no packet loss, jitter, NAT, TURN or Cloudflare, and no
  bandwidth limit; the VPS uplink is not measured.
- Synthetic media: audio and camera payloads are not decoded; real encoders
  vary bitrate and keyframes. Screen sharing shares the video forwarding path
  but was not measured separately.
- Chat uses one channel with every user subscribed; real guilds spread users
  over channels, which costs less per message. Clients that mark messages read
  add HTTP requests not modeled here.
- Each number is one 30–40 s run (the 64-user audio A/B is two runs each);
  differences of a few percent are within run-to-run noise on this VPS.
