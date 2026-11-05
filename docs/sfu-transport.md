# SFU transports

The backend SFU has two interchangeable media transports, selected per process at
startup. Signaling, authorization, room state and every forwarding rule are shared;
only how packets reach the peers' WebRTC stacks differs.

| `THISCORD_VOICE_TRANSPORT` | What runs |
| --- | --- |
| `webrtc` (default) | One webrtc-rs `PeerConnection` per participant: its own driver task, UDP socket and timer. A relay task per publisher and four egress writer tasks per receiver move packets between them through channels (`voice/mod.rs`, `voice/egress.rs`). |
| `shard` | `THISCORD_VOICE_SHARDS` threads (default: one per core, at most 16), each one event loop over one UDP socket that owns the `rtc` sans-I/O cores of every connection in the rooms hashed to it. A received packet is decrypted, authorized, rewritten and encrypted for each receiver in that loop (`voice/shard/`). |

Both variables are validated at startup (`webrtc` or `shard`; shards 1-64, only with
`shard`). **Reverting is removing the variable (or setting `webrtc`) and restarting.**
Nothing is persisted per transport; clients cannot tell them apart except by the
server's candidate port.

## Why

Profiles of the webrtc transport (docs/load-benchmarks.md, "Where SFU CPU goes")
put 78-82% of SFU CPU inside webrtc-rs. Most of it was per-packet overhead around
the protocol work rather than the work itself: every relayed RTP packet woke the
receiver's driver task, which took the core's async mutex about six times, recomputed
every handler's timeout, registered a new timer and rebuilt a `select!`, then sent one
datagram per system call. Fan-out added a channel hop and a task wakeup per receiver
per packet. The shard transport removes those hand-offs: one wakeup handles a batch of
datagrams (`recvmmsg`), forwards every packet to every receiver in place, and sends the
whole batch with one `sendmmsg`.

## What is the same

- Signaling, offers (server offerer, `MediaKind::TRACK_COUNT` m-lines with fixed relay
  SSRCs), codecs and RTP interceptors (`media_setup` in `voice/mod.rs`), RED, layers,
  views, byte budgets, keyframe requests, rosters, resumption and authorization
  rechecks.
- Track classification and ingress limits: `shard/peer.rs` `open_track`/`media` follow
  `Handler::on_track` and its reader task line by line.
- Forwarding: `shard/forward.rs` runs the relay loop's checks and header rewrite, admits
  each delivery with the same `egress::Sender::admit`, then runs `egress::Writer::run`'s
  per-packet steps (RED floors, sequence/timestamp continuity, screen sequence windows,
  final `authorized_write` revalidation with both endpoints' permits held through the
  write).

**Changing forwarding or permission rules means changing both `voice/mod.rs` +
`voice/egress.rs` and `voice/shard/forward.rs`.** CI runs the whole voice integration
suite under both transports.

## What differs

- No queue between admission and write. The queue's capacity and age limits
  (`Sender::try_send`) cannot be exceeded and are not applied; the writer's deadline
  still applies to packets a screen route held back. Revalidation and admission run
  under the same room read-lock acquisition, but both still run.
- No transport queue to fill, so the webrtc mode's send-timeout path (`metrics[4]`,
  `SCREEN_BACKOFF`) never triggers. When the kernel socket buffer is full, a shard waits
  at most 5 ms for capacity, then drops what is left of that batch (UDP semantics; video
  receivers recover through their normal loss/keyframe path).
- One UDP port per shard instead of per participant. Datagrams are routed by source
  address, learned only from a STUN Binding request carrying a live connection's local
  ICE ufrag or from addresses the connection's own ICE agent sent to (at most 16 per
  connection; `shard/demux.rs`). Unknown sources are dropped before any DTLS/SRTP work.
- Host candidates only, on the bind address (`THISCORD_VOICE_BIND`, or the default-route
  IPv4 address). The server does not gather STUN server-reflexive or TURN relay
  candidates itself; clients reach it directly or through their own TURN relay (the
  `relay -> host` route in docs/turn.md). The bind address must be concrete (no
  wildcard), and a fixed port gives shard *i* port + *i*. mDNS is disabled.
- A room lives on one shard thread; rooms are spread across shards by channel id. At
  `ROOM_CAPACITY = 8`, one room is far from a core's capacity.
- A shard waits for the room lock when a signaling task holds it for writing. Writers
  hold it only for synchronous updates; a long hold would delay every room on that
  shard, so keep it that way.

