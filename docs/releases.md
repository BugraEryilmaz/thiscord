# Desktop releases and updates

The native client checks the public GitHub update feed three seconds after launch
and every six hours. It shows an optional update prompt and an **Install and
restart** button. Nothing downloads or installs until that button is clicked.
**Settings → App updates** also provides a manual check. Installation is blocked
while connected to voice; joining voice is blocked during installation/download.
The login screen has an Updates button too. Development builds disable updates.

Updates use Tauri's native Rust updater and an embedded Ed25519 public key.
Downloads require HTTPS, the expected repository/tag, a valid artifact signature
and a signed version matching the manifest. Older/equal versions are ignored.
Downloads have a 15-minute timeout and 512 MiB limit. Failures leave the current
installation usable and can be retried. Account tokens never go to GitHub.

## One-time setup

1. Back up the **encrypted updater private key and its password** securely. The
   key generated for this checkout is outside Git at
   `C:\Users\bugra\.thiscord\signing\updater.key`; its password is in
   `updater-password.txt` alongside it. Both are restricted to this Windows user
   and SYSTEM. This is a separate key from the backend's TLS certificate.
2. In [repository Actions secrets](https://github.com/BugraEryilmaz/thiscord/settings/secrets/actions),
   add `TAURI_SIGNING_PRIVATE_KEY` with the **contents** of `updater.key`, and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` with the password file's contents. Do not
   put a local file path in GitHub's secret value. Never commit these files.
3. Ensure Actions can run with the release job's `contents: write` permission.
   This public repository serves installers and `latest.json` directly; clients
   do not need GitHub credentials. Making it private requires a different public
   update distribution endpoint.
   Reserve GitHub's Latest release for stable clients; future backend-only releases
   must not replace that marker.

If setting up another checkout, obtain the existing signing key from your backup.
Do not generate a different key: already installed clients trust the public key
embedded in their binary. Key rotation needs an update signed with the old key
that distributes the new trust configuration first.

## Publish a version

1. Update `[workspace.package].version` in root `Cargo.toml` and `version` in
   `frontend/tauri.conf.json` to the same stable `MAJOR.MINOR.PATCH`, then refresh
   `Cargo.lock` with `cargo check -p thiscord-shared`.
2. Commit and push the changes. Tag that commit and push the tag, for example:

   ```powershell
   git tag client-v0.1.0
   git push origin client-v0.1.0
   ```

   Use the actual version; published tags/releases are immutable. A subsequent
   version must be higher than every previously published stable client version.
3. Watch [Client release](../.github/workflows/release-client.yml). It validates
   versions/secrets, runs the existing format, Clippy, database, shared, WASM and
   desktop checks, and then builds four installer targets. Production WASM assets
   compile with `THISCORD_API_URL=https://thiscord.com.tr`.
4. The final job downloads every platform artifact and independently verifies
   each updater signature against the embedded public key and signed version.
   It creates a draft release, uploads all installers, signatures, SHA256SUMS and
   the complete four-platform `latest.json`, then publishes and marks it latest.
   A failed build never advertises a partial update. A failed upload leaves a draft;
   rerun failed jobs to complete it. Concurrent release runs are serialized.

The Rust toolchain, Trunk/Tailwind versions and Cargo.lock remain pinned. Packaging
uses Tauri CLI **2.12.0**; older CLIs may omit the signed version required by these
clients. There is no Node/npm build step and no fourth Rust package.

## Installers and platform trust

| Platform | Initial installation | In-app update artifact |
| --- | --- | --- |
| Windows x64 | NSIS `.exe`, per-user install | Same signed `.exe` |
| macOS Apple Silicon | `.dmg` | `.app.tar.gz` |
| macOS Intel | `.dmg` | `.app.tar.gz` |
| Linux x64 | `.AppImage` or `.deb` | `.AppImage` only |

An older executable without updater support must be replaced once by installing
the first updater-enabled release manually. Keep Linux AppImages in a user-writable
location. Debian packages require manual/package-manager updates; no apt repository
is configured. Linux builds use Ubuntu 24.04 and require compatible glibc/system
libraries; an AppImage is not a promise of compatibility with every distribution.

Updater signatures authenticate in-app updates. They do **not** replace Windows
Authenticode or Apple Developer ID/notarization for the first download. Windows
installers currently have no Authenticode certificate and may show SmartScreen
warnings. macOS builds are ad-hoc signed unless Apple signing secrets are provided;
Gatekeeper may block downloaded applications. For trusted macOS distribution add:

- `APPLE_CERTIFICATE`: base64-encoded Developer ID Application `.p12` certificate.
- `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY`.
- `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), `APPLE_TEAM_ID` for notarization.

The macOS entitlement enables microphone access under the hardened runtime;
`Info.plist` supplies the microphone usage prompt. Certificate purchase, Windows
signing configuration and Apple account setup remain deployment work.

## Local checks

```powershell
cargo install tauri-cli --version 2.12.0 --locked
./scripts/release-client.ps1 -Mode Validate -Tag client-v0.1.0
```

For local bundles, set `TAURI_SIGNING_PRIVATE_KEY` to the key file path and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` by reading the password file into the process
environment (never print either). From `frontend/`, run `cargo tauri build --ci
--target x86_64-pc-windows-msvc -- --locked` with `THISCORD_API_URL` set to the
production URL. Clear the signing environment afterward. On other OSes select
their matching Rust target; build on the matching native host.

`scripts/release-client.ps1 -Mode Collect` verifies/copies bundles for one target;
`-Mode Assemble` verifies a complete downloaded matrix and creates a local feed
without publishing. `-Mode Publish` additionally writes to GitHub and is intended
for the release workflow. The verifier is the frontend's `verify_update` Rust
example, enabled by `release-tools`; it does not add another workspace package.

Acceptance before broad distribution: run the workflow, manually install each
OS's first release, publish a higher test version, then verify Later, offline retry,
voice protection, signature failure and install/restart on each OS. CI compilation
does not establish native installer/update runtime compatibility.

References: [Tauri updater](https://v2.tauri.app/plugin/updater/),
[Tauri signing/distribution](https://v2.tauri.app/distribute/pipelines/github/).
