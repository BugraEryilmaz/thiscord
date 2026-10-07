# VPS backend deployment

The backend is deployed through `ssh vps` to Ubuntu 26.04 at `57.129.178.14`.
The public origin remains `https://thiscord.com.tr`; desktop clients and Google
OAuth callback URLs do not need changing. Cloudflare proxies this hostname, with
A origin `57.129.178.14` and AAAA origin `2001:41d0:801:2000::1151`; use SSL/TLS
Full (strict). The API readiness check passes through Cloudflare.
STUN/TURN must use a separate `turn.thiscord.com.tr` hostname with the same A/AAAA
addresses and **DNS-only** records. Cloudflare's ordinary HTTP proxy does not
forward TURN UDP traffic. This exposes the VPS address; media traffic relies on
the hosting provider's network protection, not Cloudflare's HTTP protection.
HTTPS binds `[::]:443` with dual-stack sockets;
coturn listens and relays on both public address families. PostgreSQL 18 listens
only on loopback.

## Layout and services

- `/home/ubuntu/thiscord`: source checkout; build as `ubuntu`, not root.
- `/opt/thiscord/releases/<commit>/thiscord-backend`: deployed native binary.
- `/opt/thiscord/current`: symlink to the selected release directory.
- `/etc/thiscord/backend.env`: root-owned, service-readable production settings.
  It preserves provider credentials and uses a separate VPS database password.
- `/etc/thiscord/tls`: restricted TLS files read by the backend.
- `/etc/letsencrypt`: migrated ACME account, certificate lineage and renewal config.
- `/var/lib/thiscord`: private writable state for the `thiscord` service account.
- `/opt/thiscord/compose.yml` and `/etc/thiscord/coturn.conf`: Docker coturn setup.
  The latter contains a secret; never commit it or print it in diagnostics.

`infra/vps/thiscord.service` runs the backend as an unprivileged system account,
with only the capability to bind HTTPS port 443. Startup applies embedded Diesel
migrations. systemd restarts failures and starts the service on reboot.

```sh
sudo systemctl status thiscord
sudo journalctl -u thiscord --since '10 minutes ago'
sudo docker compose -f /opt/thiscord/compose.yml ps
curl --fail https://thiscord.com.tr/api/v1/ready
```

Coturn uses host networking on native Linux, avoiding the Docker Desktop NAT
translation used by the old installation. Its pinned image and sandbox settings
are in `infra/vps/compose.yml`. Coturn uses both public relay addresses.
The native backend SFU bind remains IPv4, while
coturn also accepts IPv6 clients. Private/loopback relay peers remain denied.

UFW allows SSH TCP 22, ACME HTTP TCP 80, backend HTTPS TCP 443, STUN/TURN UDP/TCP
3478 and UDP 32768–60999. The last range covers the OS-selected ephemeral SFU
ports and coturn's 49160–49259 relay range. Do not expose PostgreSQL 5432.
If the kernel ephemeral port range changes, review the media firewall rules.

## Backups and certificate renewal

`thiscord-backup.timer` creates a protected custom-format database dump daily at
03:15 UTC with up to 15 minutes of jitter. Dumps are in `/var/backups/thiscord`
and retained for 14 days. The separately named migration archive is retained.
These are on-server backups; keep an additional protected off-server copy.

```sh
sudo systemctl start thiscord-backup.service
sudo systemctl list-timers thiscord-backup.timer certbot.timer
sudo certbot renew --dry-run
```

Certbot uses standalone HTTP validation on port 80. Cloudflare must forward
`/.well-known/acme-challenge/*` to this VPS on HTTP port 80 without redirecting it
to the application's HTTPS listener, caching it, or requiring a browser challenge.
Verify this with a renewal dry run after DNS delegation has propagated.
Its deploy hook `/etc/letsencrypt/renewal-hooks/deploy/thiscord` installs the new
certificate pair with restricted permissions and sends SIGHUP to the backend.
This reloads TLS without disconnecting active sessions. The source hook is
`infra/vps/renew-certificate.sh`.

## Updating the backend

