# Evening responsiveness investigation, 2026-10-21

Investigated on October 22 using the deployed VPS journal through `ssh vps`,
historical sysstat samples, the source, and the existing local desktop diagnostic
files. The journal window was October 21 15:00 UTC through October 22 03:00 UTC.
Times below are UTC; add two hours for Europe/Zurich on this date. These are
observations of the incident, not measurements of a deployed fix.

## Evidence

The backend was running release `0bd9c2804726-20261020T220910Z.m37Qk94M`, started
October 20 at 22:09:11, with no systemd restarts. Local diagnostics identify
client version 0.1.37. The working source has subsequent screen-sharing changes.

| HTTP operation | Count | Median | p95 | Maximum |
| --- | ---: | ---: | ---: | ---: |
| POST `/api/v1/permissions` | 84 | 5 ms | 7 ms | 32 ms |
| POST `/api/v1/account` | 11 | 9 ms | 61 ms | 61 ms |
| POST `/api/v1/chat` | 2 | 9 ms | 9 ms | 9 ms |
| GET `/api/v1/admin/diagnostics` | 1,288 | 4 ms | 5 ms | 31 ms |

All these requests returned 200. These are handler timings, excluding the time
before the request reaches the application and delivery of the response body.
There were also 25 permissions OPTIONS requests. Server selection performs one
`ViewGuild` HTTP request through the WebView; it does not wait for the chat socket
before rendering channels. JSON/bearer requests require a CORS preflight when its
permission is not already cached.

Sysstat samples from 17:00 through 23:59 averaged 97.53% CPU idle, 0.09% I/O wait,
and about 3.0 GiB available memory. The network interface reported no errors or
drops in those samples. No non-firewall kernel warnings or PostgreSQL service
warnings appeared in the checked window. These samples do not rule out short
stalls or loss elsewhere on the network, but do not support VPS saturation as
the primary explanation for a 700 ms server switch.

Of 67 voice upgrades, the recorded endings were 35 signaling reattachments,
11 signaling receive failures, six client leaves, five microphone refresh
timeouts, five cancelled resume authorizations, four joins with no initial frame
within five seconds, and one negotiation timeout. The 13 logged successful media
connections all selected host-to-server-reflexive routes. ICE password-empty,
SRTP-not-ready, TURN permission 403 and TURN refresh 401/437 messages also occurred.
They do not by themselves establish the join failure: private candidate denial
is intentional, and the selected routes did not use TURN relay candidates.

The local client logs corroborate a signaling deadline problem:

- The microphone refresh expired at 20:46:31 on the server. Client recovery
  began at 20:46:34.050 and its new socket took 2.372 seconds to connect, ending
  with `resume_unavailable`.
- The next call reported media connected at 20:46:51.404 and began recovering
  at 20:46:56.416, almost exactly five seconds later. Its resume acknowledgement
  and the terminal server error arrived together at 20:46:59.032.
- Another attempt took 16.427 seconds from the client WebSocket connection to
  peer `Connecting`; the server had already reached its 15-second negotiation
  timeout. No audio startup occurred during that wait. Existing records cannot
  separate delayed offer delivery from client peer/ICE operations.
- The retained evening client socket-connect measurements ranged from 886 to
  2,372 ms. Audio worker heartbeat ages around the failures were 2–9 ms with no
  worker failures; DSP initialization took about 237–306 ms. These records do
  not establish slow audio processing as the incident's cause.

## Changes and limits

The strongest actionable defect is matching client and server five-second
privacy deadlines. The server's timer can start before the client receives the
challenge; recovery after five seconds is already too late. The client now starts
signaling recovery after two seconds without a boundary acknowledgement. The
server's proof/Ready deadline, fresh authorization, microphone fencing and
terminal rejection behavior remain intact. Resume does not renew that deadline.
Severe downstream delay can still consume the recovery margin.

The native call loop now polls one outstanding audio startup/keepalive operation
alongside signaling. Previously it awaited a worker command inside the timer
branch, potentially withholding privacy-message processing for the command's
ten-second timeout. This fixes a concrete stall path, but is hardening rather
than a proven explanation of the recorded incident.

HTTPS now enables `TCP_NODELAY` beneath TLS. The installed axum-server 0.8 default
acceptor leaves it disabled. This removes an avoidable delay for small TLS/API
and signaling writes; the size of its benefit on the affected route is unmeasured.
CORS permission is cached for 600 seconds to avoid repeat preflights. API data
remains uncached and authenticated on every request. Repeated clicks on a pending
guild keep the original request instead of superseding useful responses.

The backend records offer-send and answer-receive elapsed times. When desktop
diagnostic logging is enabled, the client records offer receipt, answer creation
and answer transmission, including the ICE-gathering wait. These records contain
timings and request IDs only. The existing bounded negotiation deadline remains
unchanged; this evidence is needed before attributing its timeout to a particular
phase or extending it.

These changes do not prove where the historical signaling delay originated.
No production restart, deployment, firewall change or TURN policy relaxation was
performed during investigation. Recheck cold/warm guild switches, repeated voice
joins and an extended call after deploying the backend and updated desktop.
Correlate request IDs and body-free negotiation phase timings; do not collect SDP,
credentials, message bodies or audio packets. Cross-host wall-clock comparisons
have possible clock skew; use per-process elapsed values where available.

## Validation

Targeted checks passed on this workstation:

- Six navigation state tests, four backend HTTP tests and the HTTPS certificate,
  hostname and reload integration test.
- Eight microphone-boundary tests and three audio-control tests, including a
  held worker that cannot block ACK handling, a cancelled late VoiceStart, and
  cancelled settings publication while a replacement owns the settings lock.
- Six native signaling recovery tests. The new real-WebSocket regression delays
  the challenge by 200 ms, withholds its ACK and delays the replacement upgrade
  by 2.4 seconds. Using the production two-second deadline wake, it completes
  recovery and Ready within the original five-second budget (observed 4.62 s).
  This tests the native client against a controlled protocol peer, not a live
  production SFU or a guarantee for arbitrary network delays.
- WASM UI and Windows desktop compilation, WSL backend compilation, targeted
  Clippy checks and workspace formatting.

Independent reviews prompted the cancelled-settings-publication fix and removal
of a favorable test-only polling interval; the final review found no actionable
correctness concerns. Physical headset behavior, an actual multi-user call,
Linux/macOS desktop runtime and post-deployment latency remain unverified.
