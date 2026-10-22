# Thiscord

English and Turkish are available from the language selector on the login screen
and **Settings → Language** (also in the browser admin dashboard). The first launch
uses the first supported system language, falling back to English. The choice is
saved on this device, survives sign-out/restarts, and updates the UI without a
reload. Overlay and notification windows follow the choice through storage events.
If local storage is unavailable, switching still works for the current window.

Translations live in `frontend/locales/tr.json`, keyed by English source text.
The frontend build generates Rust lookups and checked formatting macros, with
English fallback for unknown text. Use reactive `i18n::t` calls for labels and
`localized_format!` for text with arguments; keep placeholders intact and pass
named arguments explicitly. Shared wire codes, user messages/names, credentials,
and machine-readable diagnostics are never translated. Dates display in the
selected locale with an explicit UTC zone. Unrecognized server errors use their
stable wire error code in Turkish and retain the request ID. External provider
pages, account emails/recovery web pages, and native OS/tray text remain English.

The home screen includes friends, online presence, friend requests, private text
conversations and one-to-one desktop voice calls. Use **Add friend** in the top
right to search by part of a display name or username. See [friends and private conversations](docs/friends.md)
for behavior, access rules and current limitations.

**Settings → Notifications** controls sounds, banners, volume and event filters.
Right-click channels or conversations for per-source settings and quick mute;
banners offer snooze and incoming-call actions. Alerts use authenticated server
subscriptions across all accessible chats, with preferences synced between devices.
See [notifications and release announcements](docs/notifications.md).

A browser dashboard at `/admin` provides instance-owner/admin-only CPU/RAM,
backend health, active voice rooms and per-participant network diagnostics.
Click a participant to adjust their connected desktop's audio settings using the
same controls as the desktop app, or download their technical diagnostic logs.
See [admin build, access and deployment](docs/admin.md).
Automatic backend and desktop UI counters support the prioritized
[performance audit](docs/performance-audit.md), without collecting user content.

A personal Discord-style application built with Rust: Leptos/WASM inside Tauri,
an Axum backend running in WSL, and locally hosted PostgreSQL managed by Diesel.
HTML/CSS, configuration, SQL migrations and generated WASM JavaScript glue are
supporting assets. No Node.js or handwritten JavaScript/TypeScript is required.

The app includes account registration/login, recovery and verification, profile and
device management, OS-backed desktop sessions, and Google OIDC integration. Local
emails default to ignored `backend/.mail/`; Resend, SMTP and Google use provider
credentials configured in `backend/.env` (see `docs/accounts.md`).
Everyone can upload or remove a profile picture in **Settings > Profile**. Pictures
appear in voice chat and the desktop overlay, with initials when no picture is set.
The bottom account ribbon spans the server rail and channel list, showing your
profile, authenticated socket status, mute/deafen and settings controls. It expands
during calls with the channel/server name, camera, screen sharing and disconnect controls.
Sidebar controls show immediate hover/focus hints
outside the scroll area. Visible voice channels show participants before joining;
clicking a voice channel joins directly while keeping the current text chat open.
Clicking that channel again opens its voice room without reconnecting.
Hover **Voice connected** for brief latency, health and drop diagnostics; click
the channel name in the ribbon to open call controls. Server/channel creation,
server joining and screen-picker dialogs close when clicking their backdrop.

Settings open in a focused modal over the current conversation. Account/app settings
and server settings have separate navigation. Profile, roles and channel permissions
have explicit save/reset controls; closing or switching sections confirms unsaved
changes. Audio changes save automatically, with diagnostics under **Advanced & troubleshooting**.
Use the account ribbon’s gear for app settings or the server-header gear for server
settings. Escape and outside clicks close the modal; keyboard focus stays inside it.

