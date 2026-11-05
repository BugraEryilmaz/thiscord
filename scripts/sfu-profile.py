#!/usr/bin/env python3
"""Attribute an SFU `perf record -g` profile to forwarding stages.

Reads `perf script` output (frame-pointer call graphs of `voice_load_server`
built with `-C force-frame-pointers=yes`) on stdin. Each sample is charged to
the first recognized stage on its stack, by priority (our ingress reader, relay fan-out,
egress writer, the webrtc-rs driver, ...) and, separately, classified by what
its leaf frame does (kernel networking, allocation, copies, SRTP crypto, ...).
Prints both tables and the stage x leaf matrix. See docs/load-benchmarks.md.
"""
import collections
import re
import sys

# Priority order: a sample belongs to the first stage found anywhere on its stack.
STAGES = [
    ("our egress writer (revalidate + write_rtp)", re.compile(r"voice::egress::Writer>::run")),
    ("our relay (authorize + fan-out)", re.compile(r"voice::serve::\{closure#0\}::\{closure#\d+\}")),
    ("our ingress reader (track poll + limits)", re.compile(r"voice::Handler as webrtc::peer_connection::PeerConnectionEventHandler>::on_track")),
    ("our signaling/roster loop", re.compile(r"voice::serve::\{closure#0\}$|voice::serve\b")),
    ("webrtc-rs peer driver (RTP/RTCP, SRTP, ICE)", re.compile(r"webrtc::peer_connection::driver|PeerConnectionDriver")),
    ("webrtc-rs track/interceptors", re.compile(r"rtc_interceptor|rtc::|webrtc::")),
    ("tokio runtime", re.compile(r"tokio::runtime")),
]
LEAVES = [
    ("kernel: UDP/IP/netfilter", re.compile(r"\[kernel|udp|ip_|nf_|nft_|skb|netif|xmit|sock|syscall|entry_SYSCALL")),
    ("allocation (malloc/free)", re.compile(r"malloc|free|realloc|_int_|tcache|arena")),
    ("memory copy/zero", re.compile(r"memcpy|memmove|memset|rep_movs")),
    ("SRTP crypto (AES-GCM/HMAC)", re.compile(r"aes|gcm|ghash|sha1|hmac|ring_core", re.I)),
    ("hash maps", re.compile(r"hashbrown|HashMap|sip|RandomState")),
    ("locks/atomics/wakeups", re.compile(r"futex|Mutex|RwLock|semaphore|Semaphore|wake|Notify|event_listener|concurrent_queue|async_channel")),
    ("libc, unsymbolized (mostly malloc/free/memcpy)", re.compile(r"libc\.so")),
]


def classify(frame, table, default):
    for name, pattern in table:
        if pattern.search(frame):
            return name
    return default


def samples(lines):
    stack = []
    for line in lines:
        line = line.rstrip("\n")
        if not line.strip():
            if stack:
                yield stack
            stack = []
        elif line.startswith((" ", "\t")):
            parts = line.strip().split(None, 1)
            stack.append(parts[1] if len(parts) == 2 else parts[0])
    if stack:
        yield stack


def main():
    stage_count = collections.Counter()
    leaf_count = collections.Counter()
    matrix = collections.Counter()
    total = 0
    for stack in samples(sys.stdin):
        total += 1
        leaf = classify(stack[0], LEAVES, "other leaf (application logic)")
        # The highest-priority stage anywhere on the stack: work our tasks
        # cause inside webrtc-rs calls is charged to our stage.
        stage = next((name for name, pattern in STAGES
                      if any(pattern.search(frame) for frame in stack)), "other/unattributed")
        stage_count[stage] += 1
        leaf_count[leaf] += 1
        matrix[(stage, leaf)] += 1
    if not total:
        sys.exit("no samples")
    pct = lambda n: f"{100 * n / total:5.1f}%"
    print(f"samples: {total}\n\nBy stage (highest-priority stage on the stack):")
    for name, n in stage_count.most_common():
        print(f"  {pct(n)}  {name}")
    print("\nBy leaf activity:")
    for name, n in leaf_count.most_common():
        print(f"  {pct(n)}  {name}")
    print("\nStage x leaf (>= 1%):")
    for (stage, leaf), n in matrix.most_common():
        if n * 100 >= total:
            print(f"  {pct(n)}  {stage} :: {leaf}")


if __name__ == "__main__":
    main()
