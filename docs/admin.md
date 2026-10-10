# Browser administration

The browser dashboard is served at `/admin` (also `/admin/`) by the Rust backend
when its web bundle is installed. Sign in with an existing password or Google
account. Only the **instance owner and instance admins** can retrieve diagnostics;
guild ownership and guild Administrator do not qualify. Use the existing local
owner bootstrap and instance controls described in [permissions.md](permissions.md).

The static login shell is public; operational data comes from bearer-authenticated
`GET /api/v1/admin/diagnostics`. Each request rechecks the current session and instance
role under shared account/session locks. Account and per-account instance-role
barriers validate the snapshot again before serializing the response. Demotion,
ownership transfer, logout and session revocation therefore also reject diagnostics
whose telemetry collection overlaps the revocation. Instance-role barriers are
separate from guild/chat access, and unrelated users' changes do not reject a
snapshot. Two-request admission covers the entire operation, before database work
and through telemetry collection; no authorization locks span telemetry waits.
This explicitly permits inspection of instance-wide room metadata, including rooms the admin has not joined.
It does not grant guild membership, message access or permission to join/listen to media.

Browser session tokens stay in memory; reloading requires signing in again.
The dashboard polls every five seconds, clears data on errors or denied access,
and times out unresponsive requests after four seconds. Requests do not overlap.
Responses and web assets are `Cache-Control: no-store`. Admin uses same-origin API
requests; desktop and ordinary browser preview retain compile-time `THISCORD_API_URL`.

## Measurements

- Host CPU: Linux `/proc/stat` deltas across all cores, normalized to 0–100%.
  I/O wait is idle; guest time is not double counted. The first sample has no CPU
  value. Host readings are cached for four seconds.
- Host RAM: `MemTotal - MemAvailable`, plus total RAM. Backend RAM is RSS.
  Load averages are Linux 1/5/15-minute values. These describe the host or VM,
  not container/cgroup limits. Unavailable `/proc` fields appear as `—`.
- Backend uptime/version and current open/idle database pool connections.
  Diagnostics require a working database; liveness remains independent.
- Active voice rooms, server/channel names, participant counts, unique account
  count and mute/deafen/screen-share state. Connections negotiating media may
  appear before network statistics are available.
- Latency: the nominated ICE transport's most recent STUN round trip between
  that client and the SFU, not one-way or mouth-to-ear delay.
- Jitter: most recently reported microphone RTP inter-arrival jitter at the SFU.
  Pinned `rtc 0.21.0` copies raw RTCP ticks into stats despite documenting seconds;
  the adapter converts Opus's 48 kHz clock to milliseconds. Recheck on upgrades.
  See the [WebRTC definitions](https://www.w3.org/TR/webrtc-stats/). Muted/idle
  streams may retain their last reading; users with no microphone packets show `—`.
- Microphone received/lost and video queue drop counters are cumulative per
  connection. Lost counts can be negative after duplicate/recovered packets.

WebRTC stats are sampled every five seconds with a 100 ms deadline, independently
of signaling/media forwarding. Registry locks do not span telemetry waits. The UI
hides network readings older than fifteen seconds. Raw reports, candidate IPs, SDP,
credentials, messages and media never enter the response. There is no persisted
history. The registry and invalidation design cover **one backend process**.

## Build and serve

Use pinned Rust, Trunk 0.21.14 and the standalone Tailwind in `Trunk.toml`:

```powershell
cd frontend
trunk build --release --locked --public-url /admin/ --dist dist-admin
```

Install `frontend/dist-admin` contents into `admin` beside the backend executable.
Alternatively set `ADMIN_DIST_DIR` to the bundle's absolute directory (a Linux
path for WSL). An explicitly configured missing/invalid directory fails startup.
Without configuration or an adjacent bundle, API-only development still works.

The backend hashes Trunk's inline bootstrap for its CSP, restricts scripts and
connections to the same origin and disables framing. Keep bundle files immutable
for the process lifetime; restart after replacement. Serve only the built bundle,
never source or secrets. Production requires HTTPS and the public origin in
`ALLOWED_ORIGINS` for account login.

`infra/vps/deploy-backend.sh` builds/installs the bundle in the binary's versioned
release. Install pinned Trunk on the build host first. Build-only verifies both
builds; ordinary deployment uses the existing backup/activation/health procedure.
Deployment restarts the backend and disconnects active calls. CI builds a separate
admin-assets artifact without changing desktop asset paths.

Tests cover current instance privileges, demotion/revocation, static routing/CSP,
measurement units and live multi-room presence/telemetry/cleanup. No migration or
additional package is needed.
