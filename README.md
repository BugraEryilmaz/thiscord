# Thiscord

A personal Discord-style application built with Rust: Leptos/WASM inside Tauri,
an Axum backend running in WSL, and locally hosted PostgreSQL managed by Diesel.
HTML/CSS, configuration, SQL migrations and generated WASM JavaScript glue are
supporting assets. No Node.js or handwritten JavaScript/TypeScript is required.

The app includes account registration/login, recovery and verification, profile and
device management, OS-backed desktop sessions, and Google OIDC integration. Local
emails default to ignored `backend/.mail/`; Resend, SMTP and Google use provider
credentials configured in `backend/.env` (see `docs/accounts.md`).

Guild memberships, role hierarchy, channel overrides and the role editor are
implemented. After verifying an account, run `./run-wsl.ps1 -BootstrapOwner YOUR_USERNAME`
from `backend/`, then open **Guilds & roles**. See [permission rules and setup](docs/permissions.md).
Text chat includes persisted history, edits/deletes, unread mentions and authenticated
live sockets. See [chat protocol and limits](docs/chat.md). Desktop voice channels
use native Rust audio, independent speaker volume controls and a Rust WebRTC SFU.
Open **Settings > Audio & voice** to select devices and test audio, then join a
voice channel. See [audio setup, networking and remaining checks](docs/audio.md).
For echo troubleshooting, Audio & voice can save a 60-second local debug recording
of playback, raw microphone and outgoing audio, with metadata for offline DSP replay.
Experimental DeepFilterNet3 noise suppression is available in Audio & voice;
see [model selection, regression tests and benchmarks](docs/deep-filter.md).
Experimental neural residual echo estimation is also available, independently
of denoising. Its model is bundled with native clients; enable echo cancellation
and the neural estimator to try it. See [setup and measured results](docs/voice-isolation.md#neural-residual-echo-estimator-extension-2026-10-02).
The main screen lists joined servers in a left rail. Use **+** (verified instance
owner/admin) to create a server with an optional password, or **Join** with a server
ID and password. Server selection shows its ID and permission-filtered channels.
See [account setup and security](docs/accounts.md). Liveness/readiness, request IDs,
shared API contracts and Diesel migrations underpin the backend. Remaining media
and chat work is tracked in [TODO.md](TODO.md). Contributor rules are in [AGENTS.md](AGENTS.md).

## Layout

Exactly three Cargo packages share a root lockfile:

| Directory | Responsibility |
| --- | --- |
| `frontend/` | Leptos `thiscord-ui` WASM binary and native `thiscord-desktop` Tauri binary |
| `backend/` | Axum API, Diesel pool, schema and migrations; run in WSL |
| `shared/` | Portable Serde communication contracts used by both sides |

The root defaults to `shared` so plain `cargo check` needs neither desktop system
libraries nor PostgreSQL headers. Select other packages explicitly.

## Prerequisites

Install [Rust](https://rustup.rs/) separately on Windows and inside WSL.
`rust-toolchain.toml` pins Rust 1.98.1 (the latest stable release at this update),
rustfmt, Clippy, rust-analyzer and the WASM target. The exact pin keeps desktop
and WSL builds reproducible; update it when adopting a newer stable release.
Keep Windows and Linux build artifacts separate.
The workspace Cargo configuration increases compiler stack size for Leptos builds
on Windows. If Trunk rejects an inherited `NO_COLOR=1`, unset it or use
`$env:NO_COLOR = "true"` in PowerShell.

Install [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/)
on each desktop host: MSVC C++ tools and WebView2 on Windows, Xcode tools on
macOS, and WebKitGTK/GTK development libraries on Linux. Build desktop releases
on their respective OSes. This template targets all three; runtime verification,
installer packaging, icons and signing remain roadmap work.
Native audio also needs CMake and a C compiler for bundled libopus; Linux needs
`libasound2-dev` and `pkg-config`. macOS uses CoreAudio and Windows uses WASAPI.
The initial native path requires 48 kHz mono/stereo devices. Hardware acceptance,
macOS runtime and public STUN/TURN deployment remain open; see docs/audio.md.

Install frontend tools on the desktop host:

```sh
cargo install trunk --locked
cargo install tauri-cli --version 2.12.0 --locked
```

## Backend in WSL

For direct HTTPS with the existing Let's Encrypt certificate, see
[HTTPS setup and certificate renewal](docs/https.md).

The existing local PostgreSQL installation is **PostgreSQL 17 in `kali-linux`**,
cluster `17/main`, listening on `localhost:5432`. The application database and
role are both named `thiscord`; the separate test database is `thiscord_test`.
The role is not a superuser and cannot create roles or databases.

Credentials have been generated and saved only in ignored `backend/.env`.
Do not overwrite it with the example file. PostgreSQL is configured to start
with systemd in that WSL distribution; starting WSL is still required.

Start directly from PowerShell (the launcher uses Kali's Rust installation and
a separate Linux build directory):

```powershell
cd C:\thiscord\backend
.\run-wsl.ps1
# Apply migrations and exit:
.\run-wsl.ps1 -MigrateOnly
```

Or use **Tasks: Run Task > Backend: run in WSL** in the native VS Code workspace.
Plain `cargo run` in a Windows terminal launches a Windows executable; it does
not automatically run the backend in WSL.

To work interactively in WSL, from PowerShell:

```powershell
wsl -d kali-linux
```

Then in WSL:

```sh
source ~/.cargo/env
pg_lsclusters
# If the cluster is stopped:
sudo pg_ctlcluster 17 main start
export CARGO_TARGET_DIR="$HOME/.cache/thiscord-target"
cd /mnt/c/thiscord/backend
cargo run -p thiscord-backend --locked
```

Startup connects to PostgreSQL and applies pending embedded migrations. A
configured but unreachable database or failed migration prevents startup.
Ctrl-C or SIGTERM shuts down gracefully. To apply migrations without serving:

```sh
cargo run -p thiscord-backend --locked -- --migrate-only
```

```sh
curl -i http://localhost:3000/api/v1/health
curl -i http://localhost:3000/api/v1/ready
```

Liveness returns `{"status":"ok"}` even without a database. Readiness queries
the `instance` table and returns HTTP 200 with a stable installation ID and UTC
check timestamp, or HTTP 503 with a safe API error. Every response includes a
server-generated `x-request-id`, also included in errors and request logs.
The account UI connects to this same backend.

The legacy `public` and `tower_sessions` schemas were reset after a full backup.
The backup is retained in Kali at
`/var/backups/thiscord/legacy-20260927T105457Z.dump` (root-only). It contains the
previous tables/data, including 523 user rows and 7 sessions. No other database
was reset. See [database operations](docs/database.md) for inspection, restore,
new-machine setup and test instructions.

The first migration creates one `instance` record with a UUID and creation
timestamp; the second adds accounts, identities, sessions, email and OAuth state.
The third adds instance ownership, guild membership, roles, channels and overrides.
The fourth adds optional Argon2id-protected server joining.
The fifth adds messages, durable chat events, read markers and expiring presence.
Add subsequent models
with Diesel, from `backend/`:

```sh
diesel migration generate create_users
# Implement up.sql and down.sql before applying.
diesel migration run
diesel print-schema > src/schema.rs
```

The installed/CI Diesel CLI version is 2.3.13. Its `diesel.toml` configuration
updates `schema.rs` after migrations. Keep `up.sql`, `down.sql` and generated
schema changes together. Rollbacks are destructive and are not run on startup.

## Frontend

Start the WSL backend, then in a native Windows terminal:

```powershell
cd C:\thiscord\frontend
cargo tauri dev
```

On macOS/Linux, also run `cargo tauri dev` from `frontend/`, with access to the
backend. For a browser preview, use `trunk serve` and visit
`http://127.0.0.1:1420`. Browser sessions stay in memory; desktop sessions use the
OS credential store. Restart an existing backend and desktop process after updating.

Windows normally reaches WSL via `localhost:3000`; if forwarding is unavailable,
configure WSL networking and `BACKEND_BIND` deliberately. Remote machines need a
reachable backend address. Set the compile-time UI URL before starting Trunk/Tauri:

```powershell
$env:THISCORD_API_URL = "https://thiscord.com.tr"
cargo tauri dev
```

On macOS/Linux: `THISCORD_API_URL=https://api.thiscord.com.tr cargo tauri dev`.
This is a planned hostname, not a deployed API. Configure DNS, TLS, routing and
firewall access first. Align backend `ALLOWED_ORIGINS` and Tauri `connect-src`
with the endpoints in use. Development CSP currently allows only the local API;
add the remote origin there for remote development.

Run `cargo tauri build --no-bundle` in `frontend/` for a native release executable.
Installer bundling is disabled until metadata/icons and signing are configured.
The Tauri manifest lives directly in `frontend/`, avoiding a fourth `src-tauri`
package. Always run the Tauri CLI from that directory.

### Styling with Tailwind CSS

Tailwind CSS 4.3.3 is pinned in `frontend/Trunk.toml`. Trunk downloads the
standalone CLI on the first build, compiles `frontend/style.css`, and links the
generated stylesheet into the app. No Node.js/npm installation, extra terminal,
or JavaScript configuration is needed. The first build needs network access to
download the CLI; subsequent builds can reuse Trunk's tool cache.

Use the usual `trunk serve`, `trunk build`, or `cargo tauri dev` commands from
`frontend/`. Trunk rebuilds the stylesheet when frontend sources change;
`trunk build --release` also minifies the CSS. Tauri's existing build hook runs
that release command and embeds the generated assets.

Write utility classes directly in Leptos `view!` markup. `style.css` explicitly
scans `src/**/*.rs` and `index.html`; add `@source` entries there if components
move elsewhere. Keep conditional classes as complete strings (for example,
`"text-red-500"`), rather than assembling fragments with `format!`.
Shared color tokens live in its `@theme` block and produce utilities such as
`bg-brand`, `bg-surface`, and `text-content`.

See [Tailwind source detection](https://tailwindcss.com/docs/detecting-classes-in-source-files)
and [Trunk's asset pipeline](https://github.com/trunk-rs/trunk/blob/v0.21.14/guide/src/assets/index.md).

## Validation

### VS Code workspaces

Open these files in separate VS Code windows:

```sh
code --new-window thiscord-leptos.code-workspace
code --new-window thiscord-native.code-workspace
```

- **Leptos** analyzes `frontend/src/app.rs` and the shared contracts for
  `wasm32-unknown-unknown`. Its explicit Cargo commands build only `thiscord-ui`
  and its dependencies, including the procedural macros needed for `view!`.
- **Native** analyzes the backend, shared crate and Tauri launcher on the host
  target, with the `desktop` feature enabled. Edit `frontend/src/desktop.rs` here.

Both show the repository root so workspace manifests and documentation remain
accessible. Their analysis targets are intentionally different; edit the UI in
the Leptos window. Build output is separated under `target/rust-analyzer-leptos`
and `target/rust-analyzer-native`. These editor settings do not change Trunk,
Tauri or normal terminal Cargo commands.

Native checks require Tauri platform dependencies and PostgreSQL client development
libraries on the host running rust-analyzer. The backend still runs in WSL as
documented above. When opening the native workspace in WSL, install Linux Tauri
dependencies as well, since that workspace checks the launcher too. If using
both Windows and WSL, use separate checkouts or set WSL's `CARGO_TARGET_DIR` and
adjust the native workspace's target-directory settings to a Linux-only path.

After switching workspaces, run **rust-analyzer: Restart server** if diagnostics
from the previous target remain. The original `thiscord.code-workspace` is
replaced by these two files.

### Command-line checks

For desktop/Pi voice capacity measurements from another machine, use the
[Rust voice load generator](docs/voice-load.md). It includes a disposable test
server and Mac instructions; measured capacity on real hardware remains pending.

```sh
cargo fmt --all -- --check
cargo test -p thiscord-shared --locked
cargo check -p thiscord-frontend --bin thiscord-ui --target wasm32-unknown-unknown --locked
# In WSL with libpq-dev:
cargo clippy -p thiscord-shared -p thiscord-backend --all-targets --locked -- -D warnings
cargo test -p thiscord-shared -p thiscord-backend --locked -- --include-ignored
# On a desktop host, from frontend/:
trunk build
cargo check -p thiscord-frontend --bin thiscord-desktop --features desktop --locked
```

Use Clippy with the same package/target/features. Do not build all workspace
features for WASM: backend and native launcher are native programs.

PostgreSQL integration tests load `TEST_DATABASE_URL` from the environment or
`backend/.env`, require a database name ending in `_test`, and create/drop only
a uniquely named test schema. They are ignored in a normal `cargo test`; use
`--include-ignored` to require them. Missing database configuration then fails
the test rather than silently skipping it.

[CI](.github/workflows/ci.yml) runs formatting, shared/backend Clippy and tests
against PostgreSQL 17, a generated-schema check, and a release WASM/Tailwind
build. A Windows/macOS/Linux matrix lints and builds native executables with
those web assets embedded, then uploads them as artifacts. Independent native audio
jobs run on all three platforms. CI runs on pull requests, pushes to `main`, and
manual dispatch; feature-branch pushes do not duplicate PR runs. Tagged client
releases build Windows x64, Linux x64 and macOS Apple Silicon installers alongside
checks once the WASM assets are ready. Publication waits for every check and build.
See [CI caching and runner setup](docs/ci.md). Clients check automatically
but install/restart only on request. See [release setup and signing secrets](docs/releases.md)
before pushing the first `client-vMAJOR.MINOR.PATCH` tag. GUI/installer smoke tests
and macOS runtime verification require their respective hosts; services are not deployed.

Shared transport conventions are described in [docs/api.md](docs/api.md).
Cargo currently reports an upstream future-compatibility warning in
`proc-macro-error2 2.0.1`; it does not block the pinned toolchain.

References: [Leptos CSR](https://book.leptos.dev/getting_started/index.html),
[Tauri + Leptos](https://v2.tauri.app/start/frontend/leptos/),
[Axum](https://docs.rs/axum/latest/axum/),
[Diesel](https://diesel.rs/guides/getting-started).