Guild memberships, role hierarchy, channel overrides and the role editor are
implemented. Right-click servers, channels or voice participants for relevant
actions; the browser context menu is suppressed. Server settings sit beside the
server name, and moderation is available through context menus. Channel pins and
notification mutes are saved per account on this device; muted channels hide unread
and mention badges without marking messages as read or changing voice playback.
Participant voice settings adjust local playback volume (0% mutes that person).
Each server has a **Moderation** panel for voice disconnects,
timeouts, kicks, bans and unbans, with server-side permission and hierarchy checks.
After verifying an account, run `./run-wsl.ps1 -BootstrapOwner YOUR_USERNAME`
from `backend/`, then open your server’s **Server settings > Roles**. See [permission rules and setup](docs/permissions.md).
Text chat includes persisted history, edits/deletes, unread mentions and authenticated
live sockets. Links in guild and private messages are clickable and open in the
default browser on desktop. Drop images, videos or files onto a text channel, or use the attachment
button, then send them with an optional message. Images and videos open in a
full-window viewer; videos have a play overlay and playback controls. Files and the
download buttons save attachments. Each message supports four files of up to 25 MiB
each. See [chat protocol and limits](docs/chat.md). Desktop voice channels
use native Rust audio, independent speaker volume controls and a Rust WebRTC SFU.
Open **Settings > Voice & audio** to select devices and test audio, then join a
voice channel. See [audio setup, networking and remaining checks](docs/audio.md).
Missing saved audio devices immediately fall back to the system default for that
direction on join or recovery, preserving the other selection and saved preferences.
Other device failures retry selected devices three times, then system defaults
three times; the voice bar reports recovery and the devices actually opened.
Windows can list possible conflicting audio-session processes after retries fail.
Desktop clients have an opt-in **Settings > App behavior > Keep Thiscord running when
I close the window** preference, saved on this computer. Closing hides the window
while calls and screen sharing continue. Use the Thiscord system tray icon
(Windows/Linux) or menu-bar icon (macOS) to **Open Thiscord** or **Quit Thiscord**.
Launching Thiscord again restores the running client, including when minimized or
hidden in the tray, instead of opening a second instance.
On macOS, clicking the Dock icon also restores the window. Linux minimizes to the
taskbar as well, keeping a recovery path on desktops without a tray host. Tray creation failures
disable the preference for that run. With the setting off, closing exits the app.
The voice bar shows recent quality observations; Voice & audio has advanced
[voice-quality diagnostics](docs/audio-quality.md) for local buffering, timing,
clipping and packet handling. Metrics work without logging or recording audio.

