# Voice load generator (Mac to desktop)

`voice_load` is a headless Rust WebRTC client generator. `voice_load_server`
hosts the real backend HTTP/signaling/SFU routes and PostgreSQL permission checks
against a disposable test schema. Both are examples in the backend package;
there is no fourth package, Node pipeline, microphone access or GUI dependency.

The server seeds distinct test accounts, hashed bearer tokens, one guild and
voice channels. It grants no instance ownership/admin rights. Authentication,
permission checks, the five-connection database pool, SFU queues and eight-person
room limit remain active. Only the benchmark router's connection limit changes
to the requested fixture size (2–500). A normal backend still admits at most 64
voice connections, even when compiled with the `load-test` feature.

This measures steady-state **voice transport**, not codec quality, login hashing,
text-chat throughput or real microphone/device performance. The payload is
synthetic RTP labeled as Opus, not decodable speech. Its default 80-byte payload
at 50 packets/second models 32 kbit/s encoded audio without silence suppression.
WebRTC still encrypts/decrypts and forwards it through the actual SFU. Never join
these fixture rooms with a normal audio client.

## Build on the desktop in WSL

From the repository root in Kali WSL, use the pinned Rust toolchain and existing
PostgreSQL development dependencies. See [database.md](database.md).

```sh
source "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/.cache/thiscord-target"
cargo build -p thiscord-backend --features load-test --release --locked --example voice_load_server
```

The server reads `TEST_DATABASE_URL` from its environment, or only that key from
the local ignored `backend/.env`. The database name must end in `_test`; URL
`options` overrides are rejected. Do not use the application database.
Migrations and fixture writes run in a fresh `voice_load_<random UUID>` schema.
The Mac never receives database access or database credentials.

Generate a random **64-character hexadecimal secret** in your password manager.
Enter the same secret in the two terminals below. It derives temporary benchmark
session tokens in memory; database rows contain their hashes only. Do not put it
in command arguments, committed files or reports. Use a fresh secret when
restarting the benchmark server.

In WSL's Bash terminal:

```sh
read -rsp 'Benchmark secret: ' THISCORD_LOAD_SECRET
echo
export THISCORD_LOAD_SECRET
"$CARGO_TARGET_DIR/release/examples/voice_load_server" --bind 0.0.0.0:3001 --users 64 --room-size 8 --allow-insecure
```

This starts a **separate** server on port 3001. Stop other workloads when measuring
capacity. `--users` is both the number of fixture accounts and this server's
connection ceiling. To test 200, restart it with `--users 200`; the client can
then run any smaller count with the same room size. Keep fixture size fixed
through a comparison: guild permission evaluation loads membership, so fixture
size itself affects work even when some accounts are disconnected.

Ctrl+C stops the server and drops its specific schema. Stop the generator first.
An abrupt kill/power loss can leave that printed `voice_load_*` schema behind;
inspect and remove only that schema when no corresponding server is running.
Do not reset the test database. No existing application tables are modified.

## Network setup for the Mac

The Mac needs both:

1. TCP access to the benchmark listener on the desktop's LAN IP, port 3001.
2. A working WebRTC media route to the backend's UDP candidates, directly or
   through your configured TURN server.

WSL localhost forwarding alone does **not** make UDP reachable from another
computer. A TCP port proxy can expose port 3001, but does not solve media routing.
Use your established working WSL/Windows voice network configuration. Bind media
to a reachable interface with `THISCORD_VOICE_BIND=IP:0`; allow the associated
UDP traffic through Windows/WSL firewalls. Do not use one fixed UDP port for all
peers. Prefer a wired desktop connection while the Mac uses Wi-Fi.

The benchmark server intentionally does **not** automatically load the production
ICE/TLS/provider settings from `backend/.env`. If your WSL configuration requires
TURN, explicitly export the same `THISCORD_STUN_URL`, `THISCORD_TURN_URL` and
`THISCORD_TURN_SECRET` into the **server** environment before launching it.
See [turn.md](turn.md) for the existing deployment's routing requirements.
The client gets only short-lived TURN credentials over signaling. Add `--relay`
on the client to disallow direct ICE fallback on that client and measure relay
transport specifically. It does not force the SFU's own candidate policy.

`--allow-insecure` permits unencrypted signaling for this explicit trusted-LAN
test; media is still SRTP encrypted. For other networks, put this listener behind
your HTTPS/WebSocket proxy, or start the server with `--tls` and exported
`TLS_CERT_PATH`/`TLS_KEY_PATH`. Use `https://...` on the client. Certificate and
hostname verification remain enabled; there is no skip-verification option.

If warmup fails, fix routing before adding clients. Every receiver must get a
warmup packet from every other expected sender in its room. Negotiated sockets
alone never count as a successful test. On macOS, also check Terminal's local
network permission if connections are blocked.

## Build and run on macOS

Use the same repository revision and pinned Rust toolchain. The backend package
needs PostgreSQL **client libraries** to compile, but the generator does not run
PostgreSQL or connect to a database.

```sh
brew install libpq pkg-config cmake
export PQ_LIB_DIR="$(brew --prefix libpq)/lib"
cargo build -p thiscord-backend --release --locked --example voice_load
```

In the Mac's default Zsh terminal, enter the same benchmark secret:

```sh
read -rs 'THISCORD_LOAD_SECRET?Benchmark secret: '
echo
export THISCORD_LOAD_SECRET
```

Find the Mac's LAN IP in System Settings → Wi-Fi → Details → TCP/IP. Substitute
both addresses below; the `--bind` address is the **Mac's**, and its port must
stay `0` so each client gets a separate UDP socket. If `CARGO_TARGET_DIR` is set
on your Mac, use its `release/examples/voice_load` path instead of `target/...`.

