# Local coturn in Docker Desktop

`infra/coturn/compose.yml` runs the official coturn 4.18.0-r0 image pinned by
digest. Docker Desktop publishes ports on Windows; this is not a service bound
directly to the Kali WSL network interface. Host networking is deliberately not
used because Docker Desktop and the backend's WSL distro have separate networks.

Both backend and clients receive `stun:thiscord.com.tr:3478` and
`turn:thiscord.com.tr:3478?transport=udp`. The backend generates account-bound,
one-hour TURN REST credentials using HMAC-SHA1. The shared secret stays in the
ignored `backend/.env` and `infra/coturn/turnserver.conf`; never print, upload or
commit those files. coturn requires authenticated allocations; STUN binding is
unauthenticated by design. It runs as the image's unprivileged user, with a
read-only root filesystem, bounded allocations, and disabled session logging.

## Setup / address changes

From PowerShell in the repository root:

```powershell
./scripts/setup-turn.ps1 -PublicIp YOUR_PUBLIC_IPV4 -SfuIp YOUR_WSL_IPV4
docker compose -f infra/coturn/compose.yml up -d
```

Find WSL's IPv4 with `wsl -d kali-linux -- ip -4 route`; use the `src` address.
The public IP must match the DNS record for `thiscord.com.tr` and the router's
forwarding destination must be the Windows PC. The setup script validates IPv4
inputs, generates a 32-byte random secret on first use, preserves it on later runs,
updates only the three backend ICE variables, and restricts the private coturn
configuration's Windows ACL. It does not modify other provider settings.

The container uses `172.29.250.2` on an isolated Docker subnet. That exact address
is also used for coturn's public/private address mapping and same-server relay
permissions. If this subnet conflicts with another network, update both Compose
and the configuration template together before setup.

When only coturn's configuration changes, explicitly restart it:

```powershell
docker compose -f infra/coturn/compose.yml restart coturn
```

Restart the backend after changing its `.env`: stop the existing backend with
Ctrl+C in its terminal, then run `./backend/run-wsl.ps1` from the repository root.
An already-running backend does not reload STUN/TURN settings on SIGHUP; SIGHUP
only reloads TLS certificates. Rejoin any voice channel to receive fresh ICE
configuration. Do not start a second backend on the same listening port.

After a WSL IP change, rerun setup with the new SFU IP and restart coturn. Only
that specific private SFU address and coturn's own relay address are permitted;
other private-network peers, loopback and multicast are blocked. This avoids
giving relay users general access to the LAN. The backend's UDP listener must
remain reachable from the Docker container.

## Router and Windows firewall

Forward these ports unchanged to the Windows PC's LAN IPv4 (currently
`192.168.1.126`; reserve it in DHCP):

| Protocol | Public and internal ports | Purpose |
| --- | --- | --- |
| UDP | 3478 | STUN and TURN control |
| UDP | 49160–49259 | Relay media allocations |
| TCP | 3478 | Optional TURN TCP client transport |

The current backend advertises UDP transport. The TCP listener is available,
but automatic UDP/TCP fallback and TURN over TLS are not configured in this pass.
`no-tcp-relay` disables RFC 6062 TCP **peer allocations**, not TURN-over-TCP clients.
WebRTC audio remains encrypted by DTLS-SRTP over the relay. The backend's HTTPS
certificate is independent of this UDP TURN configuration.

If Windows blocks incoming traffic, run `./infra/coturn/open-firewall.ps1` in an
administrator PowerShell. No firewall rules were changed by the local setup.
Do not forward PostgreSQL or use Windows TCP `portproxy` for these UDP ports.
Docker Desktop must be running; `restart: unless-stopped` restarts coturn when the
Docker engine starts. Public-IP changes require DNS/configuration updates. The
backend and local clients must also be able to reach the public hostname from
inside the LAN (NAT loopback/hairpin).

## Verification and operations

```powershell
docker compose -f infra/coturn/compose.yml ps
docker compose -f infra/coturn/compose.yml exec coturn turnutils_stunclient -p 3478 127.0.0.1
docker compose -f infra/coturn/compose.yml down
```

The healthcheck verifies local STUN responses, not Internet reachability or TURN
authentication. For a complete check, use `turnutils_uclient` with temporary REST
credentials and confirm received packet counts; never pass the static secret in
a command or enable verbose logs for real sessions. Test from a separate network
(for example mobile data) before considering public voice connectivity verified.

