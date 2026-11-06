# Desktop releases and updates

The native client checks the public GitHub update feed once at launch and when
a newer release is announced through its server notification subscription. An
available update opens a large prompt with **Download and install** and **Not now**
(dismissed until the next launch); announcements also show a customizable banner.
Nothing downloads or installs until the user chooses to install.
**Settings → App updates** also provides a manual check. Installation is blocked
while connected to voice; joining voice is blocked during installation/download.
The login screen has an Updates button too. Development builds disable updates.
Every one of these update screens shows the new version's [changelog](#changelog):
its highlights first, and every change behind **Show all changes**.

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

4. Add the macOS code-signing certificate described in
   [Stable macOS signing](#stable-macos-signing) as `MACOS_SIGNING_CERTIFICATE`
   and `MACOS_SIGNING_CERTIFICATE_PASSWORD`. Releases fail without it.

If setting up another checkout, obtain the existing signing key from your backup.
Do not generate a different key: already installed clients trust the public key
embedded in their binary. Key rotation needs an update signed with the old key
that distributes the new trust configuration first.

## Runner selection

On each self-hosted runner, audio checks/benchmarks, desktop checks and native
Windows/macOS installers share the `native` Cargo target directory. Benchmarks
use an explicit host target to match Tauri's release artifact layout. Cargo reuses
compatible dependency artifacts; different features/profiles and final executable
linking can still require compilation. One runner service executes jobs serially;
separate runner names, repositories and PRs use separate directories. Hosted
caches remain keyed by job. Ubuntu-container installers have a separate cache
group so they never reuse Kali-built system-library artifacts. The first run of
a new cache group starts cold; subsequent stages/releases populate and reuse it.

Every job uses fixed self-hosted labels: Windows x64, macOS ARM64 or Linux x64.
There is no availability query or GitHub-hosted fallback. Offline/busy runners
leave jobs queued until an eligible runner is available. `RUNNER_STATUS_TOKEN`
is no longer used and may be removed from repository secrets.

CI starts only for pull requests, also on self-hosted runners. Main pushes do not
start checks. All changes, including version bumps, go through PRs before release.
Publishing builds web assets and installers and verifies their signatures; it
does not repeat formatting, Clippy, regression tests, benchmarks or backend checks.

Linux installers run on the self-hosted Linux x64 runner, including
Kali. They always build inside an Ubuntu 24.04 Docker image so released binaries
preserve the Ubuntu system-library baseline. No distribution-specific runner label
is required. The runner account needs a working Linux Docker daemon without sudo.
The image contains Rust, PowerShell, Tauri CLI and packaging dependencies; image
layers and a separate Ubuntu build cache remain on the host between releases.
The initial image build is slower; subsequent releases reuse it. Containers run
with the runner's UID/GID so generated files remain owned by the runner, and
AppImage extraction works without privileged containers or `/dev/fuse` access.
Compilation defaults to two jobs to fit memory-limited Docker/WSL instances;
set `CARGO_BUILD_JOBS` in the installer job if the daemon has more RAM available.
The image is defined in `infra/ci/linux-installer.Dockerfile`; packaging and
signature verification run through `scripts/build-linux-installer.sh`.

Self-hosted machines must already have Rust/rustup, PowerShell 7, Git and the
platform build prerequisites available to the runner account. Linux also needs
GitHub CLI (`gh`) for release publication, Python 3, passwordless package
installation and a running Docker daemon accessible
to that account for the PostgreSQL service container. CI uses a dynamically
assigned database port to avoid the locally hosted development PostgreSQL port.

## Changelog

Every client version has a reviewed `changelog/MAJOR.MINOR.PATCH.md`, written in the
release PR together with the version bump. Publishing puts it at the top of the
GitHub release and into the `notes` of `latest.json`, which the updater hands to the
update prompt, **Settings → App updates** and the login screen's Updates dialog.
Release validation fails without it, and `cargo test -p thiscord-frontend --lib`
(`changelog::tests`) checks the file for the version being built and all others.

To write one, read every PR merged since the previous `client-v` tag
(`git log --merges --first-parent client-vPREVIOUS..HEAD` and the PR descriptions)
and summarize what users will notice. Write for people using Thiscord, not for
developers: plain sentences, no internal module names, no proof or CI notes.
Leave out changes nobody can notice (refactors, CI, docs) unless they matter to users.

```markdown
## Highlights

- **Short title.** One or two sentences on what changed and why it helps.
- **Up to four of these.** The biggest, most visible changes come first.

## New

- One line per new feature or setting.

## Improved

- Changes to existing behavior, speed or looks.

## Fixed

- Bugs that no longer happen, described by their symptom.
```

`## Highlights` is required and holds 1–4 bullets that each start with a bold
`**Title.**` and a sentence. `## New`, `## Improved` and `## Fixed` are optional;
omit empty ones and use no other headings. Bullets are single paragraphs; `**bold**`
and `` `code` `` are the only formatting shown, and links show only their label.
Keep the file under 8 KiB. Clients treat the notes as untrusted text: they parse
this subset into text spans (`frontend/src/changelog.rs`) and never render HTML.
The notes describe only the version being installed, so users who skipped versions
see the newest changelog; earlier ones stay on their GitHub releases.

## Publish a version

Installer jobs clear generated bundle archives/signatures before packaging while
retaining compiled dependencies. Persistent caches may otherwise contain multiple
versioned `.deb`, `.dmg`, or `.exe` files, causing strict collection to fail. Do not
weaken collection to choose an arbitrary file; updater signatures and versions
must still be verified.

1. Update `[workspace.package].version` in root `Cargo.toml` and `version` in
   `frontend/tauri.conf.json` to the same stable `MAJOR.MINOR.PATCH`, then refresh
   `Cargo.lock` with `cargo check -p thiscord-shared`. Let Cargo update the three
   workspace package entries; do not use global version replacement in
   `Cargo.lock`, because dependencies can have the same version as Thiscord.
   Run `./scripts/release-client.ps1 -Mode Validate -Tag client-vMAJOR.MINOR.PATCH`
   with the actual version to verify the complete locked dependency graph before
   tagging, including optional and platform-specific dependencies. Add
   `changelog/MAJOR.MINOR.PATCH.md` as described in [Changelog](#changelog).
2. Commit the changes on a branch, open a PR, and merge it after CI passes.
   Tag the merged main commit and push the tag:

   ```powershell
   git switch main
   git pull --ff-only
   git tag client-vMAJOR.MINOR.PATCH
   git push origin client-vMAJOR.MINOR.PATCH
   ```

   Use the actual version; published tags/releases are immutable. A subsequent
   version must be higher than every previously published stable client version.
   Main pushes do not trigger CI; regression checks belong to the PR.
3. Watch [Client release](../.github/workflows/release-client.yml). It validates
   versions/secrets and builds three installer targets concurrently once the WASM
   assets are ready. Publication waits for all installers to succeed. The release
   workflow does not call PR CI or repeat regression checks. Production WASM assets
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

Windows installers include `frontend/windows/installer-hooks.nsh` through Tauri's
[`installerHooks`](https://v2.tauri.app/distribute/windows-installer/#extending-the-installer)
configuration. After a successful installation, including a passive `/UPDATE`,
the hook calls
[`SHChangeNotify`](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shchangenotify)
with `SHCNE_ASSOCCHANGED` and `SHCNF_FLUSH` to invalidate cached Shell icons.
This lets existing desktop, Start menu and pinned shortcuts reload the updated
executable's icon even though its path has not changed. Shortcut preferences and
properties are preserved; the hook does not recreate deleted shortcuts, delete
Windows cache files or restart Explorer. The same signed installer carries this
behavior to every Windows user who installs the release containing it.

When changing branding, verify an upgrade from the old icon in a disposable
Windows profile or VM, using both the interactive installer and passive updater.
Check existing desktop/Start menu/pinned icons and confirm that a deliberately
deleted desktop shortcut stays absent during an in-app update. Building the
installer alone does not verify Explorer's visible refresh behavior.

An older executable without updater support must be replaced once by installing
the first updater-enabled release manually. Keep Linux AppImages in a user-writable
location. Debian packages require manual/package-manager updates; no apt repository
is configured. Linux builds use Ubuntu 24.04 and require compatible glibc/system
libraries; an AppImage is not a promise of compatibility with every distribution.

Updater signatures authenticate in-app updates. They do **not** replace Windows
Authenticode or Apple Developer ID/notarization for the first download. Windows
installers currently have no Authenticode certificate and may show SmartScreen
warnings. macOS builds use a stable self-signed certificate, so Gatekeeper still
blocks the first downloaded `.dmg` until the user opens it explicitly; in-app
updates are not quarantined.

The macOS entitlement enables microphone and camera access under the hardened
runtime; `Info.plist` supplies the usage prompts. Certificate purchase, Windows
signing configuration and Apple account setup remain deployment work. Updater keys
and HTTPS certificates cannot substitute for platform code-signing credentials.

### Stable macOS signing

macOS remembers Screen Recording, microphone and camera grants (TCC) and Keychain
item access by the app's *designated requirement*. An ad-hoc signature's
requirement is its code hash, which changes every build: each update then looked
like a new app, lost Screen Recording and asked for the login password before
reading the stored session. Releases up to 0.1.54 were ad-hoc signed.

Releases are now signed with the self-signed **Thiscord Release Signing**
certificate (RSA-3072, code-signing EKU, valid until 2046). Its requirement is
`identifier "tr.com.thiscord.desktop" and certificate root = H"2f0c37cc…"`, the
same for every build signed with it. The installer job imports
`MACOS_SIGNING_CERTIFICATE` (base64 `.p12`) with `scripts/macos-signing.ps1` into
a temporary keychain, checks its SHA-1 against `MACOS_SIGNING_SHA1` in
`build-installers.yml`, signs through Tauri's `APPLE_SIGNING_IDENTITY`, verifies
that requirement on the built `.app` and removes the keychain. `security` lists
the identity as untrusted (`CSSMERR_TP_NOT_TRUSTED`); codesign, TCC and Keychain
matching do not need it to be trusted. The binary links only system libraries,
so hardened-runtime library validation holds without a Team ID.

The certificate, its `.p12` password and PEM are outside Git in
`C:\Users\bugra\.thiscord\signing\macos-signing.*`, next to the updater key.
**Keep using this certificate.** A different certificate, or switching to
Developer ID, changes the requirement: every macOS user must re-grant permissions
and approve one Keychain read once more. Update `MACOS_SIGNING_SHA1` and this
section deliberately if that is ever intended.

Moving from an ad-hoc release to the first signed one is such a change. Existing
users must, once, remove the old Thiscord entry under **System Settings → Privacy
& Security → Screen & System Audio Recording** with **−** and add the app again,
and approve one Keychain prompt. The client deletes and re-adds its Keychain item
after that approved read (deleting and adding need no approval), so the item
then trusts the signed requirement and later updates read it silently; see
[accounts](accounts.md#session-model-and-desktop-storage).

To recreate an equivalent certificate (only for a deliberate rotation), use
LibreSSL, or OpenSSL 3 with `-legacy` on the export, for a `.p12` that
`security import` accepts:

```sh
openssl req -x509 -newkey rsa:3072 -sha256 -days 7300 -nodes \
  -keyout key.pem -out macos-signing.cer.pem -subj "/CN=Thiscord Release Signing/O=Thiscord" \
  -addext "basicConstraints=critical,CA:false" -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning"
openssl pkcs12 -export -inkey key.pem -in macos-signing.cer.pem -out macos-signing.p12 \
  -name "Thiscord Release Signing" -passout file:macos-signing-password.txt
openssl x509 -in macos-signing.cer.pem -noout -fingerprint -sha1
```

Delete `key.pem` afterwards. Store `base64` of the `.p12` and the password file's
contents as the two repository secrets.

### Developer ID (not configured)

A [Developer ID](https://developer.apple.com/programs/enroll/) certificate
(Apple Developer Program, USD 99 per year) plus notarization would also remove
the Gatekeeper warning on first download. It needs `APPLE_CERTIFICATE`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, an app-specific
`APPLE_PASSWORD` and `APPLE_TEAM_ID` passed to Tauri, a requirement check for the
Apple-anchored requirement instead of the pinned certificate, and one more
permission reset for existing users. See [Tauri's macOS signing guide](https://v2.tauri.app/distribute/sign/macos/).

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
