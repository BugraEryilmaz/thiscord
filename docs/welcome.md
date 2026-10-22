# Public welcome page

The public landing page introduces Thiscord and offers the latest desktop
installers. It uses the existing `thiscord-ui` Leptos/WASM binary with a `welcome`
feature selected by `frontend/welcome/index.html`, excluding the chat client
modules from this build. The desktop and admin
entry point remains `frontend/index.html`; no additional package or JavaScript
application logic is introduced. The page currently uses English copy.

## Build and preview

From `frontend/`:

```powershell
# If the host sets NO_COLOR=1, use the boolean value expected by Trunk.
$env:NO_COLOR = 'true'
trunk serve welcome/index.html --port 1430 --dist dist-welcome
```

Open `http://127.0.0.1:1430`. For the backend's production paths:

```sh
trunk build welcome/index.html --release --locked --public-url /welcome/ --dist dist-welcome
```

The backend serves the HTML at `/` and the bundle at `/welcome/`. Set
`WELCOME_DIST_DIR` to the absolute build directory during local backend testing,
or place the bundle in `welcome/` beside the executable. An explicitly configured
missing/invalid directory fails startup; without either directory the backend
continues to run API-only. No database is needed to serve the page.

`infra/vps/deploy-backend.sh` builds and installs the page alongside the admin
bundle before activating a release. Rollback restores the prior executable and
its matching bundles. CI builds and uploads `welcome-assets`. These repository
changes do not themselves deploy the site or configure DNS.

Other static hosts can serve the bundle too; use their matching `--public-url`.
Allow same-origin WASM, styles, fonts and generated JavaScript, plus connections
to `https://api.github.com`. The built-in server hashes Trunk's inline bootstrap
for CSP and does not allow arbitrary inline scripts/styles. API routes, including
their JSON 404 responses, and `/admin` are independent of the welcome page.

## Downloads

On each page load, the Rust frontend requests the public
[`releases/latest` endpoint](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)
for `BugraEryilmaz/thiscord`, without credentials. It accepts published stable
`client-vMAJOR.MINOR.PATCH` releases and matches the exact installer names used
by `scripts/release-client.ps1`. Download URLs must belong to that repository,
tag and filename. Signatures and updater-only archives are not offered as installers.

The main button suggests a platform using the user agent. macOS is explicitly
labeled Apple Silicon because browser user agents cannot reliably identify the
chip. Mobile/unknown platforms show the platform list. Windows and Linux builds
are x64; all download choices remain available regardless of the suggestion.

Loading lasts at most ten seconds. Failed requests, rate limits and invalid
metadata show retry and GitHub release links. A missing installer is labeled
“Not in this release”; the page never fabricates its download URL. The latest
release page remains accessible without JavaScript through the HTML fallback.
Publishing a new stable client release updates downloads without rebuilding the
website. Keep GitHub's Latest marker reserved for stable client releases.

The room preview is illustrative, with local room and mute controls; it does not
connect to voice, request microphone permissions or represent live presence.
The Bricolage Grotesque font is hosted locally under its accompanying OFL license.