Optional rotating audio diagnostics are off by default. Turn on **Settings > Voice & audio > Advanced & troubleshooting > Enable diagnostic logging** before reproducing a failure, then use **Open
diagnostic logs** to share the `audio-diagnostics*.jsonl` files. The preference
persists on this computer; disabling logging keeps existing files. Logs contain
technical events, not voice recordings or credentials, and are never auto-uploaded.
Voice failures include safe transport error categories, audio processing stages and
request IDs for correlation with backend logs; see [disconnect diagnostics](docs/audio.md#native-voice-connection-diagnostics).
If an older voice client triggers a slot compatibility transition, updated desktops
automatically reconnect after a brief interruption instead of requiring a manual join.
Desktop calls and voice rooms show simultaneous webcams and screen shares in a
grid, with local previews, click-to-fullscreen and right-click hide/restore.
The camera button uses the system default webcam (up to 720p/30); right-click a
participant for separate voice and screen-audio volume/mute controls. Camera
publishing requires WebView H.264/WebRTC and device permissions. Windows screen
sharing retains its explicit source/quality picker, D3D11 hardware encoder and
system audio capture. See [cameras and hardware-free tests](docs/cameras.md) and
[screen sharing requirements](docs/screen-sharing.md) for limits and platform checks.
For echo troubleshooting, Voice & audio can save a 60-second local debug recording
of playback, raw microphone and outgoing audio, with metadata for offline DSP replay.
Voice activation uses sustained energy before automatic gain, hysteresis and a
60 ms pre-roll delay to retain short quiet onsets. Push-to-talk bypasses the
automatic gate and its delay. See [activation behavior and limits](docs/voice-activation.md).
Quiet microphones can use the saved Microphone boost control (1×–10×, default 1×)
in Audio & voice to amplify audio before activation and automatic gain.
Experimental DeepFilterNet3 noise suppression is available in Voice & audio;
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

Desktop voice includes a participant overlay with green speaking rings while
Thiscord is out of focus. Hover your own entry for mute/deafen controls; other
areas pass clicks through. Voice & audio also has customizable global mute/deafen
shortcuts (Ctrl+Shift+M / Ctrl+Shift+D by default). Use windowed or
borderless games. See [overlay behavior and platform limits](docs/audio.md#in-game-voice-overlay).

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
macOS, and WebKitGTK/GTK development libraries on Linux. Native attachment downloads
require macOS 11.3 or newer. Build desktop releases
on their respective OSes. This template targets all three; runtime verification,
installer packaging, icons and signing remain roadmap work.
Native audio also needs CMake and a C compiler for bundled libopus; Linux needs
`libasound2-dev` and `pkg-config`. macOS uses CoreAudio and Windows uses WASAPI.
The native path accepts 8-384 kHz PCM devices with 1-32 channels and converts
capture/playback to the internal 48 kHz pipeline. Hardware acceptance,
macOS runtime and public STUN/TURN deployment remain open; see docs/audio.md.

Install frontend tools on the desktop host:

```sh
cargo install trunk --locked
cargo install tauri-cli --version 2.12.0 --locked
```

## Backend in WSL

Production hosting is documented in [VPS deployment and backups](docs/vps.md).
After the VPS cutover, the local `thiscord` database is a read-only rollback copy;
`thiscord_test` remains available for local integration tests.

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
the Leptos window. Both editors, Trunk, Tauri and terminal Cargo builds share the
intermediate cache configured in `.cargo/config.toml`: `${CARGO_HOME}/thiscord`
(normally `~/.cargo/thiscord`). Final executables stay in each checkout's `target`
directory. This prevents a full compiler cache per T3 worktree or editor window.
Cargo serializes builds using that cache. See [build cache configuration](docs/ci.md)
for fixed parallel slots, CI isolation, storage budgets and preview-first cleanup.
Development/tests keep line-number backtraces and incremental compilation; CI
disables incremental state and reports build storage after each job.

Native checks require Tauri platform dependencies and PostgreSQL client development
libraries on the host running rust-analyzer. The backend still runs in WSL as
documented above. When opening the native workspace in WSL, install Linux Tauri
dependencies as well, since that workspace checks the launcher too. If using
both Windows and WSL, use separate Cargo homes and set WSL's `CARGO_TARGET_DIR`
to a Linux-only path for final outputs, as shown above. The intermediate cache
automatically follows the host's Cargo home; no editor target-directory override
is needed.

After switching workspaces, run **rust-analyzer: Restart server** if diagnostics
from the previous target remain. The original `thiscord.code-workspace` is
replaced by these two files.

### Command-line checks

For interactive voice testing without other people, the instance owner can
right-click a voice channel and choose **Voice test bots…**. Add multiple bots
with distinct microphone loops and good, low-bandwidth, lossy or unstable media
profiles; edit the normal voice settings by clicking them in `/admin`. Bots run
the desktop audio engine, including RED/DRED, noise/echo processing and diagnostics,
with twelve English/Turkish speech loops (quiet room, fan, keyboard, café, traffic
and rain), repeatable noise/echo scenarios and virtual PTT. Backend `run.sh` builds
the headless companion automatically. Bots also watch
cameras and screen shares (including shared audio), decode video, and report
receiver feedback and counters.
See [voice test bots](docs/voice-bots.md) for permissions and simulation limits.

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
