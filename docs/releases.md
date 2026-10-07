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

## Runner selection

CI and release workflows call `select-runners.yml` on GitHub-hosted Ubuntu before
scheduling build jobs. An online runner with matching `self-hosted`, OS and
architecture labels is preferred (Windows x64, Linux x64, macOS ARM64). Otherwise
that platform uses `windows-latest`, `ubuntu-24.04` or `macos-latest`. Selection is
reported in the routing job summary. Busy online runners remain eligible; jobs
queue behind their existing work instead of falling back to hosted machines.
Installer jobs reuse CI's initial selection.
Linux installers additionally require the `thiscord-ubuntu-24.04` label on a real
Ubuntu 24.04 x64 runner; otherwise they use GitHub-hosted Ubuntu. The packaging job
verifies `/etc/os-release` before installing dependencies. Do not apply that label
to Kali: general Linux checks may run there, but released binaries must preserve
the Ubuntu system-library baseline. The existing `ubuntu-24.04` label alone does
not establish installer eligibility.
This is a snapshot, not a reservation: jobs can queue behind each other, and a
runner going offline after selection does not trigger another fallback check.

Add repository Actions secret `RUNNER_STATUS_TOKEN`: a fine-grained PAT scoped
only to this repository with **Administration: read-only**. GitHub requires this
permission for the runner-list API; the normal `GITHUB_TOKEN` is insufficient.
Missing/expired tokens and API failures emit a warning and select hosted runners.
Do not copy a broadly scoped personal CLI credential into this secret. Pull request
jobs always use hosted runners, and their router never receives the PAT.

Self-hosted machines must already have Rust/rustup, PowerShell 7, Git and the
platform build prerequisites available to the runner account. Linux also needs
Python 3, passwordless package installation and a running Docker daemon accessible
to that account for the PostgreSQL service container. CI uses a dynamically
assigned database port to avoid the locally hosted development PostgreSQL port.
The router checks availability and labels, not installed software or GPU readiness.

## Publish a version

1. Update `[workspace.package].version` in root `Cargo.toml` and `version` in
   `frontend/tauri.conf.json` to the same stable `MAJOR.MINOR.PATCH`, then refresh
   `Cargo.lock` with `cargo check -p thiscord-shared`. Let Cargo update the three
   workspace package entries; do not use global version replacement in
   `Cargo.lock`, because dependencies can have the same version as Thiscord.
   Run `./scripts/release-client.ps1 -Mode Validate -Tag client-vMAJOR.MINOR.PATCH`
   with the actual version to verify the complete locked dependency graph before
   tagging, including optional and platform-specific dependencies.
2. Commit and push the changes. Tag that commit and push the tag, for example:

   ```powershell
   git tag client-v0.1.0
   git push origin client-v0.1.0
   ```

   Use the actual version; published tags/releases are immutable. A subsequent
   version must be higher than every previously published stable client version.
3. Watch [Client release](../.github/workflows/release-client.yml). It validates
   versions/secrets, runs the existing format, Clippy, database, shared, WASM and
   desktop/audio checks, and builds three installer targets concurrently with the
   remaining checks once the WASM assets are ready. Publication waits for all
   checks and all installers to succeed. Production WASM assets
   compile with `THISCORD_API_URL=https://thiscord.com.tr`.
4. The final job downloads every platform artifact and independently verifies
   each updater signature against the embedded public key and signed version.
   It creates a draft release, uploads all installers, signatures, SHA256SUMS and
   the complete three-platform `latest.json`, then publishes and marks it latest.
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
signing configuration and Apple account setup remain deployment work. The existing
workflow passes these Apple secrets to Tauri; adding them enables Developer ID
signing and notarization on the macOS runner. Updater keys and HTTPS certificates
cannot substitute for platform code-signing credentials.

### Enroll and configure macOS signing

1. Enroll in the [Apple Developer Program](https://developer.apple.com/programs/enroll/).
   Apple lists USD 99 per membership year, with regional pricing. An individual
   membership can distribute Developer ID applications outside the App Store.
2. On a Mac, generate a certificate signing request in Keychain Access and use it
   to create a **Developer ID Application** certificate in the developer account.
   Install the certificate into the keychain containing the corresponding private
   key. Follow [Apple's certificate instructions](https://developer.apple.com/help/account/certificates/create-developer-id-certificates).
3. Export the certificate with its private key as a password-protected `.p12`.
   Encode it into a local file, outside the repository:

   ```sh
   openssl base64 -A -in /path/to/developer-id.p12 -out /path/to/certificate-base64.txt
   security find-identity -v -p codesigning
   ```

4. Put the encoded file's contents in the repository's `APPLE_CERTIFICATE` Actions
   secret and its export password in `APPLE_CERTIFICATE_PASSWORD`. Set
   `APPLE_SIGNING_IDENTITY` to the full `Developer ID Application: ... (TEAMID)`
   identity reported by `security`. Use GitHub secrets, not backend `.env`.
5. Add `APPLE_ID`, an **app-specific** password as `APPLE_PASSWORD`, and
   `APPLE_TEAM_ID`. These authenticate notarization; do not use the Apple account's
   normal password. See [Tauri's macOS signing guide](https://v2.tauri.app/distribute/sign/macos/).
6. Publish a new version through the release workflow and verify the downloaded
   app's signature, notarization ticket and first launch on a separate Mac.
   Until credentials are configured, CI continues to produce ad-hoc signed builds.

### Choose Windows signing before configuring CI

Windows needs a publicly trusted Authenticode certificate/signing service issued
to the publisher. Choose a provider that supports the publisher's country and
individual/company status, plus unattended GitHub Actions signing. Modern public
code-signing keys require protected storage; do not assume an ordinary exportable
PFX can be bought and uploaded to CI. See
[Tauri's Windows signing guide](https://v2.tauri.app/distribute/sign/windows/).

One option is Microsoft Azure Artifact Signing (formerly Trusted Signing), subject
to [Microsoft's current eligibility requirements](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart).
At the time of this setup review, public-trust individual enrollment is limited
to the US and Canada; organizations have a broader supported-country list including
Switzerland. A Swiss individual does not qualify through that organization list. Private
Trust is not a substitute for public trust on friends' unmanaged computers.
Otherwise, select a certificate authority's cloud/HSM signing service that accepts
the publisher's location and status. For example, [SSL.com eSigner](https://www.ssl.com/faqs/esigner-faq/)
supports individual code signing and cloud keys; confirm Swiss individual
eligibility and unattended CI access with the issuer before purchasing.
Provider selection and identity validation
are still pending; no Windows signing provider is configured in this repository.

Once selected, integrate its signer using Tauri's `bundle.windows.signCommand` so
both the application and installer are signed during packaging, before updater
signatures and release checksums are produced. Verify Authenticode on the final
downloaded installer and installed executable. Code signing identifies the
publisher but does not guarantee immediate Windows SmartScreen reputation.

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
