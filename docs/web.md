# Browser web client (server side)

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
