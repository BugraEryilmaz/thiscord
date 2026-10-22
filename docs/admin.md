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

The **Performance monitors** section adds automatic backend operation timings,
recent rates, database waits, unchanged polling counts and sampled SFU work.
Selecting a participant also shows automatically collected desktop UI timings;
audio deadline/queue metrics remain under advanced diagnostics. No user action
or file logging is required. See the prioritized [performance audit](performance-audit.md)
for exact scopes, budgets, limitations and how to confirm each suspected issue.

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


## Remote audio support

Click any participant name to open **Audio settings**. Updated desktop clients in
an active voice connection expose their actual devices, saved preferences, meters,
echo/quality diagnostics and speaker gains. The dialog renders the same
`AudioSettingsPanel`, shortcut, overlay and diagnostic-logging components as the
local desktop, using an explicit remote command adapter. Changes use the native
save path, apply during the call, and persist on that computer; the overlay toggle
retains its existing session-only behavior. The desktop refreshes audio preferences
so remote changes appear locally. Local-only recording, filesystem-opening,
push-to-talk hold and connection/test actions are not remotely callable.

**Download diagnostics** produces JSON containing current settings, numeric audio
status and the three technical JSONL log generations (up to 6 MiB of source logs).
Downloads remain available if live audio status fails, with the failure included
in the JSON and unavailable snapshot fields set to null.
Logging may be enabled before reproducing the issue; when disabled, existing logs
remain downloadable alongside the current snapshot. Recording files, PCM, session
tokens and arbitrary files are excluded. Device names and configured model paths
are visible to the administrator. Log rotation can overlap a download; the export
is a best-effort snapshot, not an atomic capture of the logger queue.

`POST /api/v1/admin/audio` accepts shared, allowlisted commands. Each request checks
current instance owner/admin privileges under account/session locks, then validates
the scoped authorization snapshot before signaling and again before returning data.
Guild administrators do not qualify. Revoked/expired sessions and demoted admins
cannot dispatch queued commands or retrieve pending replies. Already dispatched
operations may have applied; a timeout is not an acknowledgment and is never
retried automatically. The browser should refresh before another change.

Requests bind to a specific media session's voice connection ID. A full reconnect,
replacement, leave or voice-access revocation removes that target; commands never
follow an account to a new media session. Signaling-only recovery retains the target,
but rejects an interrupted request instead of replaying commands or partial replies.
New requests while signaling is detached receive an unavailable error; after resume,
administrators can issue a fresh request to the same target. Offline users and older clients show an unavailable
message. Both offer and answer negotiate support, retaining compatibility with old
servers and clients. No extra socket, database migration or persistent remote
settings store is introduced.

At most eight admin operations are admitted globally, with one active operation
and one queued request per connection. A 60-second deadline and 16 MiB reply cap
bound retained state. Replies travel over authenticated voice signaling in 32 KiB
UTF-8 chunks paced at ten per second, separately rate limited from media signaling.
Only the targeted socket can complete a request. No settings, log contents or
command payloads are written to server logs. Large reply parsing and serialization
run on bounded blocking workers. Responses use `Cache-Control: no-store`.

Device/processing edits compare the administrator's displayed settings against the
latest native preferences under the same lock as application and persistence.
Concurrent desktop/admin changes are rejected with a refresh message, rather than
silently overwritten. Mute/deafen commands retain the native latest-state toggle
behavior and device saves preserve those controls.

Tests cover authorization/demotion, disconnect identity, reply bounds, cross-socket
reply rejection, legacy capability defaults, stale native settings and export file
selection. Real-device acceptance remains listed in TODO.md.