Local setup verification on 2026-09-28: STUN answered through the public hostname
from Windows and WSL; authenticated relay-to-relay traffic delivered 10/10 test
packets; TURN-to-WSL traffic delivered 5/5; access to an unrelated private peer was
rejected with 403; invalid passwords and expired credentials were rejected with
401. These tests ran inside the hosting network, not off-site.
Backend process liveness and database readiness remained HTTP 200.

### Forced-relay WebRTC verification

From WSL in the repository root, with the backend's configured `.env`:

```sh
cargo run -p thiscord-backend --example turn_probe --locked
```

This manual probe uses the application's WebRTC library and temporary REST
credentials. It opens no microphone, uses no account/session or database, and
prints no credentials, SDP or packet contents. It tests encrypted synthetic Opus
in both directions, first between two relay-only peers, then between a relay-only
peer and a host-only peer on the WSL interface. It checks the selected candidate
types, requires ten received packets each direction and closes both peers.
Direct fallback cannot make the relay-only test pass. It is deliberately an
example rather than a CI test that would require a live TURN deployment.

Verification on 2026-09-29 through `thiscord.com.tr:3478`:

- STUN binding responses succeeded from both Windows and WSL.
- Both local STUN responses reported Docker gateway `172.29.250.1` as the mapped
  address. The hosting network/Docker forwarding path hides the original source
  address on these probes. This is not a usable public server-reflexive address;
  a successful STUN response alone does not prove public NAT discovery works.
- Forced WebRTC selected `relay -> relay`, then `relay -> host`, with ten encrypted
  RTP packets received each direction in each test.
- Authenticated CreatePermission requests accepted WSL SFU `172.31.185.21` and
  coturn relay `172.29.250.2`; Docker gateway `172.29.250.1` and Windows LAN
  `192.168.1.126` were rejected with 403, matching the configured peer restrictions.

All these checks ran inside the hosting network. They verify TURN data forwarding,
including public-hostname access, but do not establish off-site router/firewall
reachability or the STUN mapped address seen by an external client. Repeat from
another network before claiming that coverage.

### Reading voice connection logs

After restarting a backend containing the route diagnostic, each joined voice
connection logs `voice media route selected` once its first ICE pair is selected.
The `local_candidate_type` and `remote_candidate_type` fields contain only types:
`relay` on either side confirms that the selected route includes TURN. Absence of
`relay` in the backend log does **not** rule out a client relay: the receiving
agent can learn its translated source address as `prflx` (peer reflexive), even
when the sending client selected a local relay candidate. See [ICE peer-reflexive
candidate discovery](https://www.rfc-editor.org/rfc/rfc8445.html#section-7.3.1.3).
A later ICE route change is not tracked by this one-time diagnostic.

The forced-relay probe now prints both peers' selected candidate types. On
2026-09-29 the relay-to-WSL test reproduced `local=relay, remote=host` at the
relay-only sender and `local=host, remote=prflx` at the receiver, while delivering
ten encrypted RTP packets in each direction. Therefore the backend's `host/prflx`
pair alone cannot distinguish a direct client from one using TURN. Inspect the
client's selected **local** candidate, or confirm a rebuilt relay-only client
successfully exchanges audio. The local reproduction does not prove which route
an uninstrumented installed client used.

The library can try relay permissions for private candidates even when another
candidate succeeds. A 403 means coturn rejected that requested peer address,
not that every relay allocation failed; see [TURN CreatePermission processing](https://www.rfc-editor.org/rfc/rfc8656.html#section-9.2).
The local forbidden-address probes reproduce this error. Preserve the private
peer restrictions; do not allow the entire LAN just to suppress these logs.
`unhandled STUN packet` is a library ICE-handler diagnostic, not proof of a
successful or failed relay path. Use selected candidate types and actual packet
delivery to assess the route; the historical warnings alone do not identify it.

One-hour credentials currently have no automatic refresh/ICE restart. Long calls
may require leaving and rejoining; this remains tracked in TODO.md. coturn is a
network relay, not a replacement for the Rust SFU.

References: [official coturn Docker image](https://github.com/coturn/coturn/blob/master/docker/coturn/README.md),
[coturn TURN REST authentication](https://github.com/coturn/coturn/blob/master/README.turnserver).
