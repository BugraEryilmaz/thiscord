# Web client

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
  **Continue in Google** link for popup blockers and mobile Safari.
- The device name is **Thiscord web**.
- Keep every request same-origin: the server only honors the cookie with an
  allowed `Origin`.

### Desktop-only features

`account_client::native` and `listen` fail closed (“Available in the desktop app”)
when `__TAURI__` is absent, so no web code path can call into Tauri. The web
client hides shortcuts/overlay and close-to-tray settings and the updater, and
shows **Get the desktop app** (linking to `/#downloads`) in Settings and the
Updates panel. Screen sharing and camera buttons are disabled with an “Available in
the desktop app” hint. In-app banners and sounds remain; native notification
windows are desktop-only, and browser `Notification` permission is not requested.
Links open in a new tab with `noopener noreferrer`; attachments download through a
blob link. Voice in browsers is documented separately.

### Invitations

`/app/#invite=CODE` (from the browser invite page) fills the same pending slot as
the desktop deep link. The code waits in memory through sign-in and email
verification; when **Join a server** opens with it, `history.replaceState` removes
the fragment. `frontend/src/invites.rs` validates the fragment and tests the round
trip.

### Phones and tablets

Below 768 px the shell shows either navigation (server rail, channel or friends
list, account dock) or the open channel/conversation. Selecting a channel or
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
  native long-press `contextmenu` does not open a second menu. Message actions are
  always visible on touch screens.

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
`cargo tauri dev` also runs `trunk serve`, but the desktop app ignores the proxy
and keeps using `THISCORD_API_URL`. Build the web bundle like production with
`trunk build --release --locked --public-url /app/ --dist dist-app` from `frontend/`.
