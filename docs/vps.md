# VPS backend deployment

The backend is deployed through `ssh vps` to Ubuntu 26.04 at `57.129.178.14`.
The public origin remains `https://thiscord.com.tr`; desktop clients and Google
OAuth callback URLs do not need changing. Production DNS uses the VPS IPv4 A
record. Remove the old home-network AAAA record; this deployment initially serves
HTTP and TURN over IPv4. PostgreSQL 18 listens only on loopback.

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
are in `infra/vps/compose.yml`. The relay address and backend voice bind address
are the VPS's public IPv4. Private/loopback relay peers remain denied.

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

Certbot uses standalone HTTP validation on port 80. DNS must reach this VPS.
Its deploy hook `/etc/letsencrypt/renewal-hooks/deploy/thiscord` installs the new
certificate pair with restricted permissions and sends SIGHUP to the backend.
This reloads TLS without disconnecting active sessions. The source hook is
`infra/vps/renew-certificate.sh`.

## Updating the backend

Build the reviewed commit with the repository's pinned Rust toolchain. This
4 GiB VPS uses two build jobs and disables release LTO to limit build memory:

```sh
cd /home/ubuntu/thiscord
export PATH="$HOME/.cargo/bin:$PATH"
CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8 \
  cargo build -p thiscord-backend --bin thiscord-backend --release --locked
```

Take a database backup before deploying, install the binary into a new immutable
release directory, atomically replace `/opt/thiscord/current`, and restart
`thiscord.service` during a planned interruption. Verify liveness and readiness.
Database migrations can make a binary-only rollback unsafe; use the reviewed
migration rollback behavior or restore a pre-update backup to a separate database.

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

Until DNS points to the VPS and the old AAAA record is removed, domain-based
client access and Certbot renewal validation still target the old installation.
Verify `certbot renew --dry-run` after propagation before treating renewal as
tested. Do not restart the old backend to bridge DNS propagation: its database
is retained for rollback, not as a second writable production instance.
