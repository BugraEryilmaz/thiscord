# Browser web client

The backend serves the Leptos web client at `/app` (also `/app/`) on the same
origin as the API, for desktop and mobile Chrome, Safari and Firefox. This page
covers the backend's session model, hosting and deployment. Client behavior and
browser voice limitations are documented with the frontend; server-side voice
handling is in [audio.md](audio.md#browser-voice).

## Cookie sessions

The web client never sees its session token in JavaScript. Desktop/Tauri, bots
and `/admin` keep using `Authorization: Bearer` headers unchanged.

- **Issuing.** `POST /api/v1/account` with header `x-thiscord-session: cookie`
  (`SESSION_MODE_HEADER`/`SESSION_MODE_COOKIE` in `shared/src/account.rs`) from an
  `Origin` listed in `ALLOWED_ORIGINS` turns every `session` response (register,
  login, Google completion, rotation) into
  `Set-Cookie: __Host-thiscord_session=<token>; Max-Age=<seconds until the session's
  absolute expiry>; Path=/; HttpOnly; Secure; SameSite=Strict`, and returns the
  session with an empty `token`. Rotating a cookie-authenticated session never
  returns a readable token, even without the header. Otherwise, without the
  header or from any other origin, the response is the ordinary bearer response
  and no cookie is set; a same-origin bearer client (the admin dashboard) signing
  in while a web cookie exists therefore still receives its own body token.
- **Authenticating.** An outer API middleware runs before every handler. When a
  request has no `Authorization` header and exactly one well-formed (43-character)
  session cookie, it inserts `Authorization: Bearer <cookie>` only if the CSRF rule
  passes; otherwise the cookie is ignored and the request is anonymous. All
  existing session checks, locks, replay revocation and rate limits then apply.
  An explicit `Authorization` header always takes precedence.
- **CSRF rule.** Either the `Origin` header is present and in `ALLOWED_ORIGINS`,
  or there is no `Origin` and the method is `GET`/`HEAD`. Browsers send `Origin`
  on all cross-origin requests and on same-origin non-GET/HEAD requests, so
  forged cross-site mutations carry a foreign (or `null`) origin and are
  anonymous. `SameSite=Strict` is a second, independent layer. CORS is unchanged
  and does not allow credentials; the web client is same-origin and needs none.
- **Clearing.** For a cookie-authenticated request, `logout` always returns a
  clearing cookie (`Max-Age=0`, same attributes). `logout_all`, `delete_account`,
  `change_password` and `unlink_identity` clear it on success; `current`,
  `sessions` and `rotate` clear it on HTTP 401. Revoking the current device via
  `revoke_session` takes effect server-side immediately; the next `current` check
  returns 401 and clears the cookie. A wrong reauthentication password never clears it.
- **Sockets.** Chat, friends, notifications and voice sockets authenticate with a
  token in their first frame. The web client sends an **empty** token; the server
  then uses the bearer derived from the cookie at upgrade time. A non-empty frame
  token always wins. Upgrades still require an `Origin` in `ALLOWED_ORIGINS`, so
  a disallowed origin is refused before any cookie is considered.
- **Rotation and voice.** A voice connection retains the token accepted at join.
  Rotating the cookie session (`rotate`) closes that voice connection, as for desktop.
  The web client should not rotate during calls.

Threat boundary: the HttpOnly cookie is not readable by page scripts, so a
script injection cannot exfiltrate the token, although it can still act as the
user while the page is open (as with any web session). Only exact allowed
origins can obtain or use the cookie for mutations. `__Host-` forbids `Domain`
and requires `Secure` and `Path=/`, so subdomains cannot set or read it. Never
log cookie or token values. Keep `ALLOWED_ORIGINS` to trusted, exact origins:
any listed origin can use a user's cookie.

## Google attempt cookie

A Google sign-in started without a desktop loopback callback (the web client,
`/admin`, the browser preview) is bound to the browser that started it
([accounts.md](accounts.md#attempt-binding)). `google_start` responds with
`Set-Cookie: __Host-thiscord_google=<random 256-bit secret>; Max-Age=300; Path=/;
HttpOnly; Secure; SameSite=Lax` (`cookie::issue_google_attempt`) and stores only
the secret's hash on the attempt. The Google callback must present exactly one such
cookie matching the attempt before it is processed; otherwise it answers `403`
with a plain page saying the sign-in was not started in this browser and fails the
attempt. A matching callback clears the cookie (`Max-Age=0`, same attributes).

- **Why `Lax`.** Google returns to the callback with a cross-site top-level `GET`
  navigation from `accounts.google.com`. Browsers withhold `SameSite=Strict`
  cookies on such navigations (which is also why the session cookie never reaches
  the callback) but send `Lax` ones. The cookie authorizes nothing by itself: it
  only marks this browser as the one that started the attempt, and it is useless
  without the ticket held by the page that started it.
- **Scope.** It is set for any caller of `google_start` without a callback, not
  only allowed origins, because it lands only in the caller's own browser; a
  cross-origin page cannot read the ticket. Five minutes matches the attempt's
  expiry. One cookie name serves one attempt at a time per browser: starting a
  second Google sign-in replaces it, so the first one's callback is refused.
- It is separate from `__Host-thiscord_session`: the session cookie rules (allowed
  origins, `SameSite=Strict`, never readable by JavaScript) are unchanged, and the
  cookie middleware never treats the attempt cookie as a credential.
- The callback page is same-host with the API (`GOOGLE_REDIRECT_URL`), so the
  browser must have started the sign-in on that host. Cookies ignore ports, but
  `localhost` and `127.0.0.1` are different hosts.

## Hosting `/app`

Build from `frontend/` with pinned Trunk 0.21.14 (same `index.html` as desktop
and admin):

```sh
trunk build --release --locked --public-url /app/ --dist dist-app
```

Install `frontend/dist-app` contents into `app` beside the backend executable, or
set `APP_DIST_DIR` to the bundle's absolute directory (a Linux path in WSL). An
explicitly configured missing/invalid directory fails startup; without
configuration or an adjacent bundle the backend stays API-only. Keep bundle files
immutable for the process lifetime and restart after replacement: the CSP hashes
Trunk's inline bootstrap from `index.html` at startup.

Routing: existing files under the bundle are served with their content types
(`application/wasm`, `text/javascript`, `application/manifest+json`, images).
Unknown paths without a file extension (for example `/app/invite/<code>`) return
`index.html` so client-side deep links work; missing asset-like paths return 404.
API routes keep their JSON error envelope.

Response headers on `/app`:

- `Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'
  <bootstrap hashes>; style-src 'self'; img-src 'self' blob: data:; media-src 'self'
  blob:; connect-src 'self' <wss origin>; object-src 'none'; frame-ancestors 'none';
  base-uri 'none'; form-action 'self'`. The `wss`/`ws` origin derives from
  `PUBLIC_BACKEND_URL` (default `http://localhost:3000`), because older WebKit does
  not match WebSockets with `'self'`. This is the desktop CSP minus Tauri IPC,
  asset and localhost entries. No third-party scripts, styles, fonts or frames are
  allowed; inline `style` attributes are blocked exactly as on desktop. AudioWorklet
  or worker modules must be bundle files (`'self'`), not `blob:` URLs.
- `Cache-Control: no-store` for HTML (it names the current hashed assets),
  `no-cache` (revalidate) for other files.
- `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
  `Cross-Origin-Opener-Policy: same-origin`, and
  `Permissions-Policy: microphone=(self), camera=(self), display-capture=(self)`.

Production requires HTTPS (`Secure` cookies) and `https://thiscord.com.tr` in
`ALLOWED_ORIGINS`. `infra/vps/deploy-backend.sh` builds and installs `app` next to
`admin` and `welcome` in each versioned release, with the same build-only,
backup, activation and rollback behavior, and checks `/app/` after activation.
CI uploads the bundle as the `web-app-assets` artifact.

## Local development

Run the backend in WSL as usual. For the hosted bundle, build `dist-app` and set
`APP_DIST_DIR`, then open `http://localhost:3000/app/`. For iterative UI work the
frontend uses `trunk serve` with a proxy to the backend so that the page and API
share one origin (configured in `frontend/Trunk.toml`). The proxy forwards the
browser's `Origin`, so `ALLOWED_ORIGINS` must include the dev origin
`http://127.0.0.1:1420` (the example `.env` already does).

`Secure` cookies are accepted on `http://localhost` and `http://127.0.0.1` by
Chrome and Firefox, which treat loopback as a secure context. Safari does not
store them over plain HTTP; test Safari against an HTTPS origin.

## Tests

`backend/tests/web_sessions.rs` (PostgreSQL) covers cookie issuance with an empty
body token, no cookie without the header or from foreign/missing origins, cookie
authentication from allowed origins only, safe GET without `Origin`, bearer
precedence, cookie-only rotation, logout/logout-all/401 clearing, and socket
authentication with an empty frame token (including frame-token precedence and
refusal of disallowed origins). `backend/tests/web_app.rs` covers static routing,
SPA fallback, content types and headers. Real-browser acceptance on each
platform still needs manual checks.

## Web client

The browser client is the same `thiscord-ui` Leptos/WASM bundle as the desktop
app, built from `frontend/index.html` and served at `/app/` (see the server
section for hosting, cookies and CSP). `account_client::web_app()` is true for
browser pages other than `/admin`; such pages call the same-origin API
(`api_base()` is empty) and derive socket URLs from the page origin.

### Sessions

- Every `/api/v1/account` request sends `x-thiscord-session: cookie`. Session
  responses carry an empty token; the server sets an HttpOnly cookie that the page
  cannot read. The signed-in marker is `ui.token == Some("")`.
- `account_client::bearer` drops empty tokens, so HTTP requests, attachment XHRs
  and `session_rejected` send no `Authorization` header and the browser attaches
  the cookie. Socket hello frames send the empty token. `/admin` keeps its
  in-memory bearer token and never sends the session-mode header.
- Startup calls `Current` once (`restore_cookie_session`) behind a loading screen:
  success opens the app, 401 shows the login form silently. The web client never
  calls `Rotate`. `persist` is a no-op; sign-out and revocation still clear local
  state, and the server clears the cookie.
- Google sign-in starts as in the old browser preview: the page polls the ticket
  and tries `window.open(url, "_blank", "noopener,noreferrer")`, keeping a visible
  **Continue in Google** link for popup blockers and mobile Safari. The new tab
  shares this browser's cookies, so it carries the
  [Google attempt cookie](#google-attempt-cookie) back to the callback. Finishing
  the Google step in another browser (or a home-screen web app whose links open in
  a separate browser, as on iOS) is refused.
- The device name is **Thiscord web**.
- Keep every request same-origin: the server only honors the cookie with an
  allowed `Origin`.

### Desktop-only features

`account_client::native` and `listen` fail closed (“Available in the desktop app”)
when `__TAURI__` is absent, so no web code path can call into Tauri. Browser voice,
device tests, microphone boost, per-speaker volumes above 100%, the soundboard and
per-person soundboard volume, connection diagnostics and focused-page keyboard
shortcuts (Settings → Keyboard shortcuts) run in the browser; see
[browser voice](audio.md#browser-voice) and [soundboard](soundboard.md#web-client).
Voice messages record with MediaRecorder and play through a media element on the
selected output where `setSinkId` exists ([chat.md](chat.md#voice-messages)).
Browser system notifications are optional and requested only from Settings →
Notifications; see [notifications](notifications.md#browser-system-notifications).
Cameras (getUserMedia) and screen sharing (getDisplayMedia, with tab or system audio
in Chrome and Edge) also run in the browser, and the call grid plays received
screens and cameras; see
[browser screen sharing](screen-sharing.md#browser-screen-sharing). Phones and
tablets have no `getDisplayMedia`: their Share screen control explains “Screen
sharing isn't supported in this browser”.

What remains desktop-only is impossible or unavailable in a browser page: global
shortcuts while another app or tab has focus (pages receive keys only while
focused), the in-game overlay (an always-on-top window over other apps),
close-to-tray (no tray icon; closing a tab ends the page), the signed auto-updater
(the server always serves the current web bundle), native neural noise-suppression
models and neural echo, packet-loss options (RED/deep redundancy/adaptive FEC need
the native Opus encoder, which browsers do not expose), echo debug recordings and
diagnostic logs (raw device PCM), and remote admin audio control (it changes native
preferences on the user's computer). The web client hides those settings and shows
**Get the desktop app** (linking to `/#downloads`) in Settings and the Updates
panel. Links open in a new tab with `noopener noreferrer`; attachments download
through a blob link.

### Invitations

`/app/#invite=CODE` (from the browser invite page) fills the same pending slot as
the desktop deep link. The code waits in memory through sign-in and email
verification; when **Join a server** opens with it, `history.replaceState` removes
the fragment. `frontend/src/invites.rs` validates the fragment and tests the round
trip.

### Phones and tablets

Below 768 px the shell shows either navigation (server rail, channel or friends
list, account dock) or the open channel/conversation. Groups are listed above
friends under **Messages**, and **New group** sits beside **Add friend**; a group's
member panel opens as a drawer from the header. Selecting a channel or
direct message switches to content; the back button in the chat header (or above
a voice channel) returns to the list without closing the channel. `ui.mobile_content`
holds that choice; content is shown only while something is open. Settings become
full-screen with a scrolling section bar. From 768 px the desktop layout applies
(the existing 800 px rule narrows the sidebar).

- `index.html` uses `viewport-fit=cover` and `interactive-widget=resizes-content`;
  the shell pads `env(safe-area-inset-*)`. On narrow screens the shell height
  follows `visualViewport` (`--viewport-height`, set through CSSOM because the web
  CSP forbids inline styles) so the composer stays above the on-screen keyboard.
- Inputs are at least 16 px on phones/coarse pointers to avoid iOS zoom; coarse
  pointers get 44 px icon buttons, menu items and channel rows.
- A still ~500 ms touch (10 px slop) opens the same context menus as right-click
  (`frontend/src/touch.rs`, unit-tested); on other controls it focuses the
  control so its `data-tip` hint shows. The release click is swallowed and Android's
  native long-press `contextmenu` does not open a second menu. On touch screens a
  message's actions appear in a row under it once the message is tapped (the
  tap focuses it); until then they stay visually hidden but reachable by screen
  readers, and sticky `:hover` never reveals them.

### Web app manifest

`frontend/assets/web/manifest.webmanifest` (start URL and scope `/app/`,
standalone display) and the 512 px `icons/icon.png` are copied into the bundle
root. The relative manifest link is harmless in the desktop app. There is no
service worker or offline support.

### Local development

`frontend/Trunk.toml` proxies `/api/` and the chat, friends, notifications and
voice WebSockets from `trunk serve` (`127.0.0.1:1420`) to `localhost:3000`. Trunk
forwards `Origin` and `Cookie` on both HTTP and WebSocket requests, so cookie
sessions work as in production. The backend's `ALLOWED_ORIGINS` must include
`http://127.0.0.1:1420`. Chromium treats `http://127.0.0.1` as secure and stores
the `Secure` session cookie there; Safari does not, so test Safari (and confirm
other browsers) against an HTTPS deployment.
Google sign-in from `trunk serve` needs the callback on the same host as the page:
the preview runs on `127.0.0.1`, so set `GOOGLE_REDIRECT_URL` to
`http://127.0.0.1:3000/api/v1/account/google/callback` (and register it with
Google). With the example `localhost` URL the browser does not send the attempt
cookie to the callback, which refuses the sign-in as started in another browser.
The hosted bundle at `http://localhost:3000/app/` works with the `localhost` URL.
`cargo tauri dev` also runs `trunk serve`, but the desktop app ignores the proxy
and keeps using `THISCORD_API_URL`. Build the web bundle like production with
`trunk build --release --locked --public-url /app/ --dist dist-app` from `frontend/`.
