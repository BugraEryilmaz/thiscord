# Planned restarts and in-place ICE restarts

Two mechanisms keep calls alive when the network or the server changes:

- **Planned restarts** (a deploy, `systemctl restart`/`stop`, maintenance): the
  backend drains, tells clients it is restarting, and clients rejoin the next
  process on their own with a calm "Reconnecting…" state.
- **In-place ICE restarts**: a lost media path, a network change or expiring TURN
  relay credentials restart ICE on the existing peer connection; only when that
  fails does the client fall back to a full reconnection.

## Planned restarts

### Server (`backend/src/shutdown.rs`)

On SIGTERM or Ctrl-C (`main.rs`), the router's `Shutdown` enters the draining
state. The state belongs to the router, not the process, so tests can drain one
router while others serve.

1. Readiness (`/api/v1/ready`) answers 503; liveness (`/health`) stays 200.
2. New voice, chat, friends and notification WebSocket upgrades get a retryable
   HTTP 503. A voice `Join` already on an open socket gets a `ServiceUnavailable`
   error (older clients retry that too).
3. Every negotiated voice session that opted in hears
   `ServerEvent::Restarting { retry_after_ms }`, then the socket closes with code
   1012 (RFC 6455 "Service Restart", `voice::RESTART_CLOSE_CODE`). Older clients
   only see the 1012 close. The hint is 1.5 s plus up to 1.5 s of jitter per
   connection, so a room does not reconnect in the same instant.
4. Chat sockets opened with `?restart_notice=1` (`chat::RESTART_NOTICE_QUERY`) get
   the chat `Restarting` event, then every chat socket closes with 1012. Friends
   and notification sockets end; their clients reconnect as after any drop.
5. Rooms drain without voice leave notifications (everyone rejoins shortly). The
   process waits up to `DRAIN_TIMEOUT` (5 s) for every voice connection to end,
   then gives open HTTP requests up to 8 s, then exits. The log records how many
   sessions were notified, how many remained and how long it took.

A new process starts with a three-minute rejoin window (`REJOIN_WINDOW`). In it:

- A `Resume` for a session this process does not know answers `Restarting`
  instead of `ResumeUnavailable`: resume tokens are per process, so the session
  belonged to the previous one. This also covers a client whose signaling was
  already detached when the old process drained, and crash restarts within the
  client's 20-second resume window. Outside the window the old rule applies.
- Automatic rejoins (`JoinAttempt { reconnect: true }`) do not announce a voice
  join notification.

Trade-offs of the window: a released 0.1.61 client resuming in it reports an
unexpected resume response instead of "Voice session ended" (its call ends either
way). A current client whose signaling was lost while a moderator disconnected it
(without changing permissions) and that then reaches a restarted server rejoins;
any permission change still refuses the rejoin.

Rejoins are ordinary `Join`s: session, guild membership, channel permissions,
timeouts and moderation are checked against the database again, the same as any
first join. Nothing from before the restart (grants, rosters, resume capabilities)
is trusted. A permission removed during the restart refuses the rejoin and ends
the call with the usual message.

### Clients

The desktop app and the `/app` web client handle restarts the same way:

- `Restarting` (or a 1012 close, or a `Restarting` reply to a resume) ends the
  attempt as a calm, retryable restart. Signaling recovery never resumes into the
  next process.
- The first rejoin waits for the hint, then retries every 1.5–2.5 s for up to
  three minutes while the next process starts (connections are refused or get
  503 meanwhile). The ordinary 1/2/4/…/30 s schedule resumes after that. A server
  `ServiceUnavailable` error starts the same steady pacing.
- The rejoin reuses the call's `call_id` with `reconnect: true`, keeps self mute,
  deafen and every audio setting, and keeps the last participant roster on screen.
  The voice dock shows **Reconnecting…** with a slow breathing animation (none with
  reduced motion) and a tooltip saying Thiscord is restarting.
- Screen sharing and the camera are not restarted without the user. After the
  rejoin the dock offers **Resume sharing** (the same source, audio and quality on
  desktop; the browser asks again, as it must) and **Turn camera back on**, or
  **Not now**. The camera control does not show its device error for this stop.
- The chat socket opts in to the notice, shows "Thiscord is restarting.
  Reconnecting…" instead of an error and retries on the same steady schedule.

### Compatibility

All of this is additive. Servers advertise `restart_notice` and `ice_restart` in
the offer; clients echo them in the answer only when offered, because older servers
deny unknown answer fields. Older clients never receive `Restarting` or
`IceRestart` on a negotiated session; they see the 1012 close and behave as before
(the call drops after signaling recovery finds no session). Chat uses a URL query
because older servers deny unknown `connect` fields but ignore queries.