## Patched webrtc-rs crates

`rtc`, `rtc-interceptor` and `rtc-srtp` are vendored with allocation/copy fixes on the
per-packet path (`vendor/rtc/THISCORD_PATCH.txt`). They apply to both transports and to
the native clients. Measured, they contribute little (below); the transport change is
where the gain is.

## Measurements (2026-11-05)

`scripts/load-bench.py` on a desktop (i7-13700K, WSL2), server pinned to one core
(`--server-cpus 2`), generator on eight others, rooms of eight, 45 s windows, release
build with the deploy settings. CPU is percent of that one core. The VPS figures in
docs/load-benchmarks.md are about 2.7x higher per user on its older virtual cores;
ratios are what carry over.

| Workload | webrtc | shard | Ratio |
| --- | --- | --- | --- |
| 64 audio users | 24.2% | 14.6% | 1.66x |
| 16 camera users (1 Mbit/s) | 16.9% | 10.4% | 1.62x |
| 192 audio users | 77.8%, p99 16 ms | 38.6%, p99 7 ms | 2.0x |
| 320 audio users | 100%, failed (20,789 late) | 66.6%, pass, p99 13 ms | - |

All other runs passed with no loss. The ratio grows with load because batches grow.
Patch-only effect at 64 audio users: webrtc 24.8% -> 24.7%, shard 14.9% -> 14.3%.

Profile of the shard transport at 64 audio users (perf, frame pointers): `rtc` core
handlers 23%, interceptors 14%, SRTP 4%, kernel send/receive including loopback delivery
to the generator 25%, the shard loop and in-loop forwarding about 20% (of which
signaling and roster sends are a few percent). The remaining webrtc-rs cost is mostly
moving each packet's message structs through eight handler stages and the interceptor
chain; removing that would mean boxing packets through the vendored interceptors.

### Production VPS (2026-11-05, round 5)

The VPS and methods of docs/load-benchmarks.md, same build for both transports,
interleaved. Every run is in
[benchmarks/load-runs-2026-11-05.jsonl](benchmarks/load-runs-2026-11-05.jsonl)
(sets starting "round 5"). On this host the backend test suite passed under both
transports except the chat unread-plan test that also fails on the base commit; the
voice integration suite and the voice-load smoke checks passed under both.

Off-host (generators in WSL on a desktop, media over the internet, server on both
vCPUs, so two shards); CPU is percent of one vCPU (host total 200%):

| Audio users | webrtc | shard |
| --- | --- | --- |
| 96 | 106.7 / 102.4%, 0 late, p99 42 ms | 49.6 / 50.3%, 0 late, p99 38-39 ms |
| 128 | 136.1 / 143.1%, 1,985 / 815 late, p99 52-59 ms | 67.7 / 69.3%, 0 late, p99 40-45 ms |
| 160 | - | 86.9%, 0 late, p99 44 ms |
| 192 | - | 107.9%, 2,691 late, 0.27% missing, p99 59 ms |

Missing packets were 0.04-0.07% for both transports up to 160 users, the internet
path's own baseline (which is why the generic 0.1% verdict reads "failed" for every
off-host run; the series is judged by late packets and loss above that baseline).
At 192 users the host still had CPU to spare in total, so the onset is not a whole-
host limit; uneven room placement on two shards (one thread per shard) or the path
may explain it, and was not isolated.

On-host, pinned (server on one vCPU, generator on the other): 64 audio users 62.8 /
62.7% -> 37.3 / 35.9%, 16 camera users 42.3 / 44.0% -> 23.9 / 24.9%, 128 audio users
98.9 / 98.2% (saturated: 0.14% and 1.5% missing, p99 125-207 ms) -> 71.3 / 71.3%
(nothing missing, p99 56-68 ms). In this layout the generator's own vCPU saturates
from 128 users (92-94%, thousands of skipped sends) and several runs of both
transports failed only on generator slips, so the off-host series is the capacity
reference.

In short, the shard transport uses about half the SFU CPU of the webrtc transport on
the production host for the same audio load, and 160 users stay clean where 128 were
already at the webrtc transport's edge.

Not yet measured: TURN-relayed clients, browsers and camera/screen video at scale on
the VPS, mixed chat and voice load, and real clients and networks. Off-host video and
a per-shard CPU breakdown at 192 users would be the next runs.