```sh
./target/release/examples/voice_load \
  --url http://192.168.1.100:3001 \
  --bind 192.168.1.101:0 \
  --allow-insecure \
  --users 8 --room-size 8 --seconds 60 \
  --output voice-8.json
```

Start with 8, then 16, 32, 48 and 64 participants. Above 64, increase the
benchmark server's fixture size first. Room size must match on both machines.
Counts can include a partial final room, but a one-person final room is rejected
because it has no forwarding workload. Each run joins at most four peers
concurrently, verifies all media paths, then starts a shared measurement window
and drains incoming traffic for two seconds afterward.

Use 10-minute runs (`--seconds 600`) near the limit, followed by a one-hour run
(`--seconds 3600`). The output file must be new; existing results are never
overwritten. Ctrl+C generates an incomplete report and closes connections. Wait
for the process to exit before restarting. Very rapid repeats can hit the normal
six-joins-per-account-per-minute limiter; wait a minute instead of disabling it.

Additional options:

| Option | Default | Range / purpose |
| --- | --- | --- |
| `--users` | 8 | 2–500 simulated accounts |
| `--room-size` | 8 | 2–8 users per room; match server fixture layout |
| `--seconds` | 60 | 1–3600 measured seconds, excludes joins/warmup/drain |
| `--payload-bytes` | 80 | 32–1200; 80 models 32 kbit/s encoded payload |
| `--late-ms` | 100 | 1–1000; late-packet threshold for the complete network path |
| `--relay` | off | Require client relay candidates; server must configure TURN |
| `--output` | none | JSON report with aggregate and every sender/receiver pair |

## Reading the results

- `scheduled`, `attempted`, `sent`: planned 20 ms sends, actual attempts and
  successful WebRTC writes. `generator_skipped` is scheduling work the Mac did
  not perform; `send_errors` is failed/timed-out writes. Neither is disguised as
  downstream network loss. A successful write means accepted by the client stack,
  not proof that a packet reached the wire.
- `expected_deliveries`: successful sends multiplied by actual other participants
  in each room. `missing` compares bounded per-stream sequence bitmaps against
  those sends, including entire missing streams and trailing loss. Late packets
  arriving during the drain are received, with lateness recorded separately.
  Reordering/duplicates do not inflate successful delivery. Sequence accounting
  remains correct past RTP's 16-bit sequence wrap.
- `path_p50_ms`, `path_p95_ms`, `path_p99_ms`: generator send → SFU → generator
  receive, measured with one monotonic clock on the Mac. Includes both network
  legs, client processing and queues. **Not server-only forwarding time**, and no
  desktop clock synchronization is required. Histograms use 1 ms buckets; `1001`
  means at least 1001 ms. Null means there were no measured deliveries.
- `streams`: each receiver/sender pair's expected, received, missing, late,
  duplicate and reordered packet counts and p99 delay. Check the worst stream,
  not only aggregate averages.
- `generator_max_scheduling_lag_us` and `generator_scheduling_over_20ms`: sender
  scheduling delays relative to the next expected tick. The timer skips missed
  slots rather than bursting catch-up packets into the server.
- `routes`: selected local/remote ICE candidate types for each client (host,
  server-reflexive or relay), without addresses or credentials. `--relay` also
  verifies that the selected client candidate is a relay.
- `invalid_or_cross_room`: malformed, wrong-run, self or cross-room test payloads.
  They fail the run. Packet bodies, account tokens, SDP and TURN secrets are never
  included in reports or error messages.

The built-in conservative PASS requires a completed run, zero failed clients,
zero write errors/invalid packets, at most 0.1% skipped generator slots, and at
most 0.1% missing-plus-late packets **on every expected stream**. The default
100 ms threshold is for the entire network path, not the earlier proposed 20 ms
server-only forwarding target. Adjust `--late-ms` for your experiment and keep
it constant across comparisons. Exit codes: 0 PASS/help, 1 setup/output error,
2 failed threshold or incomplete run. PASS is a workload result, not a promised
production capacity.

On the desktop monitor backend **per-core** CPU, PostgreSQL CPU, memory/swap,
network and disk latency. On the Mac monitor the generator's CPU/memory and
Wi-Fi. This generator does not yet instrument backend lock waits or query
latency. Periodic voice authorization no longer takes the global chat/auth gate
or pauses packet forwarding. Access-changing operations still drain media and
require reauthorization. Database contention, media queues and network waits can
still cause delay with low CPU utilization; repeat measurements after rebuilding.

Same-LAN Wi-Fi tests exercise the access point and desktop network stack, **not
your ISP's upload limit**. All simulated clients share one radio and one machine;
high client CPU or Wi-Fi contention can limit the result before the server does.
Repeat over wired Ethernet to isolate server capacity, and off-site to test WAN
and TURN behavior. Report hardware, fixture size, room size, network route,
payload size, build profile and thresholds with every capacity claim.

## Verification

```sh
cargo test -p thiscord-backend --features load-test --example voice_load --example voice_load_server --locked -- --include-ignored
cargo build -p thiscord-backend --features load-test --example voice_load --example voice_load_server --locked
python3 scripts/voice-load-smoke.py "${CARGO_TARGET_DIR:-target}/debug/examples/voice_load_server" "${CARGO_TARGET_DIR:-target}/debug/examples/voice_load"
```

The smoke script requires the test database and uses loopback, a random in-memory
secret and temporary reports. It checks one/two-room encrypted delivery,
accounting, rejected credentials/capacity and reconnect cleanup. Unit tests check
sequence wrap, duplicates, reordering, total/tail loss and URL validation.
Linux CI runs these checks; macOS CI compiles/lints/tests the generator. Sustained
desktop/Pi capacity and physical Mac-to-Windows Wi-Fi acceptance remain pending.