The installed deployment command performs the update as `ubuntu`:

```sh
ssh vps thiscord-deploy
# Or select an explicit reviewed commit/tag:
ssh vps thiscord-deploy COMMIT_OR_TAG
# Verify a build without changing the production service/database:
ssh vps thiscord-deploy --build-only
```

Source: `infra/vps/deploy-backend.sh`, installed at `/usr/local/bin/thiscord-deploy`.
It fetches Git, builds an isolated archive of the selected revision (default
`origin/main`) using its pinned Rust toolchain, and reuses the VPS Cargo cache.
It uses two build jobs with release LTO disabled for the 4 GiB machine. Local
checkout changes and `.env` files are not included in the build archive.

After a successful build it installs a new release directory, stops the backend,
runs the database backup service, atomically switches `/opt/thiscord/current`,
and starts the backend using the existing production environment. Startup applies
migrations. It checks origin HTTPS/database readiness directly before checking
public access through Cloudflare. Voice connections disconnect during the brief
backup/restart window. A lock prevents concurrent runs of this script.

A failure before activation restarts the previous backend if it was stopped.
A failure after activation stops the attempted release for investigation; it does
not automatically roll back because migrations may have changed the database.
The previous release and database backups remain available. Use reviewed migration
rollback behavior or restore a pre-update backup to a separate database. A public
DNS/proxy check failure leaves a healthy origin running and reports the failure.
The command does not update OS packages, coturn, secrets, or its own installed copy.

## Migration evidence

On 2026-10-07 the local PostgreSQL 17 `thiscord` database was logically dumped and
restored into a fresh PostgreSQL 18 database owned by a non-superuser `thiscord`
role. All 21 original tables matched in row count and row-content checksums
before startup migrations, with timestamps normalized to UTC. Database locale
`en_US.UTF-8` was preserved and indexes were rebuilt by the logical restore.
The local development `thiscord_test` database remains local.
The local `thiscord` database has `default_transaction_read_only=on` after cutover
to prevent accidental writes to the old instance. Keep the old backend stopped.
For a deliberate rollback, stop writes on the VPS first and reconcile any newer
data before running `ALTER DATABASE thiscord RESET default_transaction_read_only`
locally; merely switching DNS back after new writes would lose those changes.

The local rollback dump is
`/var/backups/thiscord/vps-migration-20261007/thiscord.dump` inside Kali WSL.
The VPS copy is `/var/backups/thiscord/cutover-20261007.dump`. Both are private
and contain account/session data. Keep them outside Git and public artifacts.

The initial deployed application commit is `e8a141bdbd87`. HTTPS liveness and
database readiness passed from the home network using curl's `--resolve` override
before DNS cutover. The native WebRTC probe on the VPS passed relay-to-relay and
relay-to-public-host tests with ten encrypted RTP packets received each direction.
An external authenticated coturn send-indication probe delivered 20/20 packets
with zero loss. The local older `turnutils_uclient` channel-bind mode returned
400, so that diagnostic alone is not evidence of app-client playback failure;
real desktop voice/screen-sharing acceptance remains necessary.

IPv6 HTTPS and authenticated TURN were also tested locally on the VPS; an
external IPv6 connection could not be established from the migration workstation.
External IPv6 media acceptance remains unverified.

Cloudflare serves the API successfully. DNS-only A/AAAA records for
`turn.thiscord.com.tr` were verified and production `THISCORD_STUN_URL` /
`THISCORD_TURN_URL` now use `stun:turn.thiscord.com.tr:3478` and
`turn:turn.thiscord.com.tr:3478?transport=udp`. After restarting the backend,
the native relay-to-relay and relay-to-host probes passed using this hostname,
with ten encrypted RTP packets received in each direction in each test.
Certbot's simulated renewal passed through Cloudflare on 2026-10-07 using
`certbot renew --dry-run --no-random-sleep-on-renew`. Public HTTPS readiness
also passed without a DNS override. Do not restart the old backend to bridge
DNS propagation: its database
is retained for rollback, not as a second writable production instance.