### Operations

`infra/vps/thiscord.service` uses `KillSignal=SIGTERM` and `TimeoutStopSec=30`, so
`systemctl stop`, `restart` and the deploy script all take the graceful path; the
drain needs at most about 13 s. `thiscord-deploy` stops the backend (drain),
backs up the database, switches the release and starts it; clients rejoin as soon
as the new process is ready (the backup's duration is the main part of the gap).
Do not lower `TimeoutStopSec` below about 15 s, and do not use `kill -9` for
planned restarts.

## In-place ICE restarts and relay credential refresh

### Wire contract

- `ClientEvent::IceRestart {}`: only after `ice_restart` was negotiated (otherwise
  the server ends the session as a protocol error).
- `ServerEvent::IceRestart { sdp, ice_servers }`: a restart offer for the existing
  session with **fresh** TURN credentials, minted per request.
- `ClientEvent::IceRestartAnswer { sdp }`: the gathered answer. An answer without
  an outstanding offer, or an invalid one, ends the session.
- `IceServer.expires_in_secs`: the relay credentials' lifetime from receipt.

The server keeps one restart in flight, ignores requests closer than 2 s apart,
and ends the session if an offer stays unanswered for 15 s (the client then
reconnects in full). Tracks, slot, DTLS/SRTP, permissions and the media session are
unchanged; only ICE credentials and candidates change. TURN credential lifetime is
`THISCORD_TURN_TTL_SECS` (60–86400 s, default 3600; validated at startup).

### Both SFU transports

- `webrtc`: `create_offer` with `ice_restart`, then the client's answer through
  `set_remote_description`. Host candidates are kept.
- `shard`: `Command::RestartIce` creates the restart offer in the sans-I/O core and
  registers the new local ufrag with the shard's demux. The old ufrag stops
  routing; learned addresses keep the current path until the restart completes.
  A credential-checked STUN request at the 16-address bound replaces the peer's
  oldest address (a roaming client is never locked out).

The voice integration suite runs the restart tests under both transports.

### Client behavior (`frontend/src/ice_recovery.rs`)

One state machine drives both clients:

- **Lost path**: once ICE reports the connection disconnected (about 5 s without
  traffic) and it stays so for 1 s, the client requests a restart (never while
  signaling itself is recovering). No offer within 10 s, or no reconnection
  within 10 s of answering, falls back to a full reconnection. Failed/Closed peers
  still reconnect in full at once.
- **Network change**: the browser restarts as soon as it comes back online. The
  desktop client's sockets are bound to the default-route address, so a new local
  address reconnects in full at once (checked every 3 s) instead of waiting for
  ICE to fail.
- **Relay refresh**: `expires_in_secs` schedules a check five minutes before
  expiry (a fifth of the lifetime for short ones). If the selected pair's local
  candidate is a relay (or unknown), the client restarts ICE with the fresh
  credentials. A direct path checks again every 30 s until expiry. coturn
  validates the REST username's expiry on allocation refreshes, so without this a
  relayed call would end soon after the credentials expired.
- The desktop peer discards local candidates on restart and rebinds its sockets
  (`with_discard_local_candidates_during_ice_restart`), so new relay allocations
  use the new credentials; `set_configuration` applies them first. Browsers apply
  them with `setConfiguration` before answering, and re-gather automatically.
- The dock shows **Reconnecting…** while the path recovers.

An ICE restart interrupts media briefly (the webrtc-rs agent drops its selected
pair at the restart), typically for a round trip plus the connectivity checks;
that is why refreshes happen only for relayed paths.

## Tests and proof

- `backend/tests/voice/restart.rs` (both transports): drain notices, 1012 closes,
  legacy clients without the event, opt-in chat notices, 503 admission, readiness
  versus liveness, bounded drain; a resume after restart gets `Restarting` inside
  the window and `ResumeUnavailable` outside it; a rejoin without current
  permission is refused and an authorized one admitted; repeated ICE restarts with
  new server ufrags and media both ways; unnegotiated and unsolicited restart
  messages end the session.
- Unit tests: `shutdown.rs` (bounded drain, jitter), `voice/ice.rs` (credential
  lifetime and refresh), `shard/demux.rs` (ufrag re-registration, roaming),
  `voice/membership.rs` (quiet rejoin and drain), `ice_recovery.rs` (restart,
  fallback, refresh scheduling), the native and browser retry policies, and the
  native signaling recovery worker (1012 and resume replies become rejoins).
