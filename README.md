# Thiscord

A personal Discord-style application built with Rust: Leptos/WASM inside Tauri,
an Axum backend running in WSL, and locally hosted PostgreSQL managed by Diesel.
HTML/CSS, configuration, SQL migrations and generated WASM JavaScript glue are
supporting assets. No Node.js or handwritten JavaScript/TypeScript is required.

This minimal template contains a desktop launcher, a UI with a backend check,
`GET /api/v1/health`, and an optional database pool. Chat, login and voice are
planned in [TODO.md](TODO.md). Contributor rules are in [AGENTS.md](AGENTS.md).

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

Install frontend tools on the desktop host:

```sh
cargo install trunk --locked
cargo install tauri-cli --version '^2' --locked
```

## Backend in WSL

For Ubuntu/Debian WSL:

```sh
sudo apt update
sudo apt install build-essential pkg-config libpq-dev postgresql postgresql-contrib
```

Prefer a checkout inside the WSL filesystem for faster builds, or access this
checkout using a separate Linux target directory:

```sh
export CARGO_TARGET_DIR="$HOME/.cache/thiscord-target"
cd /mnt/c/thiscord/backend
cp .env.example .env
cargo run -p thiscord-backend
```

With `DATABASE_URL` commented out, the backend starts without a database.
`curl http://localhost:3000/api/v1/health` returns `{"status":"ok"}`.
This measures process liveness, not database readiness. Ctrl-C or SIGTERM shuts
the backend down gracefully.

Enable the local database:

```sh
sudo service postgresql start
sudo -u postgres createuser --pwprompt thiscord
sudo -u postgres createdb --owner=thiscord thiscord
cargo install diesel_cli --version '~2.3' --no-default-features --features postgres --locked
```

Uncomment `DATABASE_URL` in `backend/.env`, set your chosen password (URL-encode
reserved characters), and restart. A configured but unreachable database fails
startup. PostgreSQL stays private; clients talk only to the API. There are no
application tables yet. Add the first model from `backend/` with:

```sh
diesel migration generate create_users
# Implement up.sql and down.sql before applying.
diesel migration run
diesel print-schema > src/schema.rs
```

## Frontend

Start the WSL backend, then in a native Windows terminal:

```powershell
cd C:\thiscord\frontend
cargo tauri dev
```

On macOS/Linux, also run `cargo tauri dev` from `frontend/`, with access to the
backend. For a browser preview, use `trunk serve` and visit
`http://127.0.0.1:1420`. Select **Check backend** to exercise the shared JSON type.

Windows normally reaches WSL via `localhost:3000`; if forwarding is unavailable,
configure WSL networking and `BACKEND_BIND` deliberately. Remote machines need a
reachable backend address. Set the compile-time UI URL before starting Trunk/Tauri:

```powershell
$env:THISCORD_API_URL = "https://api.thiscord.com.tr"
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

```sh
cargo fmt --all -- --check
cargo check -p thiscord-shared --locked
cargo check -p thiscord-frontend --bin thiscord-ui --target wasm32-unknown-unknown --locked
# In WSL with libpq-dev:
cargo check -p thiscord-backend --locked
# On a desktop host, from frontend/:
trunk build
cargo check -p thiscord-frontend --bin thiscord-desktop --features desktop --locked
```

Use Clippy with the same package/target/features. Do not build all workspace
features for WASM: backend and native launcher are native programs.

Initial verification: formatting and targeted Clippy passed; Trunk built the
WASM assets; the Tauri CLI produced a Windows debug executable with embedded
assets; the backend built in Ubuntu 22.04 WSL and passed an HTTP/CORS health
smoke check and SIGTERM shutdown. No live database connection or macOS/Linux
desktop runtime was verified. The development prerequisites installed for these
checks were Rust 1.93.1 on Windows/WSL and libpq development packages in WSL;
no PostgreSQL database or domain deployment was created.

After upgrading to Rust 1.98.1, formatting and Clippy checks passed for shared,
the WASM UI, the Windows launcher and the WSL backend. Cargo reports an upstream
future-compatibility warning in `proc-macro-error2 2.0.1`; it does not block this
toolchain. The toolchain's rust-analyzer component was also verified.

References: [Leptos CSR](https://book.leptos.dev/getting_started/index.html),
[Tauri + Leptos](https://v2.tauri.app/start/frontend/leptos/),
[Axum](https://docs.rs/axum/latest/axum/),
[Diesel](https://diesel.rs/guides/getting-started).
