# Local PostgreSQL

## Configured development machine

- WSL distribution: `kali-linux`.
- PostgreSQL: 17, cluster `main`, port 5432, loopback-only TCP.
- App database/owner: `thiscord`.
- Test database/owner: `thiscord_test` / `thiscord`.
- Credentials: ignored `backend/.env`, with `DATABASE_URL` and `TEST_DATABASE_URL`.
- The role has login access but no superuser, role-creation or database-creation privileges.
- Cluster startup is `auto`, with systemd inside WSL. PostgreSQL does not start
  before the WSL distribution itself starts.

From PowerShell, inspect the service without revealing credentials:

```powershell
wsl -d kali-linux -- pg_lsclusters
wsl -d kali-linux -u postgres -- psql -d thiscord
```

In that `psql` session, `\dt`, `\du` and `SELECT * FROM instance;` show the
application schema, roles and installation record. Type `\q` to exit.

The database pool is synchronous Diesel/r2d2. HTTP queries and startup migration
work run inside `spawn_blocking`. A configured but inaccessible database fails
startup; an unset URL allows liveness-only mode with readiness 503.

## Windows startup troubleshooting

Run `.\run-wsl.ps1` from `backend/` in PowerShell, or select **Backend: run in WSL**
in the native workspace's task menu. Use `-MigrateOnly` to verify connectivity and
migrations without starting HTTP. The launcher defaults to `kali-linux`; its
`-Distribution` parameter can select another configured distribution.

Plain Windows `cargo run` builds a Windows executable. On this development
machine, a pre-existing `netsh interface portproxy` rule forwards Windows port
5432 to WSL's network address, while PostgreSQL listens only on WSL loopback.
That route cannot reach PostgreSQL. Running the backend in the same WSL
distribution uses the correct localhost and requires no database port forwarding.
Local development uses HTTP port 3000. The direct HTTPS deployment uses port 443;
see [HTTPS setup](https.md).

`BACKEND_BIND` is a numeric local IP address and port, normally `127.0.0.1:3000`.
Do not set it to a domain or URL such as `thiscord.com.tr:80`; domains belong in
DNS/public URL configuration. TLS can terminate directly in the backend or at a
reverse proxy. Invalid bind values fail before database startup.

Check PostgreSQL inside WSL with `wsl -d kali-linux -- pg_lsclusters`. If stopped,
run `sudo pg_ctlcluster 17 main start` inside Kali. Existing process environment
variables override `.env`; check for a stale `DATABASE_URL` override if WSL startup
also fails. Do not paste connection URLs into logs or issue reports.

## Legacy backup

Before resetting the legacy application schema, a complete custom-format dump
was saved at `/var/backups/thiscord/legacy-20260927T105457Z.dump` inside Kali.
The directory is mode 700 and the dump is mode 600, owned by root. It includes
the previous `public` and `tower_sessions` schemas, including 523 user records
and 7 sessions. These rows are not imported into the new foundation schema.

Inspect or recover into a **separate** database from inside Kali:

```sh
sudo pg_restore --list /var/backups/thiscord/legacy-20260927T105457Z.dump
sudo -u postgres createdb thiscord_legacy_restore
sudo cat /var/backups/thiscord/legacy-20260927T105457Z.dump \
  | sudo -u postgres pg_restore --exit-on-error --no-owner --no-privileges --dbname=thiscord_legacy_restore
```

Do not restore over the active application database without deliberately planning
that replacement. The dump may contain private data; do not commit or publish it.

## Fresh development setup

On a fresh Debian/Ubuntu-based WSL distribution, install Rust and these packages:

```sh
sudo apt update
sudo apt install build-essential cmake pkg-config libpq-dev postgresql postgresql-contrib
sudo service postgresql start
sudo -u postgres createuser --pwprompt thiscord
sudo -u postgres createdb --owner=thiscord thiscord
sudo -u postgres createdb --owner=thiscord thiscord_test
```

Copy `backend/.env.example` to `backend/.env` only if no local configuration
exists. Set both URLs, URL-encoding reserved characters in passwords. Keep
`connect_timeout=3`. Use a separate Linux `CARGO_TARGET_DIR` when sharing files
with Windows. Run from `backend/` so `.env` is loaded automatically:

```sh
cargo run -p thiscord-backend --locked -- --migrate-only
cargo run -p thiscord-backend --locked
```

## Migrations

Migrations are embedded and applied at startup; `backend/build.rs` watches their
directory. The first creates a constrained singleton `instance` with a UUID and
UTC creation time. Reverting it deletes the installation identity; reapplying
generates a new identity. Future account data is not part of this migration.

Install the CLI with:

```sh
cargo install diesel_cli --version 2.3.13 --no-default-features --features postgres --locked
```

Run `diesel migration list`, `diesel migration generate NAME`, `diesel migration run`
and `diesel print-schema` from `backend/`. Write both migration directions before
applying. Never edit an already-deployed migration; add another one. Startup
migrations share the pool's bounded statement/lock timeouts; review them before
introducing longer-running data migrations.

## Integration tests

```sh
cargo test -p thiscord-shared -p thiscord-backend --locked -- --include-ignored
```

The PostgreSQL test requires `TEST_DATABASE_URL` (environment takes precedence
over `backend/.env`) and rejects database names without `_test` suffix. It creates
a uniquely named schema, sets that connection's search path, applies/reverts/
reapplies migrations, and drops only that schema on completion or panic. It tests
singleton constraints, persistent identity, HTTP readiness, pool exhaustion,
missing schema and recovery. Liveness remains successful in the failure cases.

If a test process is killed abruptly, a `foundation_test_*` schema can remain in
the test database. Inspect and remove that specific schema when no tests are
running; do not reset the application database to clean up tests.
